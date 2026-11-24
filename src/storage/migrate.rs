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

// libc stat field widths differ across targets, so these casts are portable.
#![allow(clippy::unnecessary_cast, clippy::useless_conversion)]

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::Utc;
use redb::{
    Builder as RedbBuilder, ConcurrencyMode, Database as RedbDatabase, WriteTransaction,
    backends::FileBackend,
};
use sea_orm::sqlx::{
    AssertSqlSafe, Connection as _, Executor as _, Row as _, SqliteConnection,
    sqlite::SqliteConnectOptions,
};
use serde::{Deserialize, Serialize};

use crate::{
    internal::db::schema::{CAPTURE_SCHEMA_NAME, CAPTURE_SCHEMA_VERSION, DatabaseRole},
    storage::{
        capture_store::{
            AgentAuditLogRecord, AgentBridgeCheckpointRecord, AgentBridgeEventRecord,
            AgentBridgeLinkRecord, AgentBridgeOperationRecord, AgentBridgeSessionRecord,
            AgentCaptureCloudBaseRecord, AgentCaptureIncarnationRecord,
            AgentCheckpointPruneTombstoneRecord, AgentCheckpointRecord, AgentCoverageClaimRecord,
            AgentCoverageConflictRecord, AgentCoverageRevisionRecord, AgentExportJobRecord,
            AgentImportIdentityRecord, AgentImportTombstoneRecord, AgentSessionCommitRecord,
            AgentSessionRecord, AgentSubagentContentClaimRecord,
            AgentSubagentContentRevisionRecord, AgentSubagentLinkRecord, AgentUsageStatsRecord,
            AgentWorkspaceScopeAuditRecord, BridgeLinkSourceType, BridgeOperationStatus,
            BridgeSessionState, CAPTURE_SOURCE_TABLES, CAPTURE_SOURCE_TABLES_COUNT,
            CaptureAgentKind, CaptureCatalogCopyReceiptRecord, CaptureCheckpointScope,
            CaptureCompleteness, CaptureCoverageClaimState, CaptureExportJobState, CaptureHashKind,
            CaptureImportIdentityState, CaptureSchemaVersionRecord, CaptureScopeState,
            CaptureSessionState, CaptureSourceChannel, CaptureSubagentClaimState,
            CaptureSubagentLinkState, CaptureSubagentSourceChannel, invert_capture_timestamp,
            redb_capture_store as redb_cap,
        },
        codec::encode_versioned,
        config_store::LegacyConfigEntry,
        error::{StorageBackendError, StorageResult},
        git_state_store::{
            AiDecisionProposalRecord, AiFinalDecisionRecord, AiIndexIntentContextFrameRecord,
            AiIndexIntentPlanRecord, AiIndexIntentTaskRecord, AiIndexPlanStepTaskRecord,
            AiIndexRunEventRecord, AiIndexRunPatchsetRecord, AiIndexTaskRunRecord,
            AiLiveContextWindowRecord, AiOperationLinkRecord, AiRiskScoreBreakdownRecord,
            AiSchedulerPlanHeadRecord, AiSchedulerSelectedPlanRecord, AiSchedulerStateRecord,
            AiThreadIntentRecord, AiThreadParticipantRecord, AiThreadProviderMetadataRecord,
            AiThreadRecord, AiValidationReportRecord, ApprovedPermissionRecord,
            AutomationLogRecord, BisectStateRecord, ChangeIdentityRecord, ChangePredecessorRecord,
            ChangeRevisionRecord, LayerPathRecord, LayerRecord, MemoryEpisodeEvidenceRecord,
            MemoryEpisodePathRecord, MemoryEpisodeRecord, MemoryEpisodeSearchDocRecord,
            MemoryEvidenceKind, MemoryLinkConfidence, MemoryOutcome, MemoryPathChangeKind,
            MemoryProjectionStateRecord, MemoryResolutionStatus, MemorySourceKind,
            OperationJournalRecord, OperationRecord, OperationScopeKind, OperationScopeProvenance,
            REPOSITORY_ACTIVE_SOURCE_TABLES_COUNT, RebaseStateRecord, SequenceStateRecord,
            SourceCallLogRecord, WorkingDirtyEntryRecord, WorkingDirtyMetaRecord,
            WorkspaceRecordKind, WorkspaceRecordOwnerKind, WorkspaceRecordRow,
            WorkspaceRecordState, WorktreeIntentOp, WorktreeIntentRecord, WorktreeLifecycleRecord,
            WorktreeLifecycleState, invert_ended_at, redb as redb_state,
            repository_active_source_tables,
        },
        git_store::{
            BridgePublicationPhase, ObjectHashKind, ObliterationState, RefKey, RefTarget,
            redb as redb_git,
        },
        manifest::{
            LEGACY_SQLITE_DB_FILE, PRIVATE_SUBDIR, REDB_SUBDIR, canonical_capture_redb_rel_path,
            canonical_git_redb_rel_path,
        },
        meta::init_meta_schema,
        opener::{CACHE_SIZE_STANDARD_WRITE, begin_write_bounded_blocking},
        queue::{
            CAPTURE_QUEUE_SUBDIR, CAPTURE_SPOOL_CAPACITY_LOCK_FILE, CAPTURE_SPOOL_SUBDIR,
            CAPTURE_SPOOL_TOMBSTONE_SENTINEL, QUEUE_APPEND_LOCK_FILE, QUEUE_WRITER_LOCK_FILE,
        },
        redb::{
            config_store as redb_cfg, map_redb_commit_err, map_redb_database_err,
            map_redb_storage_err, map_redb_table_err,
        },
    },
};

/// Canonical advisory lock filename for repository storage migrations under `<common_storage>`.
pub const MIGRATION_LOCK_FILE: &str = "migration.lock";

/// Canonical advisory lock filename for the legacy capture spool consumer under
/// `<common_storage>/private/` (`src/internal/ai/capture/drain.rs:87`).
pub const CAPTURE_SPOOL_CONSUMER_LOCK_FILE: &str = "capture-spool-consumer.lock";

/// Maximum age before an abandoned `capture-spool/tmp/` file is considered
/// expired (`src/internal/ai/capture/spool.rs:28`, 24 hours). Any regular file
/// with age `<= CAPTURE_SPOOL_TMP_MAX_AGE` (or future mtime) is treated as an
/// active in-flight publisher between `spool.rs:119` and `spool.rs:145`.
pub const CAPTURE_SPOOL_TMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Subdirectory inside `<common_storage>/private/capture-spool/` holding
/// in-flight publisher staging files (`spool.rs:21`).
const CAPTURE_SPOOL_TMP_SUBDIR: &str = "tmp";

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

/// Return the canonical repository migration advisory lock path
/// `<common_storage>/migration.lock`.
#[must_use]
pub fn migration_lock_path(common_storage: &Path) -> PathBuf {
    common_storage.join(MIGRATION_LOCK_FILE)
}

/// Return the canonical legacy capture spool directory path
/// `<common_storage>/private/capture-spool`.
#[must_use]
pub fn capture_spool_dir_path(common_storage: &Path) -> PathBuf {
    common_storage
        .join(PRIVATE_SUBDIR)
        .join(CAPTURE_SPOOL_SUBDIR)
}

/// Return the canonical legacy capture spool staging directory path
/// `<common_storage>/private/capture-spool/tmp`.
#[must_use]
pub fn capture_spool_tmp_dir_path(common_storage: &Path) -> PathBuf {
    capture_spool_dir_path(common_storage).join(CAPTURE_SPOOL_TMP_SUBDIR)
}

/// Return the canonical in-place legacy capture spool capacity lock path
/// `<common_storage>/private/capture-spool/capacity.lock`.
#[must_use]
pub fn capture_spool_capacity_lock_path(common_storage: &Path) -> PathBuf {
    capture_spool_dir_path(common_storage).join(CAPTURE_SPOOL_CAPACITY_LOCK_FILE)
}

/// Return the canonical legacy capture spool consumer lock path
/// `<common_storage>/private/capture-spool-consumer.lock`.
#[must_use]
pub fn capture_spool_consumer_lock_path(common_storage: &Path) -> PathBuf {
    common_storage
        .join(PRIVATE_SUBDIR)
        .join(CAPTURE_SPOOL_CONSUMER_LOCK_FILE)
}

/// Return the canonical capture queue root directory path
/// `<common_storage>/private/capture-queue`.
#[must_use]
pub fn capture_queue_dir_path(common_storage: &Path) -> PathBuf {
    common_storage
        .join(PRIVATE_SUBDIR)
        .join(CAPTURE_QUEUE_SUBDIR)
}

/// Return the canonical in-place capture queue append lock path
/// `<common_storage>/private/capture-queue/append.lock`.
#[must_use]
pub fn capture_queue_append_lock_path(common_storage: &Path) -> PathBuf {
    capture_queue_dir_path(common_storage).join(QUEUE_APPEND_LOCK_FILE)
}

/// Return the canonical capture queue consumer writer lock path
/// `<common_storage>/private/capture-queue/writer.lock`.
#[must_use]
pub fn capture_queue_writer_lock_path(common_storage: &Path) -> PathBuf {
    capture_queue_dir_path(common_storage).join(QUEUE_WRITER_LOCK_FILE)
}

/// Sanitized stage identifier for full Phase 1 non-SQLite + SQLite forward
/// migration quiescence (`ADR-RB-07` #1, `RB-10n`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardPhase1QuiesceStage {
    /// Repository storage root validation (`<common_storage>`).
    StorageRoot,
    /// Exclusive advisory lock on `<common_storage>/migration.lock`.
    MigrationLock,
    /// Exclusive advisory lock on `<common_storage>/maintenance.lock`.
    MaintenanceLock,
    /// Exclusive advisory lock on `<common_storage>/private/capture-spool-consumer.lock`.
    CaptureSpoolConsumerLock,
    /// Exclusive advisory lock on `<common_storage>/private/capture-queue/writer.lock`.
    CaptureQueueWriterLock,
    /// In-place exclusive advisory lock on `<common_storage>/private/capture-spool/capacity.lock`.
    CaptureSpoolCapacityLock,
    /// In-place exclusive advisory lock on `<common_storage>/private/capture-queue/append.lock`.
    CaptureQueueAppendLock,
    /// Active unexpired staging file check under `<common_storage>/private/capture-spool/tmp/`.
    CaptureSpoolInFlightTmp,
    /// Single-connection SQLite Phase 1 quiescence on `libra.db` and optional `capture.db`.
    SqliteSources,
}

impl ForwardPhase1QuiesceStage {
    /// Human-readable sanitized stage label (zero private paths).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StorageRoot => "storage root validation",
            Self::MigrationLock => "migration.lock",
            Self::MaintenanceLock => "maintenance.lock",
            Self::CaptureSpoolConsumerLock => "capture-spool-consumer.lock",
            Self::CaptureQueueWriterLock => "capture-queue/writer.lock",
            Self::CaptureSpoolCapacityLock => "capture-spool/capacity.lock",
            Self::CaptureQueueAppendLock => "capture-queue/append.lock",
            Self::CaptureSpoolInFlightTmp => "capture-spool/tmp in-flight publisher check",
            Self::SqliteSources => "sqlite sources quiescence",
        }
    }
}

/// Exclusive non-SQLite advisory locks held across forward migration Phase 1
/// and Phase 2 (`ADR-RB-07` #1, `RB-10n`).
///
/// Fields are declared in **strict reverse acquisition order** so dropping the
/// struct always releases inner locks before outer locks:
/// `append.lock -> capacity.lock -> writer.lock -> capture-spool-consumer.lock -> maintenance.lock -> migration.lock`.
#[derive(Debug)]
pub struct ForwardPhase1Locks {
    _queue_append_lock: File,
    _spool_capacity_lock: File,
    _queue_writer_lock: File,
    _spool_consumer_lock: File,
    _maintenance_lock: File,
    _migration_lock: File,
}

/// Full Phase 1 forward migration quiescence guard holding all 6 non-SQLite
/// exclusive advisory locks (with `capacity.lock` and `append.lock` pinned
/// in-place) plus single-connection OS `EXCLUSIVE` SQLite locks on `libra.db`
/// and optional `private/capture.db` (`ADR-RB-07` #1, `RB-10`, `RB-10m`, `RB-10n`).
#[derive(Debug)]
pub struct ForwardPhase1QuiesceGuard {
    /// Quiesced `DELETE`-mode SQLite source connections (`c_mig`).
    pub sqlite: QuiescedRepositorySqliteSources,
    /// Held non-SQLite exclusive advisory locks (released AFTER `sqlite` closes).
    locks: ForwardPhase1Locks,
}

impl ForwardPhase1QuiesceGuard {
    /// Deterministically close held SQLite `c_mig` connections first and then
    /// release all 6 non-SQLite exclusive advisory locks in strict reverse order.
    pub async fn close(self) -> StorageResult<()> {
        let Self { sqlite, locks } = self;
        let sqlite_res = sqlite.close().await;
        drop(locks);
        sqlite_res
    }
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
type QuiesceShmHook<'a> = &'a dyn Fn(&Path) -> StorageResult<()>;

#[cfg(test)]
type QuiesceShmRemoveOverride<'a> = &'a dyn Fn(&Path) -> io::Result<()>;

#[cfg(test)]
#[derive(Default)]
struct QuiesceTestHooks<'a> {
    before_begin: Option<&'a dyn Fn(&Path)>,
    after_begin: Option<fn(&Path)>,
    after_delete_before_shm_remove: Option<QuiesceShmHook<'a>>,
    shm_remove_override: Option<QuiesceShmRemoveOverride<'a>>,
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
    #[cfg(test)] shm_remove_override: Option<QuiesceShmRemoveOverride<'_>>,
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

/// Pair of non-mutating Phase A prepared SQLite quiescence handles ready for
/// Phase B finalization once all non-SQLite and SQLite gates have cleared.
struct PreparedRepositorySqliteSources {
    git: PreparedSqliteQuiescence,
    capture: Option<PreparedSqliteQuiescence>,
}

/// Phase A (100% non-mutating) SQLite quiescence preparation for a repository's
/// `libra.db` and optional `private/capture.db`.
async fn prepare_forward_repository_sqlite_sources_no_mutate(
    common_storage: &Path,
) -> StorageResult<PreparedRepositorySqliteSources> {
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

    #[cfg(test)]
    let default_hooks = QuiesceTestHooks::default();
    #[cfg(test)]
    let git = prepare_sqlite_quiescence_no_mutate_inner(
        &git_path,
        DatabaseRole::Repository,
        &git_meta,
        &default_hooks,
    )
    .await?;
    #[cfg(not(test))]
    let git =
        prepare_sqlite_quiescence_no_mutate(&git_path, DatabaseRole::Repository, &git_meta).await?;

    let capture = if let Some(cap_meta) = capture_meta {
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
                git.close_quietly().await;
                return Err(err);
            }
        }
    } else {
        None
    };

    Ok(PreparedRepositorySqliteSources { git, capture })
}

/// Phase B finalization for prepared repository SQLite sources.
async fn finalize_forward_repository_sqlite_sources(
    prepared: PreparedRepositorySqliteSources,
) -> StorageResult<QuiescedRepositorySqliteSources> {
    let PreparedRepositorySqliteSources {
        git: git_prepared,
        capture: cap_prepared,
    } = prepared;

    #[cfg(test)]
    let default_hooks = QuiesceTestHooks::default();
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
    let prepared = prepare_forward_repository_sqlite_sources_no_mutate(common_storage).await?;
    finalize_forward_repository_sqlite_sources(prepared).await
}

/// Validate that `dir` (if present) is a regular non-symlink directory owned by
/// the effective user on Unix, without modifying permissions or creating files.
fn validate_existing_dir_no_mutate(
    dir: &Path,
    require_owner: bool,
    stage: ForwardPhase1QuiesceStage,
) -> StorageResult<bool> {
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(StorageBackendError::io(
                "inspect forward migration quiescence directory",
                err,
            ));
        }
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: path is not a regular non-symlink directory",
                stage.as_str()
            ),
        });
    }
    #[cfg(unix)]
    if require_owner {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: directory owner does not match effective user",
                    stage.as_str()
                ),
            });
        }
    }
    #[cfg(not(unix))]
    let _ = require_owner;
    Ok(true)
}

/// Optional `#[cfg(test)]` hooks for deterministic Phase 1 non-SQLite concurrency tests.
#[cfg(test)]
#[derive(Default)]
struct Phase1NonSqliteTestHooks<'a> {
    after_locks_before_tmp_recheck: Option<&'a dyn Fn(&Path)>,
}

/// Ensure a private subdirectory (`0700` on Unix) exists without following
/// symlinks. Directories created for advisory locks are never unlinked on abort
/// so live lock inodes remain stable across concurrent participants.
fn ensure_private_dir_nofollow(dir: &Path, stage: ForwardPhase1QuiesceStage) -> StorageResult<()> {
    if validate_existing_dir_no_mutate(dir, true, stage)? {
        return Ok(());
    }
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(dir)
    };
    #[cfg(not(unix))]
    let created = fs::create_dir(dir);
    match created {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(StorageBackendError::io(
                "create private directory for forward migration lock",
                err,
            ));
        }
    }
    if !validate_existing_dir_no_mutate(dir, true, stage)? {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: private directory vanished after create",
                stage.as_str()
            ),
        });
    }
    Ok(())
}

/// Verify that an opened advisory lock file descriptor is a regular single-link
/// file owned by the effective user on Unix and still matches the inode at
/// `lock_path`.
fn verify_lock_fd_identity(
    lock_path: &Path,
    file: &File,
    require_owner: bool,
    stage: ForwardPhase1QuiesceStage,
) -> StorageResult<()> {
    let fd_meta = file.metadata().map_err(|err| {
        StorageBackendError::io("inspect opened forward migration lock metadata", err)
    })?;
    if !fd_meta.is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: lock descriptor is not a regular file",
                stage.as_str()
            ),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if fd_meta.nlink() != 1 {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: lock file has unexpected hard links",
                    stage.as_str()
                ),
            });
        }
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        if require_owner && fd_meta.uid() != unsafe { libc::geteuid() } {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: lock file owner does not match effective user",
                    stage.as_str()
                ),
            });
        }
        let path_meta = fs::symlink_metadata(lock_path).map_err(|err| {
            StorageBackendError::io("re-verify forward migration lock path metadata", err)
        })?;
        if path_meta.file_type().is_symlink()
            || !path_meta.file_type().is_file()
            || path_meta.dev() != fd_meta.dev()
            || path_meta.ino() != fd_meta.ino()
        {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: lock path identity changed during acquisition",
                    stage.as_str()
                ),
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (lock_path, require_owner);
    }
    Ok(())
}

/// Non-creating, non-holding read-only probe of an already-existing lock file
/// (rejects symlinks, hard links, owner mismatch, or active contention with
/// `LBR-IO-001` while creating ZERO files on disk when absent).
fn probe_existing_lock_uncontended(
    lock_path: &Path,
    require_owner: bool,
    stage: ForwardPhase1QuiesceStage,
) -> StorageResult<()> {
    let pre_meta = match fs::symlink_metadata(lock_path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(StorageBackendError::io(
                "inspect existing forward migration lock file",
                err,
            ));
        }
    };
    if pre_meta.file_type().is_symlink() || !pre_meta.file_type().is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: lock path is not a regular non-symlink file",
                stage.as_str()
            ),
        });
    }

    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = match opts.open(lock_path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(StorageBackendError::io(
                "open existing forward migration lock file",
                err,
            ));
        }
    };

    verify_lock_fd_identity(lock_path, &file, require_owner, stage)?;
    match file.try_lock() {
        Ok(()) => {
            verify_lock_fd_identity(lock_path, &file, require_owner, stage)?;
            drop(file);
            Ok(())
        }
        Err(fs::TryLockError::WouldBlock) => Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: lock is currently held by an active external process",
                stage.as_str()
            ),
        }),
        Err(fs::TryLockError::Error(err)) => Err(StorageBackendError::io(
            "probe non-blocking exclusive advisory lock",
            err,
        )),
    }
}

/// Open an existing lock file or create a permanent in-place advisory lock
/// file (`O_NOFOLLOW | O_CLOEXEC`), verifying single-link (`nlink == 1`),
/// ownership, and pre/post-lock inode identity.
///
/// Once a lock file is created at its canonical path it is **never** unlinked
/// on error or release, preventing POSIX lock-inode split-brain between waiting
/// and subsequent producers (`ADR-RB-07` #1).
fn open_and_try_lock_inplace(
    lock_path: &Path,
    mode: u32,
    require_owner: bool,
    stage: ForwardPhase1QuiesceStage,
) -> StorageResult<File> {
    if let Ok(meta) = fs::symlink_metadata(lock_path)
        && (meta.file_type().is_symlink() || !meta.file_type().is_file())
    {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: lock path is not a regular non-symlink file",
                stage.as_str()
            ),
        });
    }

    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(mode)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(not(unix))]
    let _ = mode;

    let file = opts.open(lock_path).map_err(|err| {
        StorageBackendError::io("open or create forward migration lock file", err)
    })?;
    verify_lock_fd_identity(lock_path, &file, require_owner, stage)?;
    match file.try_lock() {
        Ok(()) => {
            verify_lock_fd_identity(lock_path, &file, require_owner, stage)?;
            Ok(file)
        }
        Err(fs::TryLockError::WouldBlock) => Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: lock is currently held by an active external process",
                stage.as_str()
            ),
        }),
        Err(fs::TryLockError::Error(err)) => Err(StorageBackendError::io(
            "acquire non-blocking exclusive advisory lock",
            err,
        )),
    }
}

/// 100% read-only inspection of `<common_storage>/private/capture-spool/tmp/`
/// rejecting any unexpired active temporary file (`age <= 24h`, matching
/// `spool.rs:28, 119-145, 499`), future-mtime entry, symlink, or non-regular
/// entry with `LBR-IO-001` and ZERO disk mutation (`ADR-RB-07` #1, `RB-10n`).
///
/// On Unix, traverses `private -> capture-spool -> tmp` via pinned
/// `O_DIRECTORY | O_NOFOLLOW` directory descriptors and verifies regular-file
/// tombstones via `openat(..., O_NOFOLLOW)` + `fstat` (`nlink == 1`,
/// `uid == geteuid()`, exact `(dev, ino)` match, bounded read).
/// Strictly expired files (`age > 24h`) are ignored and NEVER deleted here so
/// Phase 1 abort invariance keeps `capture-spool/` byte-for-byte identical.
fn verify_no_inflight_spool_tmp_files(common_storage: &Path) -> StorageResult<()> {
    let stage = ForwardPhase1QuiesceStage::CaptureSpoolInFlightTmp;
    #[cfg(unix)]
    {
        verify_no_inflight_spool_tmp_files_unix(common_storage, stage)
    }
    #[cfg(not(unix))]
    {
        verify_no_inflight_spool_tmp_files_non_unix(common_storage, stage)
    }
}

#[cfg(unix)]
fn open_verified_dir_at_unix(
    parent_fd: &std::os::unix::io::OwnedFd,
    name: &std::ffi::CStr,
    require_owner: bool,
    stage: ForwardPhase1QuiesceStage,
) -> StorageResult<Option<(std::os::unix::io::OwnedFd, libc::stat)>> {
    use std::os::unix::io::{AsRawFd as _, FromRawFd as _, OwnedFd};
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `parent_fd` is a valid open directory fd and `name` is NUL-terminated.
    let raw_fd = unsafe { libc::openat(parent_fd.as_raw_fd(), name.as_ptr(), flags) };
    if raw_fd < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: unable to open directory safely without following links",
                stage.as_str()
            ),
        });
    }
    // SAFETY: `raw_fd >= 0` is exclusively owned here.
    let dir_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `dir_fd` is valid and `st` is writable.
    if unsafe { libc::fstat(dir_fd.as_raw_fd(), st.as_mut_ptr()) } != 0 {
        return Err(StorageBackendError::io(
            "stat capture-spool directory descriptor",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `fstat` succeeded.
    let st = unsafe { st.assume_init() };
    if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: path is not a directory",
                stage.as_str()
            ),
        });
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    if require_owner && st.st_uid != unsafe { libc::geteuid() } {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: directory owner does not match effective user",
                stage.as_str()
            ),
        });
    }
    Ok(Some((dir_fd, st)))
}

#[cfg(unix)]
fn verify_no_inflight_spool_tmp_files_unix(
    common_storage: &Path,
    stage: ForwardPhase1QuiesceStage,
) -> StorageResult<()> {
    use std::{
        ffi::CString,
        io::Read as _,
        os::unix::{
            ffi::OsStrExt as _,
            io::{AsRawFd as _, FromRawFd as _, OwnedFd},
        },
        time::UNIX_EPOCH,
    };

    let c_storage = CString::new(common_storage.as_os_str().as_bytes()).map_err(|_| {
        StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: invalid storage path",
                stage.as_str()
            ),
        }
    })?;
    // SAFETY: `c_storage` is NUL-terminated.
    let raw_root = unsafe {
        libc::open(
            c_storage.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw_root < 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: storage root must be a non-symlink directory",
                stage.as_str()
            ),
        });
    }
    // SAFETY: `raw_root >= 0` is exclusively owned here.
    let root_fd = unsafe { OwnedFd::from_raw_fd(raw_root) };

    let Some((private_fd, _)) = open_verified_dir_at_unix(&root_fd, c"private", true, stage)?
    else {
        return Ok(());
    };
    let Some((spool_fd, _)) =
        open_verified_dir_at_unix(&private_fd, c"capture-spool", true, stage)?
    else {
        return Ok(());
    };

    let mut tmp_stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `spool_fd` is valid and `c"tmp"` is NUL-terminated.
    if unsafe {
        libc::fstatat(
            spool_fd.as_raw_fd(),
            c"tmp".as_ptr(),
            tmp_stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(StorageBackendError::io(
            "inspect capture-spool/tmp metadata",
            err,
        ));
    }
    // SAFETY: `fstatat` succeeded.
    let tmp_stat = unsafe { tmp_stat.assume_init() };
    let tmp_kind = tmp_stat.st_mode & libc::S_IFMT;
    if tmp_kind == libc::S_IFLNK {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: capture-spool/tmp must not be a symlink",
                stage.as_str()
            ),
        });
    }
    if tmp_kind == libc::S_IFREG {
        // Open relative to pinned `spool_fd` with `O_NOFOLLOW | O_CLOEXEC`,
        // verify `fstat` (`S_IFREG`, `nlink == 1`, owner, exact `(dev, ino)`),
        // and perform a bounded read of `CAPTURE_SPOOL_TOMBSTONE_SENTINEL.len() + 1` bytes.
        // SAFETY: `spool_fd` is valid and `c"tmp"` is NUL-terminated.
        let raw_tomb = unsafe {
            libc::openat(
                spool_fd.as_raw_fd(),
                c"tmp".as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw_tomb < 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: unable to open capture-spool/tmp tombstone safely",
                    stage.as_str()
                ),
            });
        }
        // SAFETY: `raw_tomb >= 0` is exclusively owned here.
        let tomb_file = File::from(unsafe { OwnedFd::from_raw_fd(raw_tomb) });
        let mut fd_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `tomb_file` is valid.
        if unsafe { libc::fstat(tomb_file.as_raw_fd(), fd_st.as_mut_ptr()) } != 0 {
            return Err(StorageBackendError::io(
                "stat capture-spool/tmp tombstone descriptor",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fstat` succeeded.
        let fd_st = unsafe { fd_st.assume_init() };
        let expected_len = CAPTURE_SPOOL_TOMBSTONE_SENTINEL.len();
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if (fd_st.st_mode & libc::S_IFMT) != libc::S_IFREG
            || fd_st.st_nlink != 1
            || fd_st.st_uid != euid
            || fd_st.st_dev != tmp_stat.st_dev
            || fd_st.st_ino != tmp_stat.st_ino
            || fd_st.st_size != expected_len as i64
        {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: capture-spool/tmp is an unsafe or non-tombstone file",
                    stage.as_str()
                ),
            });
        }
        let mut buf = Vec::with_capacity(expected_len + 1);
        tomb_file
            .take((expected_len + 1) as u64)
            .read_to_end(&mut buf)
            .map_err(|_| StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: failed to read capture-spool/tmp tombstone",
                    stage.as_str()
                ),
            })?;
        if buf.as_slice() != CAPTURE_SPOOL_TOMBSTONE_SENTINEL {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: capture-spool/tmp regular file does not match Redb tombstone sentinel",
                    stage.as_str()
                ),
            });
        }
        return Ok(());
    }
    if tmp_kind != libc::S_IFDIR {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: capture-spool/tmp is neither a directory nor a valid tombstone",
                stage.as_str()
            ),
        });
    }

    let Some((tmp_fd, _)) = open_verified_dir_at_unix(&spool_fd, c"tmp", true, stage)? else {
        return Ok(());
    };
    // Duplicate `tmp_fd` for `fdopendir` so `tmp_fd` stays valid for `fstatat`.
    // SAFETY: `tmp_fd` is valid.
    let dup_raw = unsafe { libc::fcntl(tmp_fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if dup_raw < 0 {
        return Err(StorageBackendError::io(
            "duplicate capture-spool/tmp directory descriptor",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `dup_raw` is an open directory fd; on success `fdopendir` owns it.
    let dir_ptr = unsafe { libc::fdopendir(dup_raw) };
    if dir_ptr.is_null() {
        let err = io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so close `dup_raw`.
        unsafe {
            libc::close(dup_raw);
        }
        return Err(StorageBackendError::io(
            "open capture-spool/tmp directory stream",
            err,
        ));
    }

    struct DirGuard(*mut libc::DIR);
    impl Drop for DirGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` was returned by `fdopendir`.
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
    }
    let _dir_guard = DirGuard(dir_ptr);

    let now = SystemTime::now();
    loop {
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        // SAFETY: `dir_ptr` is open and valid for the lifetime of `_dir_guard`.
        let entry = unsafe { libc::readdir(dir_ptr) };
        if entry.is_null() {
            let err = io::Error::last_os_error();
            if err.raw_os_error().unwrap_or(0) != 0 {
                return Err(StorageBackendError::io(
                    "iterate capture-spool/tmp directory",
                    err,
                ));
            }
            break;
        }
        // SAFETY: `entry` is non-null and `d_name` is NUL-terminated.
        let name_cstr = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name_bytes = name_cstr.to_bytes();
        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }

        let mut est = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `tmp_fd` is valid and `name_cstr` is NUL-terminated.
        if unsafe {
            libc::fstatat(
                tmp_fd.as_raw_fd(),
                name_cstr.as_ptr(),
                est.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::NotFound {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "forward migration quiescence rejected at {}: concurrent publisher modified capture-spool/tmp during scan",
                        stage.as_str()
                    ),
                });
            }
            return Err(StorageBackendError::io(
                "inspect capture-spool/tmp entry metadata",
                err,
            ));
        }
        // SAFETY: `fstatat` succeeded.
        let est = unsafe { est.assume_init() };
        if (est.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: capture-spool/tmp contains a non-regular or symlink entry",
                    stage.as_str()
                ),
            });
        }
        let mtime_secs = est.st_mtime;
        let mtime_nsecs = est.st_mtime_nsec;
        let modified = if mtime_secs >= 0 && mtime_nsecs >= 0 {
            UNIX_EPOCH + Duration::new(mtime_secs as u64, mtime_nsecs as u32)
        } else {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: capture-spool/tmp entry has invalid mtime",
                    stage.as_str()
                ),
            });
        };
        let expired = now
            .duration_since(modified)
            .is_ok_and(|age| age > CAPTURE_SPOOL_TMP_MAX_AGE);
        if !expired {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: capture-spool/tmp contains an active unexpired in-flight temporary file",
                    stage.as_str()
                ),
            });
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_no_inflight_spool_tmp_files_non_unix(
    common_storage: &Path,
    stage: ForwardPhase1QuiesceStage,
) -> StorageResult<()> {
    use std::io::Read as _;

    let private_dir = common_storage.join(PRIVATE_SUBDIR);
    if !validate_existing_dir_no_mutate(&private_dir, true, stage)? {
        return Ok(());
    }
    let spool_dir = capture_spool_dir_path(common_storage);
    if !validate_existing_dir_no_mutate(&spool_dir, true, stage)? {
        return Ok(());
    }
    let tmp_path = capture_spool_tmp_dir_path(common_storage);
    let tmp_meta = match fs::symlink_metadata(&tmp_path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(StorageBackendError::io(
                "inspect capture-spool/tmp metadata",
                err,
            ));
        }
    };
    if tmp_meta.file_type().is_symlink() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: capture-spool/tmp must not be a symlink",
                stage.as_str()
            ),
        });
    }
    if tmp_meta.file_type().is_file() {
        let expected_len = CAPTURE_SPOOL_TOMBSTONE_SENTINEL.len();
        if tmp_meta.len() != expected_len as u64 {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: capture-spool/tmp is an unexpected non-directory file",
                    stage.as_str()
                ),
            });
        }
        let file = File::open(&tmp_path).map_err(|_| StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: unable to open capture-spool/tmp tombstone",
                stage.as_str()
            ),
        })?;
        let mut buf = Vec::with_capacity(expected_len + 1);
        file.take((expected_len + 1) as u64)
            .read_to_end(&mut buf)
            .map_err(|_| StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: failed to read capture-spool/tmp tombstone",
                    stage.as_str()
                ),
            })?;
        if buf.as_slice() == CAPTURE_SPOOL_TOMBSTONE_SENTINEL {
            return Ok(());
        }
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: capture-spool/tmp is an unexpected non-directory file",
                stage.as_str()
            ),
        });
    }
    if !tmp_meta.file_type().is_dir() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: capture-spool/tmp is neither a directory nor a valid tombstone",
                stage.as_str()
            ),
        });
    }
    let now = SystemTime::now();
    let entries = fs::read_dir(&tmp_path)
        .map_err(|err| StorageBackendError::io("read capture-spool/tmp directory", err))?;
    for entry_res in entries {
        let entry = entry_res
            .map_err(|err| StorageBackendError::io("iterate capture-spool/tmp directory", err))?;
        let meta = match fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "forward migration quiescence rejected at {}: concurrent publisher modified capture-spool/tmp during scan",
                        stage.as_str()
                    ),
                });
            }
            Err(err) => {
                return Err(StorageBackendError::io(
                    "inspect capture-spool/tmp entry metadata",
                    err,
                ));
            }
        };
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: capture-spool/tmp contains a non-regular or symlink entry",
                    stage.as_str()
                ),
            });
        }
        let modified = meta.modified().map_err(|_| StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected at {}: unable to inspect capture-spool/tmp entry mtime",
                stage.as_str()
            ),
        })?;
        let expired = now
            .duration_since(modified)
            .is_ok_and(|age| age > CAPTURE_SPOOL_TMP_MAX_AGE);
        if !expired {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected at {}: capture-spool/tmp contains an active unexpired in-flight temporary file",
                    stage.as_str()
                ),
            });
        }
    }
    Ok(())
}

/// Full Phase 1 forward migration quiescence gate (`ADR-RB-07` #1, `RB-10`,
/// `RB-10m`, `RB-10n`):
///
/// Acquires all 6 non-SQLite exclusive advisory locks in **strict canonical
/// order** (`migration.lock` -> `maintenance.lock` ->
/// `capture-spool-consumer.lock` -> `capture-queue/writer.lock` -> in-place
/// `capture-spool/capacity.lock` -> in-place `capture-queue/append.lock`),
/// rejects any active unexpired `capture-spool/tmp/` publisher
/// (`spool.rs:119-145`) and any contended external lock holder with
/// `LBR-IO-001` while keeping all 3 domains (SQLite `.db`/`-wal`/`-shm`,
/// `capture-spool/`, and `capture-queue/`) 100% byte-for-byte identical on
/// rejection (rolling back any newly created lock files/dirs if a late race or
/// SQLite contention fails), and then completes single-connection `c_mig`
/// SQLite quiescence on `libra.db` and optional `private/capture.db`.
///
/// Strictly scoped to repository storage (`Repository` + `Capture`): never
/// opens, creates, or modifies `GlobalConfig` or `SystemConfig` stores.
pub async fn quiesce_forward_repository_phase1(
    common_storage: &Path,
) -> StorageResult<ForwardPhase1QuiesceGuard> {
    #[cfg(test)]
    {
        quiesce_forward_repository_phase1_inner(
            common_storage,
            &Phase1NonSqliteTestHooks::default(),
        )
        .await
    }
    #[cfg(not(test))]
    {
        quiesce_forward_repository_phase1_inner(common_storage).await
    }
}

async fn quiesce_forward_repository_phase1_inner(
    common_storage: &Path,
    #[cfg(test)] hooks: &Phase1NonSqliteTestHooks<'_>,
) -> StorageResult<ForwardPhase1QuiesceGuard> {
    if !validate_existing_dir_no_mutate(
        common_storage,
        false,
        ForwardPhase1QuiesceStage::StorageRoot,
    )? {
        return Err(StorageBackendError::NotFound {
            resource: "repository storage directory".to_string(),
        });
    }

    let private_dir = common_storage.join(PRIVATE_SUBDIR);
    let spool_dir = capture_spool_dir_path(common_storage);
    let queue_dir = capture_queue_dir_path(common_storage);
    let mig_lock_path = migration_lock_path(common_storage);
    let maint_lock_path = common_storage.join("maintenance.lock");
    let consumer_lock_path = capture_spool_consumer_lock_path(common_storage);
    let writer_lock_path = capture_queue_writer_lock_path(common_storage);
    let capacity_lock_path = capture_spool_capacity_lock_path(common_storage);
    let append_lock_path = capture_queue_append_lock_path(common_storage);

    // Phase 0: 100% non-creating pre-flight validation & contention probes
    // (creates ZERO files or directories anywhere on disk).
    let _ = validate_existing_dir_no_mutate(
        &private_dir,
        true,
        ForwardPhase1QuiesceStage::CaptureSpoolConsumerLock,
    )?;
    let _ = validate_existing_dir_no_mutate(
        &spool_dir,
        true,
        ForwardPhase1QuiesceStage::CaptureSpoolCapacityLock,
    )?;
    let _ = validate_existing_dir_no_mutate(
        &queue_dir,
        true,
        ForwardPhase1QuiesceStage::CaptureQueueAppendLock,
    )?;
    probe_existing_lock_uncontended(
        &mig_lock_path,
        true,
        ForwardPhase1QuiesceStage::MigrationLock,
    )?;
    probe_existing_lock_uncontended(
        &maint_lock_path,
        false,
        ForwardPhase1QuiesceStage::MaintenanceLock,
    )?;
    probe_existing_lock_uncontended(
        &consumer_lock_path,
        true,
        ForwardPhase1QuiesceStage::CaptureSpoolConsumerLock,
    )?;
    probe_existing_lock_uncontended(
        &writer_lock_path,
        true,
        ForwardPhase1QuiesceStage::CaptureQueueWriterLock,
    )?;
    probe_existing_lock_uncontended(
        &capacity_lock_path,
        true,
        ForwardPhase1QuiesceStage::CaptureSpoolCapacityLock,
    )?;
    probe_existing_lock_uncontended(
        &append_lock_path,
        true,
        ForwardPhase1QuiesceStage::CaptureQueueAppendLock,
    )?;
    verify_no_inflight_spool_tmp_files(common_storage)?;

    // When any private lock file is not yet materialized on disk, run a
    // non-mutating SQLite Phase A pre-check BEFORE creating any lock file so
    // SQLite contention also leaves `capture-spool/` and `capture-queue/`
    // untouched without ever needing to unlink a published lock inode.
    let any_private_lock_absent = !path_exists_no_open(&consumer_lock_path)?
        || !path_exists_no_open(&writer_lock_path)?
        || !path_exists_no_open(&capacity_lock_path)?
        || !path_exists_no_open(&append_lock_path)?;
    if any_private_lock_absent {
        let probe = prepare_forward_repository_sqlite_sources_no_mutate(common_storage).await?;
        if let Some(cap) = probe.capture {
            cap.close_quietly().await;
        }
        probe.git.close_quietly().await;
    }

    // Phase 1: Acquire ALL 6 non-SQLite locks in 100% STRICT CANONICAL ORDER
    // (`ADR-RB-07` #1). Once published at its canonical path, a lock file is
    // NEVER unlinked on rejection or drop so concurrent waiting and subsequent
    // producers always contend on the exact same inode (zero split-brain).
    let migration_lock = open_and_try_lock_inplace(
        &mig_lock_path,
        0o600,
        true,
        ForwardPhase1QuiesceStage::MigrationLock,
    )?;
    let maintenance_lock = open_and_try_lock_inplace(
        &maint_lock_path,
        0o600,
        false,
        ForwardPhase1QuiesceStage::MaintenanceLock,
    )?;
    ensure_private_dir_nofollow(
        &private_dir,
        ForwardPhase1QuiesceStage::CaptureSpoolConsumerLock,
    )?;
    let spool_consumer_lock = open_and_try_lock_inplace(
        &consumer_lock_path,
        0o600,
        true,
        ForwardPhase1QuiesceStage::CaptureSpoolConsumerLock,
    )?;
    ensure_private_dir_nofollow(
        &queue_dir,
        ForwardPhase1QuiesceStage::CaptureQueueWriterLock,
    )?;
    let queue_writer_lock = open_and_try_lock_inplace(
        &writer_lock_path,
        0o600,
        true,
        ForwardPhase1QuiesceStage::CaptureQueueWriterLock,
    )?;
    ensure_private_dir_nofollow(
        &spool_dir,
        ForwardPhase1QuiesceStage::CaptureSpoolCapacityLock,
    )?;
    let spool_capacity_lock = open_and_try_lock_inplace(
        &capacity_lock_path,
        0o600,
        true,
        ForwardPhase1QuiesceStage::CaptureSpoolCapacityLock,
    )?;
    let queue_append_lock = open_and_try_lock_inplace(
        &append_lock_path,
        0o600,
        true,
        ForwardPhase1QuiesceStage::CaptureQueueAppendLock,
    )?;

    let locks = ForwardPhase1Locks {
        _queue_append_lock: queue_append_lock,
        _spool_capacity_lock: spool_capacity_lock,
        _queue_writer_lock: queue_writer_lock,
        _spool_consumer_lock: spool_consumer_lock,
        _maintenance_lock: maintenance_lock,
        _migration_lock: migration_lock,
    };

    #[cfg(test)]
    if let Some(hook) = hooks.after_locks_before_tmp_recheck {
        hook(common_storage);
    }

    // Authoritative under-lock `capture-spool/tmp/` verification (closes any
    // TOCTOU race between pre-flight and acquiring `capacity.lock`).
    verify_no_inflight_spool_tmp_files(common_storage)?;

    // Phase 2: Single-connection SQLite Phase A + Phase B quiescence on
    // `libra.db` and optional `private/capture.db` under all 6 held locks.
    let sqlite = quiesce_forward_repository_sqlite_sources(common_storage).await?;

    Ok(ForwardPhase1QuiesceGuard { sqlite, locks })
}

// ============================================================================
// RB-10a: Forward Phase 1 3-Layout Detection & Streaming 61+25 Export to .tmp
// ============================================================================

/// Maximum number of source SQLite rows fetched and committed in a single
/// streaming migration batch (`ADR-RB-07` #1, `RB-10a`, `RB-10i`).
pub const FORWARD_MIGRATION_STREAM_BATCH_ROWS: usize = 500;

/// Sub-chunk size for Layout B primary-key coverage probes against `capture.db`
/// so composite-key `WHERE` predicates stay strictly below SQLite's bind-variable
/// and expression-depth limits.
const LAYOUT_B_KEY_PROBE_CHUNK: usize = 64;

/// Bounded timeout for exclusively-owned `.tmp` redb export write transactions.
const EXPORT_TMP_REDB_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Canonical list of all **23** inline `agent_*` capture catalog tables that
/// live inside `libra.db` prior to capture detachment (Layouts A and B),
/// matching `CAPTURE_SOURCE_TABLES` minus `schema_versions` and
/// `capture_catalog_copy_receipt`.
pub const LEGACY_INLINE_CAPTURE_TABLES: [&str; 23] = [
    "agent_audit_log",
    "agent_bridge_checkpoint",
    "agent_bridge_event",
    "agent_bridge_link",
    "agent_bridge_operation",
    "agent_bridge_session",
    "agent_capture_cloud_base",
    "agent_capture_incarnation",
    "agent_checkpoint",
    "agent_checkpoint_prune_tombstone",
    "agent_coverage_claim",
    "agent_coverage_conflict",
    "agent_coverage_revision",
    "agent_export_job",
    "agent_import_identity",
    "agent_import_tombstone",
    "agent_session",
    "agent_session_commit",
    "agent_subagent_content_claim",
    "agent_subagent_content_revision",
    "agent_subagent_link",
    "agent_usage_stats",
    "agent_workspace_scope_audit",
];

/// Classification of the three legacy SQLite repository layouts supported by
/// forward migration Phase 1 (`ADR-RB-07` #1, `RB-10a`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyRepositoryLayout {
    /// Unsplit repository: only `.libra/libra.db` exists (61 active repository
    /// tables + up to 23 inline `agent_*` capture tables); `.libra/private/capture.db`
    /// is absent.
    LayoutA,
    /// Copied-but-not-yet-detached repository: both `.libra/libra.db` and
    /// `.libra/private/capture.db` exist, `libra.db` still contains one or more
    /// inline `agent_*` tables, and `capture.db` has a valid
    /// `capture_catalog_copy_receipt` (`id = 1`) plus 100% primary-key coverage
    /// of every inline capture table.
    LayoutB,
    /// Detached dual-database repository: both `.libra/libra.db` (61 active
    /// repository tables, zero inline `agent_*` capture tables) and
    /// `.libra/private/capture.db` (25 capture tables) exist.
    LayoutC,
}

/// Summary report returned after forward Phase 1 streaming export into `.tmp`
/// redb files (`RB-10a`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardPhase1ExportReport {
    /// Detected and verified legacy SQLite layout.
    pub layout: LegacyRepositoryLayout,
    /// Target redb storage generation (`>= 1`).
    pub generation: u64,
    /// Staged temporary Git redb path (`.libra/redb/git.gen-<id>.redb.tmp`).
    pub git_tmp_path: PathBuf,
    /// Staged temporary Capture redb path (`.libra/private/capture.gen-<id>.redb.tmp`).
    pub capture_tmp_path: PathBuf,
    /// Exported row count for every one of the 61 active repository source tables.
    pub git_table_counts: BTreeMap<&'static str, u64>,
    /// Exported row count for every one of the 25 capture catalog source tables.
    pub capture_table_counts: BTreeMap<&'static str, u64>,
    /// Peak row count observed in any single streaming batch (`<= 500`).
    pub max_batch_rows: usize,
}

/// Return the canonical Phase 1 staged Git redb temporary path
/// `<common_storage>/redb/git.gen-<id>.redb.tmp`.
#[must_use]
pub fn git_redb_tmp_path(common_storage: &Path, generation: u64) -> PathBuf {
    common_storage.join(format!("{}.tmp", canonical_git_redb_rel_path(generation)))
}

/// Return the canonical Phase 1 staged Capture redb temporary path
/// `<common_storage>/private/capture.gen-<id>.redb.tmp`.
#[must_use]
pub fn capture_redb_tmp_path(common_storage: &Path, generation: u64) -> PathBuf {
    common_storage.join(format!(
        "{}.tmp",
        canonical_capture_redb_rel_path(generation)
    ))
}

// ============================================================================
// Wire-Compatible Redb Stored Envelope Mirrors (1:1 Positional Postcard Match)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MigConfigKvStoredRow {
    key: String,
    value: String,
    encrypted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigConfigKvIndexMarker {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigConfigSequenceValue {
    last_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MigMetadataStoredRow {
    value: String,
    value_type: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MigSchemaReceiptStoredRow {
    name: String,
    applied_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigRefTargetStored {
    target: RefTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigReflogStoredRow {
    ref_name: String,
    old_oid: String,
    new_oid: String,
    timestamp: i64,
    committer_name: String,
    committer_email: String,
    action: String,
    message: String,
    worktree_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigReflogIndexMarker;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigBridgePublicationStoredRow {
    commit_oid: String,
    phase: BridgePublicationPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigBridgeIndexPublicationStoredRow {
    index_digest: String,
    phase: BridgePublicationPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigGitSequenceValue {
    last_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigObjectIndexStoredRow {
    o_type: String,
    o_size: i64,
    created_at: i64,
    is_synced: bool,
    pack_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigObjectObliterationStoredRow {
    state: ObliterationState,
    reason: Option<String>,
    actor: Option<String>,
    approval_source: Option<String>,
    requested_at: String,
    tombstone_written_at: String,
    payload_deleted_at: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigNoteStoredRow {
    blob: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigRevisionOrdinalStoredRow {
    oid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigRevisionOrdinalByOidStoredRow {
    ordinal: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigRevisionOrdinalMetaStoredRow {
    tip_oid: String,
    replace_sig: String,
    max_ordinal: i64,
    built_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigUnitMarker;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigSeqCounter {
    last_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigStringValue {
    value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigU64Value {
    value: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigBoolValue {
    value: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigCaptureMarkerValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigCaptureMetaGenerationValue {
    cross_store_gen: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigCaptureSequenceValue {
    last_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigBridgeEventSeqIndexEntry {
    event_id: i64,
    payload_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigEventTimeIndexEntry {
    event_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigUsageStatsIndexEntry {
    usage_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigSessionProviderIndexEntry {
    session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MigBridgeLinkIndexEntry {
    link_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigTombstoneIndexEntry {
    tombstone_id: String,
}

const MIG_REF_TAG_LOCAL_HEAD_MAIN: u8 = 0;
const MIG_REF_TAG_LOCAL_HEAD_LINKED: u8 = 1;
const MIG_REF_TAG_REMOTE_HEAD: u8 = 2;
const MIG_REF_TAG_LOCAL_BRANCH: u8 = 3;
const MIG_REF_TAG_REMOTE_BRANCH: u8 = 4;
const MIG_REF_TAG_TAG: u8 = 5;

const MIG_SEQ_KEY_CONFIG_KV: &str = "config_kv";
const MIG_SEQ_KEY_CONFIG_LEGACY: &str = "config";
const MIG_SEQ_KEY_REFLOG: &str = "reflog";
const MIG_SEQ_KEY_WORKTREE_INTENT: &str = "worktree_intent_journal";
const MIG_CAPTURE_SINGLETON_KEY: &str = "singleton";
const MIG_SEQ_KEY_BRIDGE_EVENT: &str = "agent_bridge_event";
const MIG_SEQ_KEY_BRIDGE_LINK: &str = "agent_bridge_link";

fn encode_mig_ref_key(key: &RefKey) -> (u8, &str, &str) {
    match key {
        RefKey::LocalHead { worktree_id: None } => (MIG_REF_TAG_LOCAL_HEAD_MAIN, "", ""),
        RefKey::LocalHead {
            worktree_id: Some(wt),
        } => (MIG_REF_TAG_LOCAL_HEAD_LINKED, wt.as_str(), ""),
        RefKey::RemoteHead { remote } => (MIG_REF_TAG_REMOTE_HEAD, remote.as_str(), ""),
        RefKey::LocalBranch { name } => (MIG_REF_TAG_LOCAL_BRANCH, "", name.as_str()),
        RefKey::RemoteBranch { remote, name } => {
            (MIG_REF_TAG_REMOTE_BRANCH, remote.as_str(), name.as_str())
        }
        RefKey::Tag { name } => (MIG_REF_TAG_TAG, "", name.as_str()),
    }
}

fn parse_bridge_publication_phase(raw: &str) -> StorageResult<BridgePublicationPhase> {
    match raw {
        "prepared" => Ok(BridgePublicationPhase::Prepared),
        "published" => Ok(BridgePublicationPhase::Published),
        _ => Err(StorageBackendError::BackendInternal {
            detail: "corrupt bridge publication phase value in sqlite source".to_string(),
        }),
    }
}

fn map_export_sqlite_err(
    role: DatabaseRole,
    table: &'static str,
    context: &'static str,
    err: &sea_orm::sqlx::Error,
) -> StorageBackendError {
    let kind = if is_sqlite_busy_error(err) {
        "SQLITE_BUSY"
    } else {
        "sqlite error"
    };
    StorageBackendError::BackendInternal {
        detail: format!(
            "forward migration Phase 1 export failed for {role} table `{table}` ({context}: {kind})"
        ),
    }
}

fn is_safe_sql_ident(ident: &str) -> bool {
    let mut chars = ident.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

async fn sqlite_table_exists_on_conn(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
    table: &'static str,
) -> StorageResult<bool> {
    let row = sea_orm::sqlx::query(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
    )
    .bind(table)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|err| map_export_sqlite_err(role, table, "check table existence", &err))?;
    Ok(row.is_some())
}

async fn sqlite_table_columns_on_conn(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
    table: &'static str,
) -> StorageResult<BTreeSet<String>> {
    if !is_safe_sql_ident(table) {
        return Err(StorageBackendError::BackendInternal {
            detail: "unsafe sqlite table identifier rejected during export".to_string(),
        });
    }
    if !sqlite_table_exists_on_conn(conn, role, table).await? {
        return Ok(BTreeSet::new());
    }
    let sql = format!("SELECT name FROM pragma_table_info('{table}')");
    let rows = sea_orm::sqlx::query(AssertSqlSafe(sql))
        .fetch_all(&mut *conn)
        .await
        .map_err(|err| map_export_sqlite_err(role, table, "inspect table columns", &err))?;
    let mut cols = BTreeSet::new();
    for row in rows {
        let name: String = row
            .try_get(0)
            .map_err(|err| map_export_sqlite_err(role, table, "decode column name", &err))?;
        cols.insert(name);
    }
    Ok(cols)
}

async fn sqlite_table_key_columns_on_conn(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
    table: &'static str,
) -> StorageResult<Vec<String>> {
    if !is_safe_sql_ident(table) {
        return Err(StorageBackendError::BackendInternal {
            detail: "unsafe sqlite table identifier rejected during Layout B verification"
                .to_string(),
        });
    }
    let pk_sql = format!("SELECT name FROM pragma_table_info('{table}') WHERE pk > 0 ORDER BY pk");
    let pk_rows = sea_orm::sqlx::query(AssertSqlSafe(pk_sql))
        .fetch_all(&mut *conn)
        .await
        .map_err(|err| map_export_sqlite_err(role, table, "inspect primary key columns", &err))?;
    let mut pk_cols = Vec::with_capacity(pk_rows.len());
    for row in pk_rows {
        let col: String = row
            .try_get(0)
            .map_err(|err| map_export_sqlite_err(role, table, "decode pk column name", &err))?;
        if !is_safe_sql_ident(&col) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "Layout B verification rejected unsafe key column identifier on `{table}`"
                ),
            });
        }
        pk_cols.push(col);
    }
    if !pk_cols.is_empty() {
        return Ok(pk_cols);
    }

    // Fallback to non-partial UNIQUE index columns (e.g. `agent_coverage_claim`,
    // whose uniqueness key is `UNIQUE(session_id, logical_turn_key, coverage_schema_version)`).
    let idx_sql = format!(
        "SELECT name FROM pragma_index_list('{table}') WHERE \"unique\" = 1 AND partial = 0 ORDER BY seq"
    );
    let idx_rows = sea_orm::sqlx::query(AssertSqlSafe(idx_sql))
        .fetch_all(&mut *conn)
        .await
        .map_err(|err| map_export_sqlite_err(role, table, "inspect unique indexes", &err))?;
    for idx_row in idx_rows {
        let idx_name: String = idx_row
            .try_get(0)
            .map_err(|err| map_export_sqlite_err(role, table, "decode unique index name", &err))?;
        if !is_safe_sql_ident(&idx_name) {
            continue;
        }
        let info_sql = format!("SELECT name FROM pragma_index_info('{idx_name}') ORDER BY seqno");
        let info_rows = sea_orm::sqlx::query(AssertSqlSafe(info_sql))
            .fetch_all(&mut *conn)
            .await
            .map_err(|err| {
                map_export_sqlite_err(role, table, "inspect unique index columns", &err)
            })?;
        let mut cols = Vec::with_capacity(info_rows.len());
        let mut all_valid = !info_rows.is_empty();
        for info_row in info_rows {
            let opt_col: Option<String> = info_row.try_get(0).map_err(|err| {
                map_export_sqlite_err(role, table, "decode unique index column name", &err)
            })?;
            match opt_col {
                Some(col) if is_safe_sql_ident(&col) => cols.push(col),
                _ => {
                    all_valid = false;
                    break;
                }
            }
        }
        if all_valid && !cols.is_empty() {
            return Ok(cols);
        }
    }

    Err(StorageBackendError::BackendInternal {
        detail: format!(
            "Layout B verification failed closed: inline capture table `{table}` has neither primary key nor non-partial unique key"
        ),
    })
}

async fn verify_layout_b_table_key_coverage(
    git_conn: &mut SqliteConnection,
    cap_conn: &mut SqliteConnection,
    table: &'static str,
) -> StorageResult<()> {
    let cap_cols = sqlite_table_columns_on_conn(cap_conn, DatabaseRole::Capture, table).await?;
    if cap_cols.is_empty() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "Layout B verification failed closed: capture database is missing inline capture table `{table}`"
            ),
        });
    }
    let key_cols =
        sqlite_table_key_columns_on_conn(git_conn, DatabaseRole::Repository, table).await?;
    for col in &key_cols {
        if !cap_cols.contains(col) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "Layout B verification failed closed: capture database table `{table}` is missing key column `{col}`"
                ),
            });
        }
    }

    let select_exprs = key_cols
        .iter()
        .map(|col| format!("typeof(\"{col}\"), CAST(\"{col}\" AS TEXT)"))
        .collect::<Vec<_>>()
        .join(", ");
    let first_sql =
        format!("SELECT _rowid_, {select_exprs} FROM \"{table}\" ORDER BY _rowid_ ASC LIMIT ?");
    let next_sql = format!(
        "SELECT _rowid_, {select_exprs} FROM \"{table}\" WHERE _rowid_ > ? ORDER BY _rowid_ ASC LIMIT ?"
    );

    let batch_limit = i64::try_from(FORWARD_MIGRATION_STREAM_BATCH_ROWS).unwrap_or(500);
    let mut last_rowid: Option<i64> = None;

    loop {
        let rows = match last_rowid {
            None => sea_orm::sqlx::query(AssertSqlSafe(first_sql.as_str()))
                .bind(batch_limit)
                .fetch_all(&mut *git_conn)
                .await
                .map_err(|err| {
                    map_export_sqlite_err(
                        DatabaseRole::Repository,
                        table,
                        "scan Layout B inline keys",
                        &err,
                    )
                })?,
            Some(cursor) => sea_orm::sqlx::query(AssertSqlSafe(next_sql.as_str()))
                .bind(cursor)
                .bind(batch_limit)
                .fetch_all(&mut *git_conn)
                .await
                .map_err(|err| {
                    map_export_sqlite_err(
                        DatabaseRole::Repository,
                        table,
                        "scan Layout B inline keys",
                        &err,
                    )
                })?,
        };
        if rows.is_empty() {
            break;
        }
        let fetched_len = rows.len();
        let mut batch_keys: Vec<Vec<(String, Option<String>)>> = Vec::with_capacity(fetched_len);
        for row in &rows {
            let rowid: i64 = row.try_get(0).map_err(|err| {
                map_export_sqlite_err(
                    DatabaseRole::Repository,
                    table,
                    "decode Layout B key rowid",
                    &err,
                )
            })?;
            last_rowid = Some(rowid);
            let mut key_tuple = Vec::with_capacity(key_cols.len());
            for col_idx in 0..key_cols.len() {
                let ty: String = row.try_get(1 + col_idx * 2).map_err(|err| {
                    map_export_sqlite_err(
                        DatabaseRole::Repository,
                        table,
                        "decode Layout B key type",
                        &err,
                    )
                })?;
                let val: Option<String> = row.try_get(2 + col_idx * 2).map_err(|err| {
                    map_export_sqlite_err(
                        DatabaseRole::Repository,
                        table,
                        "decode Layout B key value",
                        &err,
                    )
                })?;
                key_tuple.push((ty, val));
            }
            batch_keys.push(key_tuple);
        }

        // Verify every distinct key tuple in bounded sub-chunks against `capture.db`.
        for chunk in batch_keys.chunks(LAYOUT_B_KEY_PROBE_CHUNK) {
            let distinct: BTreeSet<&Vec<(String, Option<String>)>> = chunk.iter().collect();
            if distinct.is_empty() {
                continue;
            }
            let mut predicate = String::new();
            for (tuple_idx, _) in distinct.iter().enumerate() {
                if tuple_idx > 0 {
                    predicate.push_str(" OR ");
                }
                predicate.push('(');
                for (col_idx, col) in key_cols.iter().enumerate() {
                    if col_idx > 0 {
                        predicate.push_str(" AND ");
                    }
                    predicate.push_str(&format!(
                        "typeof(\"{col}\") = ? AND CAST(\"{col}\" AS TEXT) IS ?"
                    ));
                }
                predicate.push(')');
            }
            let probe_sql =
                format!("SELECT DISTINCT {select_exprs} FROM \"{table}\" WHERE {predicate}");
            let mut query = sea_orm::sqlx::query(AssertSqlSafe(probe_sql.as_str()));
            for tuple in &distinct {
                for (ty, val) in *tuple {
                    query = query.bind(ty.as_str()).bind(val.as_deref());
                }
            }
            let found_rows = query.fetch_all(&mut *cap_conn).await.map_err(|err| {
                map_export_sqlite_err(
                    DatabaseRole::Capture,
                    table,
                    "verify Layout B key coverage",
                    &err,
                )
            })?;
            let mut found_set: BTreeSet<Vec<(String, Option<String>)>> = BTreeSet::new();
            for frow in &found_rows {
                let mut tuple = Vec::with_capacity(key_cols.len());
                for col_idx in 0..key_cols.len() {
                    let ty: String = frow.try_get(col_idx * 2).map_err(|err| {
                        map_export_sqlite_err(
                            DatabaseRole::Capture,
                            table,
                            "decode Layout B probe key type",
                            &err,
                        )
                    })?;
                    let val: Option<String> = frow.try_get(1 + col_idx * 2).map_err(|err| {
                        map_export_sqlite_err(
                            DatabaseRole::Capture,
                            table,
                            "decode Layout B probe key value",
                            &err,
                        )
                    })?;
                    tuple.push((ty, val));
                }
                found_set.insert(tuple);
            }
            if distinct.iter().any(|k| !found_set.contains(*k)) {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "Layout B verification failed closed: capture database is missing primary keys copied from inline table `{table}`"
                    ),
                });
            }
        }

        if fetched_len < FORWARD_MIGRATION_STREAM_BATCH_ROWS {
            break;
        }
    }
    Ok(())
}

/// Read-only inspection of the legacy SQLite repository layout over the already
/// quiesced single `c_mig` connection(s) (`ADR-RB-07` #1, `RB-10a`).
///
/// - **Layout A:** `sources.capture` is `None` (only `libra.db` exists).
/// - **Layout C:** `sources.capture` is `Some` and `libra.db` contains zero of
///   the 23 inline `agent_*` capture tables.
/// - **Layout B:** `sources.capture` is `Some` and `libra.db` still retains one
///   or more inline `agent_*` capture tables. Strictly verifies that
///   `capture.db` has a complete `capture_catalog_copy_receipt` (`id = 1`,
///   non-empty `copied_at`, exact single receipt row) and 100% table +
///   primary-key coverage for every inline capture table in `libra.db`. Any
///   missing receipt, missing table, or missing primary key fails closed with
///   `LBR-IO-001` without mutating either source database.
pub async fn inspect_legacy_repository_layout(
    sources: &mut QuiescedRepositorySqliteSources,
) -> StorageResult<LegacyRepositoryLayout> {
    let Some(cap) = sources.capture.as_mut() else {
        return Ok(LegacyRepositoryLayout::LayoutA);
    };

    let mut present_inline_tables = Vec::new();
    for table in LEGACY_INLINE_CAPTURE_TABLES {
        if sqlite_table_exists_on_conn(&mut sources.git.conn, DatabaseRole::Repository, table)
            .await?
        {
            present_inline_tables.push(table);
        }
    }

    if present_inline_tables.is_empty() {
        return Ok(LegacyRepositoryLayout::LayoutC);
    }

    // Layout B: validate `capture_catalog_copy_receipt` completeness first.
    let receipt_cols = sqlite_table_columns_on_conn(
        &mut cap.conn,
        DatabaseRole::Capture,
        "capture_catalog_copy_receipt",
    )
    .await?;
    if !receipt_cols.contains("id") || !receipt_cols.contains("copied_at") {
        return Err(StorageBackendError::BackendInternal {
            detail: "Layout B verification failed closed: capture_catalog_copy_receipt table is missing or incomplete".to_string(),
        });
    }
    let count_row = sea_orm::sqlx::query("SELECT COUNT(*) FROM capture_catalog_copy_receipt")
        .fetch_one(&mut cap.conn)
        .await
        .map_err(|err| {
            map_export_sqlite_err(
                DatabaseRole::Capture,
                "capture_catalog_copy_receipt",
                "count copy receipts",
                &err,
            )
        })?;
    let total_receipts: i64 = count_row.try_get(0).map_err(|err| {
        map_export_sqlite_err(
            DatabaseRole::Capture,
            "capture_catalog_copy_receipt",
            "decode copy receipt count",
            &err,
        )
    })?;
    if total_receipts != 1 {
        return Err(StorageBackendError::BackendInternal {
            detail: "Layout B verification failed closed: capture_catalog_copy_receipt is missing or malformed".to_string(),
        });
    }
    let receipt_row = sea_orm::sqlx::query(
        "SELECT copied_at FROM capture_catalog_copy_receipt WHERE id = 1 LIMIT 1",
    )
    .fetch_optional(&mut cap.conn)
    .await
    .map_err(|err| {
        map_export_sqlite_err(
            DatabaseRole::Capture,
            "capture_catalog_copy_receipt",
            "query copy receipt id=1",
            &err,
        )
    })?;
    let Some(receipt_row) = receipt_row else {
        return Err(StorageBackendError::BackendInternal {
            detail: "Layout B verification failed closed: capture_catalog_copy_receipt row id=1 is missing".to_string(),
        });
    };
    let copied_at: Option<String> = receipt_row.try_get(0).map_err(|err| {
        map_export_sqlite_err(
            DatabaseRole::Capture,
            "capture_catalog_copy_receipt",
            "decode copied_at",
            &err,
        )
    })?;
    if copied_at.as_deref().is_none_or(|ts| ts.trim().is_empty()) {
        return Err(StorageBackendError::BackendInternal {
            detail: "Layout B verification failed closed: capture_catalog_copy_receipt has empty copied_at".to_string(),
        });
    }

    // Verify 100% table + primary-key coverage for every inline capture table
    // still present in `libra.db` in bounded batches of <= 500 keys.
    for table in present_inline_tables {
        verify_layout_b_table_key_coverage(&mut sources.git.conn, &mut cap.conn, table).await?;
    }

    Ok(LegacyRepositoryLayout::LayoutB)
}

/// Repository tables created lazily on demand by `agent_bridge/publication.rs`
/// (`GC-RB-02`: 61 active repository tables include 2 on-demand bridge publication tables).
/// All other 59 active repository tables are created by repository schema initialization
/// and MUST exist in `libra.db` prior to forward Phase 1 export.
const ON_DEMAND_REPOSITORY_SOURCE_TABLES: [&str; 2] =
    ["agent_bridge_publication", "agent_bridge_index_publication"];

async fn sqlite_list_user_tables_on_conn(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
) -> StorageResult<BTreeSet<String>> {
    let rows = sea_orm::sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table'")
        .fetch_all(&mut *conn)
        .await
        .map_err(|err| map_export_sqlite_err(role, "sqlite_master", "list source tables", &err))?;
    let mut tables = BTreeSet::new();
    for row in rows {
        let name: String = row.try_get(0).map_err(|err| {
            map_export_sqlite_err(role, "sqlite_master", "decode table name", &err)
        })?;
        tables.insert(name);
    }
    Ok(tables)
}

async fn require_sqlite_table_columns_on_conn(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
    table: &'static str,
) -> StorageResult<BTreeSet<String>> {
    let cols = sqlite_table_columns_on_conn(conn, role, table).await?;
    if cols.is_empty() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration Phase 1 rejected incomplete {role} sqlite schema: required table `{table}` is absent"
            ),
        });
    }
    Ok(cols)
}

async fn read_sqlite_sequence_highwater(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
    table: &'static str,
) -> StorageResult<i64> {
    if !sqlite_table_exists_on_conn(conn, role, "sqlite_sequence").await? {
        return Ok(0);
    }
    let row = sea_orm::sqlx::query("SELECT seq FROM sqlite_sequence WHERE name = ? LIMIT 1")
        .bind(table)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|err| {
            map_export_sqlite_err(role, "sqlite_sequence", "read sequence high-water", &err)
        })?;
    let Some(row) = row else {
        return Ok(0);
    };
    let seq: Option<i64> = row.try_get(0).map_err(|err| {
        map_export_sqlite_err(role, "sqlite_sequence", "decode sequence high-water", &err)
    })?;
    let seq = seq.unwrap_or(0);
    if seq < 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration Phase 1 rejected corrupt negative sqlite_sequence high-water for `{table}`"
            ),
        });
    }
    Ok(seq)
}

/// Strictly read-only pre-validation of the required source-table inventory for
/// the detected legacy layout (`RB-10a`), executed over the sole held `c_mig`
/// connection(s) BEFORE creating any staging directory or `.tmp` redb artifact.
async fn validate_forward_source_table_inventory(
    sources: &mut QuiescedRepositorySqliteSources,
    layout: LegacyRepositoryLayout,
) -> StorageResult<()> {
    let git_tables =
        sqlite_list_user_tables_on_conn(&mut sources.git.conn, DatabaseRole::Repository).await?;
    for table in repository_active_source_tables() {
        if ON_DEMAND_REPOSITORY_SOURCE_TABLES.contains(&table) {
            continue;
        }
        if !git_tables.contains(table) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration Phase 1 rejected incomplete repository sqlite schema: missing required table `{table}`"
                ),
            });
        }
    }
    match layout {
        LegacyRepositoryLayout::LayoutA => {
            for table in LEGACY_INLINE_CAPTURE_TABLES {
                if !git_tables.contains(table) {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "forward migration Phase 1 rejected incomplete Layout A sqlite schema: missing required inline capture table `{table}`"
                        ),
                    });
                }
            }
        }
        LegacyRepositoryLayout::LayoutB | LegacyRepositoryLayout::LayoutC => {
            let cap =
                sources
                    .capture
                    .as_mut()
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail:
                            "forward migration Phase 1 Layout B/C missing quiesced capture source"
                                .to_string(),
                    })?;
            let cap_tables =
                sqlite_list_user_tables_on_conn(&mut cap.conn, DatabaseRole::Capture).await?;
            for table in CAPTURE_SOURCE_TABLES {
                if !cap_tables.contains(table) {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "forward migration Phase 1 rejected incomplete capture sqlite schema: missing required table `{table}`"
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

// ============================================================================
// Safe `.tmp` Redb Creation + Inode-Pinned RAII Cleanup Guard
// ============================================================================

#[cfg(unix)]
struct TmpRedbFileGuard {
    common_storage: PathBuf,
    subdir: &'static str,
    root_inode: (u64, u64),
    dir_fd: std::os::unix::io::OwnedFd,
    dir_inode: (u64, u64),
    dir_forbidden_bits: u32,
    file_name: std::ffi::CString,
    inode: (u64, u64),
    expected_file_mode: u32,
    armed: bool,
}

#[cfg(unix)]
impl TmpRedbFileGuard {
    fn verify_still_bound(&self, returned_path: &Path) -> StorageResult<()> {
        use std::{
            ffi::CString,
            os::unix::{
                ffi::OsStrExt as _,
                fs::MetadataExt as _,
                io::{AsRawFd as _, FromRawFd as _, OwnedFd},
            },
        };

        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };

        // 1. Re-verify `common_storage` root identity, ownership, and non-group/world writability.
        let c_storage = CString::new(self.common_storage.as_os_str().as_bytes()).map_err(|_| {
            StorageBackendError::BackendInternal {
                detail: "invalid repository storage root path during staged redb verification"
                    .to_string(),
            }
        })?;
        // SAFETY: `c_storage` is NUL-terminated.
        let raw_root = unsafe {
            libc::open(
                c_storage.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw_root < 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: "repository storage root became unavailable or symlinked during export"
                    .to_string(),
            });
        }
        // SAFETY: `raw_root >= 0` is exclusively owned here.
        let root_fd = unsafe { OwnedFd::from_raw_fd(raw_root) };
        let mut root_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `root_fd` is valid.
        if unsafe { libc::fstat(root_fd.as_raw_fd(), root_st.as_mut_ptr()) } != 0 {
            return Err(StorageBackendError::io(
                "re-stat repository storage root after export",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fstat` succeeded.
        let root_st = unsafe { root_st.assume_init() };
        if (root_st.st_mode & libc::S_IFMT) != libc::S_IFDIR
            || (root_st.st_dev as u64, root_st.st_ino) != self.root_inode
            || root_st.st_uid != euid
            || (u32::from(root_st.st_mode) & 0o022) != 0
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "repository storage root identity or permissions changed during export"
                    .to_string(),
            });
        }

        // 2. Re-verify `subdir` reached from `root_fd` matches our pinned `dir_fd` inode and mode.
        let c_subdir =
            CString::new(self.subdir).map_err(|_| StorageBackendError::BackendInternal {
                detail: "invalid redb staging subdirectory name".to_string(),
            })?;
        // SAFETY: `root_fd` is valid and `c_subdir` is NUL-terminated.
        let raw_dir = unsafe {
            libc::openat(
                root_fd.as_raw_fd(),
                c_subdir.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw_dir < 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "redb staging directory `{}` was replaced or removed during export",
                    self.subdir
                ),
            });
        }
        // SAFETY: `raw_dir >= 0` is exclusively owned here.
        let reopened_dir_fd = unsafe { OwnedFd::from_raw_fd(raw_dir) };
        let mut dir_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `reopened_dir_fd` is valid.
        if unsafe { libc::fstat(reopened_dir_fd.as_raw_fd(), dir_st.as_mut_ptr()) } != 0 {
            return Err(StorageBackendError::io(
                "re-stat redb staging directory after export",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fstat` succeeded.
        let dir_st = unsafe { dir_st.assume_init() };
        if (dir_st.st_mode & libc::S_IFMT) != libc::S_IFDIR
            || (dir_st.st_dev as u64, dir_st.st_ino) != self.dir_inode
            || dir_st.st_uid != euid
            || (u32::from(dir_st.st_mode) & self.dir_forbidden_bits) != 0
        {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "redb staging directory `{}` identity or permissions changed during export",
                    self.subdir
                ),
            });
        }

        // 3. Re-verify the pathname inside `reopened_dir_fd` still names our exact created inode.
        let mut file_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `reopened_dir_fd` is valid and `file_name` is NUL-terminated.
        if unsafe {
            libc::fstatat(
                reopened_dir_fd.as_raw_fd(),
                self.file_name.as_ptr(),
                file_st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "staged redb temporary pathname was unlinked during export".to_string(),
            });
        }
        // SAFETY: `fstatat` succeeded.
        let file_st = unsafe { file_st.assume_init() };
        if (file_st.st_mode & libc::S_IFMT) != libc::S_IFREG
            || (file_st.st_dev as u64, file_st.st_ino) != self.inode
            || file_st.st_nlink != 1
            || file_st.st_uid != euid
            || (u32::from(file_st.st_mode) & 0o777) != self.expected_file_mode
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "staged redb temporary pathname was replaced or modified during export"
                    .to_string(),
            });
        }

        // 4. Confirm the full returned pathname resolves directly to the same inode.
        let path_meta = fs::symlink_metadata(returned_path)
            .map_err(|err| StorageBackendError::io("verify returned staged redb pathname", err))?;
        if path_meta.file_type().is_symlink()
            || !path_meta.file_type().is_file()
            || (path_meta.dev(), path_meta.ino()) != self.inode
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "returned staged redb pathname no longer names the created inode"
                    .to_string(),
            });
        }

        Ok(())
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(unix)]
impl Drop for TmpRedbFileGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd as _;
        if !self.armed {
            return;
        }
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };

        // Refuse to unlink through a staging directory whose ownership or
        // non-writable permissions were weakened (closing multi-user rename races).
        let mut dir_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `dir_fd` is valid.
        if unsafe { libc::fstat(self.dir_fd.as_raw_fd(), dir_st.as_mut_ptr()) } != 0 {
            return;
        }
        // SAFETY: `fstat` succeeded.
        let dir_st = unsafe { dir_st.assume_init() };
        if (dir_st.st_mode & libc::S_IFMT) != libc::S_IFDIR
            || (dir_st.st_dev as u64, dir_st.st_ino) != self.dir_inode
            || dir_st.st_uid != euid
            || (u32::from(dir_st.st_mode) & self.dir_forbidden_bits) != 0
        {
            return;
        }

        let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `dir_fd` is valid and `file_name` is NUL-terminated.
        if unsafe {
            libc::fstatat(
                self.dir_fd.as_raw_fd(),
                self.file_name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            // SAFETY: `fstatat` succeeded.
            let st = unsafe { st.assume_init() };
            if (st.st_mode & libc::S_IFMT) == libc::S_IFREG
                && (st.st_dev as u64, st.st_ino) == self.inode
                && st.st_nlink == 1
                && st.st_uid == euid
            {
                // SAFETY: `dir_fd` is owned exclusively by `euid` with non-group/world-writable
                // permissions and `file_name` is NUL-terminated.
                let _ =
                    unsafe { libc::unlinkat(self.dir_fd.as_raw_fd(), self.file_name.as_ptr(), 0) };
            }
        }
    }
}

#[cfg(windows)]
mod win_tmp_guard {
    use std::{
        fs::{self, File},
        io,
        os::windows::{
            ffi::OsStrExt as _,
            io::{AsRawHandle as _, FromRawHandle as _},
        },
        path::{Path, PathBuf},
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
            FileDispositionInfo, GetFileInformationByHandle, OPEN_EXISTING,
            SetFileInformationByHandle,
        },
    };

    use super::*;

    type WinFileId = (u32, u32, u32);

    pub(super) struct WinHandleGuard(HANDLE);

    // SAFETY: Windows kernel file/directory handles are process-wide and safe to send across threads.
    unsafe impl Send for WinHandleGuard {}
    // SAFETY: Handle is only read or closed on drop.
    unsafe impl Sync for WinHandleGuard {}

    impl Drop for WinHandleGuard {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                // SAFETY: `self.0` is an exclusively owned valid Win32 handle.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    fn encode_wide_nul(path: &Path) -> Vec<u16> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        wide
    }

    fn file_info(handle: HANDLE, ctx: &'static str) -> StorageResult<BY_HANDLE_FILE_INFORMATION> {
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        // SAFETY: `handle` is valid and `info` points to writable memory.
        if unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) } == 0 {
            return Err(StorageBackendError::io(ctx, io::Error::last_os_error()));
        }
        // SAFETY: `GetFileInformationByHandle` succeeded.
        Ok(unsafe { info.assume_init() })
    }

    fn id_from_info(info: &BY_HANDLE_FILE_INFORMATION) -> WinFileId {
        (
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        )
    }

    fn open_pinned_non_reparse_dir(
        dir_path: &Path,
        subject: &'static str,
    ) -> StorageResult<(WinHandleGuard, WinFileId)> {
        let wide = encode_wide_nul(dir_path);
        // Explicitly omit `FILE_SHARE_DELETE` so the directory cannot be renamed,
        // unlinked, or swapped for a junction while the handle is held.
        // SAFETY: `wide` is NUL-terminated UTF-16.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(StorageBackendError::io(subject, io::Error::last_os_error()));
        }
        let guard = WinHandleGuard(handle);
        let info = file_info(guard.0, subject)?;
        if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0
            || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0
        {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{subject} must be a real non-reparse directory"),
            });
        }
        Ok((guard, id_from_info(&info)))
    }

    pub(super) struct TmpRedbFileGuard {
        common_storage: PathBuf,
        subdir: &'static str,
        path: PathBuf,
        _root_dir_guard: WinHandleGuard,
        root_id: WinFileId,
        _staging_dir_guard: WinHandleGuard,
        dir_id: WinFileId,
        file_id: WinFileId,
        armed: bool,
    }

    impl TmpRedbFileGuard {
        pub(super) fn verify_still_bound(&self, returned_path: &Path) -> StorageResult<()> {
            let (_root_g, root_id) =
                open_pinned_non_reparse_dir(&self.common_storage, "repository storage root")?;
            if root_id != self.root_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "repository storage root identity changed during export".to_string(),
                });
            }
            let dir_path = self.common_storage.join(self.subdir);
            let (_dir_g, dir_id) =
                open_pinned_non_reparse_dir(&dir_path, "redb staging directory")?;
            if dir_id != self.dir_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "redb staging directory `{}` identity changed during export",
                        self.subdir
                    ),
                });
            }
            let wide = encode_wide_nul(returned_path);
            // SAFETY: `wide` is NUL-terminated UTF-16.
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(StorageBackendError::io(
                    "verify returned staged redb pathname",
                    io::Error::last_os_error(),
                ));
            }
            let file_guard = WinHandleGuard(handle);
            let info = file_info(file_guard.0, "stat returned staged redb pathname")?;
            if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0
                || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0
                || id_from_info(&info) != self.file_id
                || info.nNumberOfLinks != 1
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: "returned staged redb pathname no longer names the created file"
                        .to_string(),
                });
            }
            Ok(())
        }

        pub(super) fn disarm(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for TmpRedbFileGuard {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }
            // Open exclusively (`share_mode = 0`) without following reparse points,
            // verify the exact volume+file index & link count on the opened handle,
            // and atomically delete THAT handle via `SetFileInformationByHandle`.
            let wide = encode_wide_nul(&self.path);
            // SAFETY: `wide` is NUL-terminated UTF-16.
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    DELETE | FILE_READ_ATTRIBUTES,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return;
            }
            let file_guard = WinHandleGuard(handle);
            let Ok(info) = file_info(file_guard.0, "stat staged redb temporary file for cleanup")
            else {
                return;
            };
            if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) == 0
                && (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0
                && id_from_info(&info) == self.file_id
                && info.nNumberOfLinks == 1
            {
                let disp = FILE_DISPOSITION_INFO { DeleteFile: true };
                // SAFETY: `file_guard.0` is an open handle with `DELETE` access and `disp` is valid.
                let _ = unsafe {
                    SetFileInformationByHandle(
                        file_guard.0,
                        FileDispositionInfo,
                        (&disp as *const FILE_DISPOSITION_INFO).cast(),
                        std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                    )
                };
            }
        }
    }

    pub(super) fn create_redb_tmp_database(
        common_storage: &Path,
        subdir: &'static str,
        file_name: &str,
        role: DatabaseRole,
    ) -> StorageResult<(RedbDatabase, TmpRedbFileGuard)> {
        let (root_dir_guard, root_id) =
            open_pinned_non_reparse_dir(common_storage, "repository storage root")?;
        let dir_path = common_storage.join(subdir);
        match fs::create_dir(&dir_path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => {
                return Err(StorageBackendError::io(
                    "create redb staging parent directory",
                    err,
                ));
            }
        }
        let (staging_dir_guard, dir_id) =
            open_pinned_non_reparse_dir(&dir_path, "redb staging directory")?;

        let tmp_path = dir_path.join(file_name);
        let wide_file = encode_wide_nul(&tmp_path);
        // Omit `FILE_SHARE_DELETE` so no external process can unlink/rename the
        // created `.tmp` file while `RedbDatabase` holds its open handle.
        // SAFETY: `wide_file` is NUL-terminated UTF-16.
        let raw_handle = unsafe {
            CreateFileW(
                wide_file.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if raw_handle == INVALID_HANDLE_VALUE {
            return Err(StorageBackendError::io(
                "create staged redb temporary file",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `raw_handle` is a valid exclusively owned Win32 file handle.
        let file = unsafe { File::from_raw_handle(raw_handle as *mut std::ffi::c_void) };
        let info = file_info(
            file.as_raw_handle() as HANDLE,
            "stat staged redb temporary file",
        )?;
        let file_id = id_from_info(&info);
        let guard = TmpRedbFileGuard {
            common_storage: common_storage.to_path_buf(),
            subdir,
            path: tmp_path,
            _root_dir_guard: root_dir_guard,
            root_id,
            _staging_dir_guard: staging_dir_guard,
            dir_id,
            file_id,
            armed: true,
        };
        if (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0
            || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0
            || info.nNumberOfLinks != 1
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "staged redb temporary file failed regular-file verification".to_string(),
            });
        }
        let mut builder = RedbBuilder::new();
        builder
            .set_concurrency_mode(ConcurrencyMode::MultiWriter)
            .set_cache_size(CACHE_SIZE_STANDARD_WRITE);
        let db = builder
            .create_with_backend(FileBackend::new(file))
            .map_err(|err| {
                map_redb_database_err(err, role, "create staged redb temporary database")
            })?;
        Ok((db, guard))
    }
}

#[cfg(windows)]
use win_tmp_guard::TmpRedbFileGuard;

#[cfg(all(not(unix), not(windows)))]
struct TmpRedbFileGuard;

#[cfg(all(not(unix), not(windows)))]
impl TmpRedbFileGuard {
    fn verify_still_bound(&self, _returned_path: &Path) -> StorageResult<()> {
        Err(StorageBackendError::BackendInternal {
            detail:
                "staged redb temporary file identity verification is unsupported on this platform"
                    .to_string(),
        })
    }

    fn disarm(&mut self) {}
}

#[cfg(unix)]
fn create_redb_tmp_database(
    common_storage: &Path,
    subdir: &'static str,
    file_name: &str,
    dir_mode: u32,
    file_mode: u32,
    enforce_exact_dir_mode: bool,
    role: DatabaseRole,
) -> StorageResult<(RedbDatabase, TmpRedbFileGuard)> {
    use std::{
        ffi::CString,
        os::unix::{
            ffi::OsStrExt as _,
            io::{AsRawFd as _, FromRawFd as _, OwnedFd},
        },
    };

    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };

    let c_storage = CString::new(common_storage.as_os_str().as_bytes()).map_err(|_| {
        StorageBackendError::BackendInternal {
            detail: "invalid repository storage root path".to_string(),
        }
    })?;
    // SAFETY: `c_storage` is NUL-terminated.
    let raw_root = unsafe {
        libc::open(
            c_storage.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw_root < 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "repository storage root must be a non-symlink directory".to_string(),
        });
    }
    // SAFETY: `raw_root >= 0` is exclusively owned here.
    let root_fd = unsafe { OwnedFd::from_raw_fd(raw_root) };

    let mut root_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `root_fd` is valid.
    if unsafe { libc::fstat(root_fd.as_raw_fd(), root_st.as_mut_ptr()) } != 0 {
        return Err(StorageBackendError::io(
            "stat repository storage root",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `fstat` succeeded.
    let root_st = unsafe { root_st.assume_init() };
    if (root_st.st_mode & libc::S_IFMT) != libc::S_IFDIR || root_st.st_uid != euid {
        return Err(StorageBackendError::BackendInternal {
            detail: "repository storage root is not owned by the current effective user"
                .to_string(),
        });
    }
    if (u32::from(root_st.st_mode) & 0o022) != 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "repository storage root has unsafe group- or world-writable permissions"
                .to_string(),
        });
    }
    let root_inode = (root_st.st_dev as u64, root_st.st_ino);

    let c_subdir = CString::new(subdir).map_err(|_| StorageBackendError::BackendInternal {
        detail: "invalid redb staging subdirectory name".to_string(),
    })?;
    // SAFETY: `root_fd` is valid and `c_subdir` is NUL-terminated.
    let mk_rc = unsafe {
        libc::mkdirat(
            root_fd.as_raw_fd(),
            c_subdir.as_ptr(),
            dir_mode as libc::mode_t,
        )
    };
    let created_dir = if mk_rc == 0 {
        true
    } else {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::AlreadyExists {
            return Err(StorageBackendError::io(
                "create redb staging parent directory",
                err,
            ));
        }
        false
    };
    // SAFETY: `root_fd` is valid and `c_subdir` is NUL-terminated.
    let raw_dir = unsafe {
        libc::openat(
            root_fd.as_raw_fd(),
            c_subdir.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw_dir < 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("redb staging directory `{subdir}` must be a non-symlink directory"),
        });
    }
    // SAFETY: `raw_dir >= 0` is exclusively owned here.
    let dir_fd = unsafe { OwnedFd::from_raw_fd(raw_dir) };

    let mut dir_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `dir_fd` is valid.
    if unsafe { libc::fstat(dir_fd.as_raw_fd(), dir_st.as_mut_ptr()) } != 0 {
        return Err(StorageBackendError::io(
            "stat redb staging directory",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `fstat` succeeded.
    let dir_st = unsafe { dir_st.assume_init() };
    if (dir_st.st_mode & libc::S_IFMT) != libc::S_IFDIR || dir_st.st_uid != euid {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "redb staging directory `{subdir}` is not owned by the current effective user"
            ),
        });
    }
    // Reject any group- or world-writable staging directory BEFORE creating or
    // tightening anything (closing multi-user rename/replacement races).
    if (u32::from(dir_st.st_mode) & 0o022) != 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "redb staging directory `{subdir}` has unsafe group- or world-writable permissions"
            ),
        });
    }
    let dir_forbidden_bits: u32 = if enforce_exact_dir_mode { 0o077 } else { 0o022 };
    if enforce_exact_dir_mode && u32::from(dir_st.st_mode & 0o777) != dir_mode {
        // Safe to tighten a non-group/world-writable directory owned by `euid`
        // (e.g. a legacy `0755` `.libra/private` created before strict `0700`
        // enforcement) BEFORE creating `capture.gen-<id>.redb.tmp`.
        // SAFETY: `dir_fd` is owned by `euid` and has zero group/world write bits.
        if unsafe { libc::fchmod(dir_fd.as_raw_fd(), dir_mode as libc::mode_t) } != 0 {
            return Err(StorageBackendError::io(
                "enforce private directory permissions",
                io::Error::last_os_error(),
            ));
        }
    }
    let _ = created_dir;
    let dir_inode = (dir_st.st_dev as u64, dir_st.st_ino);

    let c_file = CString::new(file_name).map_err(|_| StorageBackendError::BackendInternal {
        detail: "invalid redb staging filename".to_string(),
    })?;
    // SAFETY: `dir_fd` is valid and `c_file` is NUL-terminated.
    let raw_file = unsafe {
        libc::openat(
            dir_fd.as_raw_fd(),
            c_file.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            file_mode as libc::mode_t as libc::c_uint,
        )
    };
    if raw_file < 0 {
        return Err(StorageBackendError::io(
            "create staged redb temporary file",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `raw_file >= 0` is exclusively owned here.
    let file = File::from(unsafe { OwnedFd::from_raw_fd(raw_file) });

    // Explicitly apply `file_mode` (bypassing umask for private `0600` files).
    // SAFETY: `file` is valid and exclusively owned by `euid`.
    if unsafe { libc::fchmod(file.as_raw_fd(), file_mode as libc::mode_t) } != 0 {
        let err = io::Error::last_os_error();
        let _ = unsafe { libc::unlinkat(dir_fd.as_raw_fd(), c_file.as_ptr(), 0) };
        return Err(StorageBackendError::io(
            "set staged redb temporary file permissions",
            err,
        ));
    }

    let mut file_st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `file` is valid.
    if unsafe { libc::fstat(file.as_raw_fd(), file_st.as_mut_ptr()) } != 0 {
        let err = io::Error::last_os_error();
        let _ = unsafe { libc::unlinkat(dir_fd.as_raw_fd(), c_file.as_ptr(), 0) };
        return Err(StorageBackendError::io(
            "stat staged redb temporary file",
            err,
        ));
    }
    // SAFETY: `fstat` succeeded.
    let file_st = unsafe { file_st.assume_init() };
    let inode = (file_st.st_dev as u64, file_st.st_ino);
    let guard = TmpRedbFileGuard {
        common_storage: common_storage.to_path_buf(),
        subdir,
        root_inode,
        dir_fd,
        dir_inode,
        dir_forbidden_bits,
        file_name: c_file,
        inode,
        expected_file_mode: file_mode,
        armed: true,
    };

    if (file_st.st_mode & libc::S_IFMT) != libc::S_IFREG
        || file_st.st_nlink != 1
        || file_st.st_uid != euid
        || (u32::from(file_st.st_mode) & 0o777) != file_mode
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "staged redb temporary file failed ownership or regular-file verification"
                .to_string(),
        });
    }

    let mut builder = RedbBuilder::new();
    builder
        .set_concurrency_mode(ConcurrencyMode::MultiWriter)
        .set_cache_size(CACHE_SIZE_STANDARD_WRITE);
    let db = builder
        .create_with_backend(FileBackend::new(file))
        .map_err(|err| map_redb_database_err(err, role, "create staged redb temporary database"))?;
    Ok((db, guard))
}

#[cfg(windows)]
fn create_redb_tmp_database(
    common_storage: &Path,
    subdir: &'static str,
    file_name: &str,
    _dir_mode: u32,
    _file_mode: u32,
    _enforce_exact_dir_mode: bool,
    role: DatabaseRole,
) -> StorageResult<(RedbDatabase, TmpRedbFileGuard)> {
    win_tmp_guard::create_redb_tmp_database(common_storage, subdir, file_name, role)
}

#[cfg(all(not(unix), not(windows)))]
fn create_redb_tmp_database(
    _common_storage: &Path,
    _subdir: &'static str,
    _file_name: &str,
    _dir_mode: u32,
    _file_mode: u32,
    _enforce_exact_dir_mode: bool,
    _role: DatabaseRole,
) -> StorageResult<(RedbDatabase, TmpRedbFileGuard)> {
    Err(StorageBackendError::BackendInternal {
        detail: "safe staged redb temporary file creation is unsupported on this platform"
            .to_string(),
    })
}

// ============================================================================
// Initial Redb Schema Construction (61 Repository Tables + 25 Capture Tables)
// ============================================================================

fn initialize_git_export_redb_schema(db: &RedbDatabase) -> StorageResult<()> {
    let wtx =
        begin_write_bounded_blocking(db, DatabaseRole::Repository, EXPORT_TMP_REDB_WRITE_TIMEOUT)?;
    init_meta_schema(&wtx, DatabaseRole::Repository)?;

    // 4 ConfigStore / MetadataStore tables + index + sequences
    let _ = wtx
        .open_table(redb_cfg::CONFIG_KV_TABLE)
        .map_err(|e| map_redb_table_err(e, "init config_kv"))?;
    let _ = wtx
        .open_table(redb_cfg::CONFIG_KV_BY_KEY_INDEX)
        .map_err(|e| map_redb_table_err(e, "init config_kv_by_key"))?;
    let _ = wtx
        .open_table(redb_cfg::CONFIG_LEGACY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init config"))?;
    let _ = wtx
        .open_table(redb_cfg::METADATA_KV_TABLE)
        .map_err(|e| map_redb_table_err(e, "init metadata_kv"))?;
    let _ = wtx
        .open_table(redb_cfg::SCHEMA_VERSIONS_TABLE)
        .map_err(|e| map_redb_table_err(e, "init schema_versions"))?;
    {
        let mut seq = wtx
            .open_table(redb_cfg::CONFIG_SEQ_TABLE)
            .map_err(|e| map_redb_table_err(e, "init config_sequences"))?;
        let zero = encode_versioned(
            redb_cfg::CONFIG_SEQ_CODEC_VERSION,
            &MigConfigSequenceValue { last_id: 0 },
        )?;
        seq.insert(MIG_SEQ_KEY_CONFIG_KV, zero.as_slice())
            .map_err(|e| map_redb_storage_err(e, "seed config_kv sequence"))?;
        seq.insert(MIG_SEQ_KEY_CONFIG_LEGACY, zero.as_slice())
            .map_err(|e| map_redb_storage_err(e, "seed config sequence"))?;
    }

    // 7 GitStore core tables + bridge publication + indexes + sequence
    let _ = wtx
        .open_table(redb_git::REFERENCE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init reference"))?;
    let _ = wtx
        .open_table(redb_git::REFLOG_TABLE)
        .map_err(|e| map_redb_table_err(e, "init reflog"))?;
    let _ = wtx
        .open_table(redb_git::REFLOG_BY_REF_INDEX)
        .map_err(|e| map_redb_table_err(e, "init reflog_by_ref"))?;
    let _ = wtx
        .open_table(redb_git::BRIDGE_PUBLICATION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_publication"))?;
    {
        let mut seq = wtx
            .open_table(redb_git::GIT_SEQ_TABLE)
            .map_err(|e| map_redb_table_err(e, "init git_sequences"))?;
        let zero = encode_versioned(
            redb_git::GIT_SEQ_CODEC_VERSION,
            &MigGitSequenceValue { last_id: 0 },
        )?;
        seq.insert(MIG_SEQ_KEY_REFLOG, zero.as_slice())
            .map_err(|e| map_redb_storage_err(e, "seed reflog sequence"))?;
    }
    let _ = wtx
        .open_table(redb_git::OBJECT_INDEX_TABLE)
        .map_err(|e| map_redb_table_err(e, "init object_index"))?;
    let _ = wtx
        .open_table(redb_git::OBJECT_BY_PACK_TABLE)
        .map_err(|e| map_redb_table_err(e, "init object_by_pack"))?;
    let _ = wtx
        .open_table(redb_git::OBJECT_OBLITERATION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init object_obliteration"))?;
    let _ = wtx
        .open_table(redb_git::NOTES_TABLE)
        .map_err(|e| map_redb_table_err(e, "init notes"))?;
    let _ = wtx
        .open_table(redb_git::REVISION_ORDINAL_TABLE)
        .map_err(|e| map_redb_table_err(e, "init revision_ordinal"))?;
    let _ = wtx
        .open_table(redb_git::REVISION_ORDINAL_BY_OID_TABLE)
        .map_err(|e| map_redb_table_err(e, "init revision_ordinal_by_oid"))?;
    let _ = wtx
        .open_table(redb_git::REVISION_ORDINAL_META_TABLE)
        .map_err(|e| map_redb_table_err(e, "init revision_ordinal_meta"))?;

    // 21 RB-05 GitStateStore tables + secondary index + sequence
    let _ = wtx
        .open_table(redb_state::SEQUENCE_STATE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init sequence_state"))?;
    let _ = wtx
        .open_table(redb_state::REBASE_STATE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init rebase_state"))?;
    let _ = wtx
        .open_table(redb_state::BISECT_STATE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init bisect_state"))?;
    let _ = wtx
        .open_table(redb_state::WORKTREE_REGISTRY_CAPABILITY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init worktree_registry_capability"))?;
    let _ = wtx
        .open_table(redb_state::WORKTREE_LIFECYCLE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init worktree_lifecycle"))?;
    let _ = wtx
        .open_table(redb_state::WORKTREE_INTENT_JOURNAL_TABLE)
        .map_err(|e| map_redb_table_err(e, "init worktree_intent_journal"))?;
    let _ = wtx
        .open_table(redb_state::WORKSPACE_RECORD_TABLE)
        .map_err(|e| map_redb_table_err(e, "init workspace_record"))?;
    let _ = wtx
        .open_table(redb_state::OPERATION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init operation"))?;
    let _ = wtx
        .open_table(redb_state::OPERATION_PARENT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init operation_parent"))?;
    let _ = wtx
        .open_table(redb_state::OPERATION_HEAD_TABLE)
        .map_err(|e| map_redb_table_err(e, "init operation_head"))?;
    let _ = wtx
        .open_table(redb_state::OPERATION_JOURNAL_TABLE)
        .map_err(|e| map_redb_table_err(e, "init operation_journal"))?;
    let _ = wtx
        .open_table(redb_state::CHANGE_IDENTITY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init change_identity"))?;
    let _ = wtx
        .open_table(redb_state::CHANGE_REVISION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init change_revision"))?;
    let _ = wtx
        .open_table(redb_state::CHANGE_REVISION_BY_COMMIT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init change_revision_by_commit"))?;
    let _ = wtx
        .open_table(redb_state::CHANGE_PREDECESSOR_TABLE)
        .map_err(|e| map_redb_table_err(e, "init change_predecessor"))?;
    let _ = wtx
        .open_table(redb_state::AI_OPERATION_LINK_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_operation_link"))?;
    let _ = wtx
        .open_table(redb_state::WORKING_DIRTY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init working_dirty"))?;
    let _ = wtx
        .open_table(redb_state::WORKING_DIRTY_META_TABLE)
        .map_err(|e| map_redb_table_err(e, "init working_dirty_meta"))?;
    let _ = wtx
        .open_table(redb_state::LAYER_TABLE)
        .map_err(|e| map_redb_table_err(e, "init layer"))?;
    let _ = wtx
        .open_table(redb_state::LAYER_PATH_TABLE)
        .map_err(|e| map_redb_table_err(e, "init layer_path"))?;
    let _ = wtx
        .open_table(redb_state::SPARSE_VIEW_TABLE)
        .map_err(|e| map_redb_table_err(e, "init sparse_view"))?;
    let _ = wtx
        .open_table(redb_state::SPARSE_VIEW_META_TABLE)
        .map_err(|e| map_redb_table_err(e, "init sparse_view_meta"))?;
    {
        let mut seq = wtx
            .open_table(redb_state::GIT_STATE_SEQ_TABLE)
            .map_err(|e| map_redb_table_err(e, "init git_state_sequences"))?;
        let zero = encode_versioned(
            redb_state::GIT_STATE_CODEC_VERSION,
            &MigSeqCounter { last_id: 0 },
        )?;
        seq.insert(MIG_SEQ_KEY_WORKTREE_INTENT, zero.as_slice())
            .map_err(|e| map_redb_storage_err(e, "seed worktree_intent_journal sequence"))?;
    }

    // 28 additional RB-05b source tables + MEMORY_BY_PATH secondary index
    let _ = wtx
        .open_table(redb_state::AI_THREAD_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_thread"))?;
    let _ = wtx
        .open_table(redb_state::AI_THREAD_PARTICIPANT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_thread_participant"))?;
    let _ = wtx
        .open_table(redb_state::AI_THREAD_INTENT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_thread_intent"))?;
    let _ = wtx
        .open_table(redb_state::AI_THREAD_PROVIDER_METADATA_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_thread_provider_metadata"))?;
    let _ = wtx
        .open_table(redb_state::AI_SCHEDULER_STATE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_scheduler_state"))?;
    let _ = wtx
        .open_table(redb_state::AI_SCHEDULER_PLAN_HEAD_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_scheduler_plan_head"))?;
    let _ = wtx
        .open_table(redb_state::AI_SCHEDULER_SELECTED_PLAN_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_scheduler_selected_plan"))?;
    let _ = wtx
        .open_table(redb_state::AI_LIVE_CONTEXT_WINDOW_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_live_context_window"))?;
    let _ = wtx
        .open_table(redb_state::AI_INDEX_INTENT_PLAN_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_index_intent_plan"))?;
    let _ = wtx
        .open_table(redb_state::AI_INDEX_INTENT_TASK_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_index_intent_task"))?;
    let _ = wtx
        .open_table(redb_state::AI_INDEX_PLAN_STEP_TASK_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_index_plan_step_task"))?;
    let _ = wtx
        .open_table(redb_state::AI_INDEX_TASK_RUN_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_index_task_run"))?;
    let _ = wtx
        .open_table(redb_state::AI_INDEX_RUN_EVENT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_index_run_event"))?;
    let _ = wtx
        .open_table(redb_state::AI_INDEX_RUN_PATCHSET_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_index_run_patchset"))?;
    let _ = wtx
        .open_table(redb_state::AI_INDEX_INTENT_CONTEXT_FRAME_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_index_intent_context_frame"))?;
    let _ = wtx
        .open_table(redb_state::AI_VALIDATION_REPORT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_validation_report"))?;
    let _ = wtx
        .open_table(redb_state::AI_RISK_SCORE_BREAKDOWN_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_risk_score_breakdown"))?;
    let _ = wtx
        .open_table(redb_state::AI_DECISION_PROPOSAL_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_decision_proposal"))?;
    let _ = wtx
        .open_table(redb_state::AI_FINAL_DECISION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init ai_final_decision"))?;
    let _ = wtx
        .open_table(redb_state::APPROVED_PERMISSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init approved_permission"))?;
    let _ = wtx
        .open_table(redb_state::AUTOMATION_LOG_TABLE)
        .map_err(|e| map_redb_table_err(e, "init automation_log"))?;
    let _ = wtx
        .open_table(redb_state::SOURCE_CALL_LOG_TABLE)
        .map_err(|e| map_redb_table_err(e, "init source_call_log"))?;
    let _ = wtx
        .open_table(redb_state::MEMORY_EPISODE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init memory_episode"))?;
    let _ = wtx
        .open_table(redb_state::MEMORY_EPISODE_EVIDENCE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init memory_episode_evidence"))?;
    let _ = wtx
        .open_table(redb_state::MEMORY_PROJECTION_STATE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init memory_projection_state"))?;
    let _ = wtx
        .open_table(redb_state::MEMORY_EPISODE_PATH_TABLE)
        .map_err(|e| map_redb_table_err(e, "init memory_episode_path"))?;
    let _ = wtx
        .open_table(redb_state::MEMORY_BY_PATH_TABLE)
        .map_err(|e| map_redb_table_err(e, "init MEMORY_BY_PATH"))?;
    let _ = wtx
        .open_table(redb_state::MEMORY_EPISODE_SEARCH_DOC_TABLE)
        .map_err(|e| map_redb_table_err(e, "init memory_episode_search_doc"))?;
    let _ = wtx
        .open_table(redb_state::AGENT_BRIDGE_INDEX_PUBLICATION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_index_publication"))?;

    wtx.commit()
        .map_err(|e| map_redb_commit_err(e, "commit initial git export redb schema"))?;
    Ok(())
}

fn initialize_capture_export_redb_schema(
    db: &RedbDatabase,
    seed_initial_schema_receipt: bool,
) -> StorageResult<u64> {
    let wtx =
        begin_write_bounded_blocking(db, DatabaseRole::Capture, EXPORT_TMP_REDB_WRITE_TIMEOUT)?;
    init_meta_schema(&wtx, DatabaseRole::Capture)?;

    // 25 Capture source tables
    let mut sv = wtx
        .open_table(redb_cap::SCHEMA_VERSIONS_TABLE)
        .map_err(|e| map_redb_table_err(e, "init capture schema_versions"))?;
    let seeded_sv_count = if seed_initial_schema_receipt {
        let receipt = CaptureSchemaVersionRecord {
            version: CAPTURE_SCHEMA_VERSION,
            name: CAPTURE_SCHEMA_NAME.to_string(),
            applied_at: Utc::now().to_rfc3339(),
        };
        let bytes = encode_versioned(redb_cap::CAPTURE_CODEC_VERSION, &receipt)?;
        sv.insert(CAPTURE_SCHEMA_VERSION, bytes.as_slice())
            .map_err(|e| map_redb_storage_err(e, "seed initial capture schema_version"))?;
        1
    } else {
        0
    };
    drop(sv);

    let _ = wtx
        .open_table(redb_cap::CAPTURE_CATALOG_COPY_RECEIPT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init capture_catalog_copy_receipt"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_AUDIT_LOG_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_audit_log"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_BRIDGE_CHECKPOINT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_checkpoint"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_BRIDGE_EVENT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_event"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_BRIDGE_LINK_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_link"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_BRIDGE_OPERATION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_operation"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_BRIDGE_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_session"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_CAPTURE_CLOUD_BASE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_capture_cloud_base"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_CAPTURE_INCARNATION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_capture_incarnation"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_CHECKPOINT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_checkpoint"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_CHECKPOINT_PRUNE_TOMBSTONE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_checkpoint_prune_tombstone"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_COVERAGE_CLAIM_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_coverage_claim"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_COVERAGE_CONFLICT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_coverage_conflict"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_COVERAGE_REVISION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_coverage_revision"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_EXPORT_JOB_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_export_job"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_IMPORT_IDENTITY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_import_identity"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_IMPORT_TOMBSTONE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_import_tombstone"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_session"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_SESSION_COMMIT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_session_commit"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_SUBAGENT_CONTENT_CLAIM_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_subagent_content_claim"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_SUBAGENT_CONTENT_REVISION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_subagent_content_revision"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_SUBAGENT_LINK_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_subagent_link"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_USAGE_STATS_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_usage_stats"))?;
    let _ = wtx
        .open_table(redb_cap::AGENT_WORKSPACE_SCOPE_AUDIT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_workspace_scope_audit"))?;

    // Segregated internal meta / queue highwater / dedup / sequence tables
    {
        let mut gen_tbl = wtx
            .open_table(redb_cap::META_GENERATION_TABLE)
            .map_err(|e| map_redb_table_err(e, "init capture meta_generation"))?;
        let zero = encode_versioned(
            redb_cap::CAPTURE_CODEC_VERSION,
            &MigCaptureMetaGenerationValue { cross_store_gen: 0 },
        )?;
        gen_tbl
            .insert(MIG_CAPTURE_SINGLETON_KEY, zero.as_slice())
            .map_err(|e| map_redb_storage_err(e, "seed capture meta_generation"))?;
    }
    let _ = wtx
        .open_table(redb_cap::QUEUE_HIGHWATER_PRIORITY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init queue_highwater_priority"))?;
    let _ = wtx
        .open_table(redb_cap::QUEUE_HIGHWATER_NORMAL_TABLE)
        .map_err(|e| map_redb_table_err(e, "init queue_highwater_normal"))?;
    let _ = wtx
        .open_table(redb_cap::CAPTURE_DEDUP_KEYS_TABLE)
        .map_err(|e| map_redb_table_err(e, "init capture_dedup_keys"))?;
    let _ = wtx
        .open_table(redb_cap::CAPTURE_SEQUENCES_TABLE)
        .map_err(|e| map_redb_table_err(e, "init capture_sequences"))?;

    // 9 secondary index tables
    let _ = wtx
        .open_table(redb_cap::SESSIONS_BY_TIME_TABLE)
        .map_err(|e| map_redb_table_err(e, "init sessions_by_time"))?;
    let _ = wtx
        .open_table(redb_cap::SESSIONS_BY_PROVIDER_TABLE)
        .map_err(|e| map_redb_table_err(e, "init sessions_by_provider"))?;
    let _ = wtx
        .open_table(redb_cap::CHECKPOINTS_BY_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init checkpoints_by_session"))?;
    let _ = wtx
        .open_table(redb_cap::EVENTS_BY_TIME_TABLE)
        .map_err(|e| map_redb_table_err(e, "init events_by_time"))?;
    let _ = wtx
        .open_table(redb_cap::BRIDGE_EVENTS_BY_SEQ_TABLE)
        .map_err(|e| map_redb_table_err(e, "init bridge_events_by_seq"))?;
    let _ = wtx
        .open_table(redb_cap::BRIDGE_LINKS_BY_KEY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init bridge_links_by_key"))?;
    let _ = wtx
        .open_table(redb_cap::USAGE_STATS_BY_SESSION_EVENT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init usage_stats_by_session_event"))?;
    let _ = wtx
        .open_table(redb_cap::IMPORT_TOMBSTONES_BY_PROVIDER_TABLE)
        .map_err(|e| map_redb_table_err(e, "init import_tombstones_by_provider"))?;
    let _ = wtx
        .open_table(redb_cap::IMPORT_TOMBSTONES_BY_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init import_tombstones_by_session"))?;

    wtx.commit()
        .map_err(|e| map_redb_commit_err(e, "commit initial capture export redb schema"))?;
    Ok(seeded_sv_count)
}

// ============================================================================
// Streaming SQLite -> Redb Batch Engine (<= 500 rows / WriteTransaction)
// ============================================================================

fn col_get<'r, T>(
    row: &'r sea_orm::sqlx::sqlite::SqliteRow,
    idx: usize,
    role: DatabaseRole,
    table: &'static str,
    field: &'static str,
) -> StorageResult<T>
where
    T: sea_orm::sqlx::Decode<'r, sea_orm::sqlx::Sqlite>
        + sea_orm::sqlx::Type<sea_orm::sqlx::Sqlite>,
{
    row.try_get(idx)
        .map_err(|err| map_export_sqlite_err(role, table, field, &err))
}

struct StreamTableSpec<'a> {
    source_role: DatabaseRole,
    target_role: DatabaseRole,
    table: &'static str,
    select_cols: &'a str,
    rowid_expr: &'a str,
}

async fn stream_sqlite_table_to_redb<RowItem, FDecode, FWrite>(
    conn: &mut SqliteConnection,
    db: &RedbDatabase,
    spec: StreamTableSpec<'_>,
    mut decode_row: FDecode,
    mut write_batch: FWrite,
    max_batch_rows: &mut usize,
) -> StorageResult<u64>
where
    FDecode: FnMut(&sea_orm::sqlx::sqlite::SqliteRow) -> StorageResult<RowItem>,
    FWrite: FnMut(&WriteTransaction, &[RowItem]) -> StorageResult<()>,
{
    let StreamTableSpec {
        source_role,
        target_role,
        table,
        select_cols,
        rowid_expr,
    } = spec;
    let first_sql = format!(
        "SELECT {rowid_expr}, {select_cols} FROM \"{table}\" ORDER BY {rowid_expr} ASC LIMIT ?"
    );
    let next_sql = format!(
        "SELECT {rowid_expr}, {select_cols} FROM \"{table}\" WHERE {rowid_expr} > ? ORDER BY {rowid_expr} ASC LIMIT ?"
    );
    let batch_limit = i64::try_from(FORWARD_MIGRATION_STREAM_BATCH_ROWS).unwrap_or(500);
    let mut last_rowid: Option<i64> = None;
    let mut total_exported = 0u64;

    loop {
        let rows = match last_rowid {
            None => sea_orm::sqlx::query(AssertSqlSafe(first_sql.as_str()))
                .bind(batch_limit)
                .fetch_all(&mut *conn)
                .await
                .map_err(|err| map_export_sqlite_err(source_role, table, "select batch", &err))?,
            Some(cursor) => sea_orm::sqlx::query(AssertSqlSafe(next_sql.as_str()))
                .bind(cursor)
                .bind(batch_limit)
                .fetch_all(&mut *conn)
                .await
                .map_err(|err| map_export_sqlite_err(source_role, table, "select batch", &err))?,
        };
        if rows.is_empty() {
            break;
        }
        let batch_len = rows.len();
        if batch_len > FORWARD_MIGRATION_STREAM_BATCH_ROWS {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "streaming export batch on `{table}` exceeded limit {FORWARD_MIGRATION_STREAM_BATCH_ROWS}"
                ),
            });
        }
        if batch_len > *max_batch_rows {
            *max_batch_rows = batch_len;
        }

        let mut decoded = Vec::with_capacity(batch_len);
        for row in &rows {
            let rowid: i64 = col_get(row, 0, source_role, table, "rowid")?;
            last_rowid = Some(rowid);
            decoded.push(decode_row(row)?);
        }

        let wtx = begin_write_bounded_blocking(db, target_role, EXPORT_TMP_REDB_WRITE_TIMEOUT)?;
        write_batch(&wtx, &decoded)?;
        wtx.commit()
            .map_err(|err| map_redb_commit_err(err, "commit streaming migration batch"))?;

        total_exported = total_exported.saturating_add(batch_len as u64);
        if batch_len < FORWARD_MIGRATION_STREAM_BATCH_ROWS {
            break;
        }
    }

    Ok(total_exported)
}

fn persist_config_sequence_highwater(
    db: &RedbDatabase,
    seq_key: &'static str,
    last_id: u64,
) -> StorageResult<()> {
    if last_id == 0 {
        return Ok(());
    }
    let wtx =
        begin_write_bounded_blocking(db, DatabaseRole::Repository, EXPORT_TMP_REDB_WRITE_TIMEOUT)?;
    {
        let mut seq = wtx
            .open_table(redb_cfg::CONFIG_SEQ_TABLE)
            .map_err(|e| map_redb_table_err(e, "open config_sequences"))?;
        let seq_bytes = encode_versioned(
            redb_cfg::CONFIG_SEQ_CODEC_VERSION,
            &MigConfigSequenceValue { last_id },
        )?;
        seq.insert(seq_key, seq_bytes.as_slice())
            .map_err(|e| map_redb_storage_err(e, "persist config sequence high-water"))?;
    }
    wtx.commit()
        .map_err(|err| map_redb_commit_err(err, "commit config sequence high-water"))
}

fn persist_git_sequence_highwater(
    db: &RedbDatabase,
    seq_key: &'static str,
    last_id: u64,
) -> StorageResult<()> {
    if last_id == 0 {
        return Ok(());
    }
    let wtx =
        begin_write_bounded_blocking(db, DatabaseRole::Repository, EXPORT_TMP_REDB_WRITE_TIMEOUT)?;
    {
        let mut seq = wtx
            .open_table(redb_git::GIT_SEQ_TABLE)
            .map_err(|e| map_redb_table_err(e, "open git_sequences"))?;
        let seq_bytes = encode_versioned(
            redb_git::GIT_SEQ_CODEC_VERSION,
            &MigGitSequenceValue { last_id },
        )?;
        seq.insert(seq_key, seq_bytes.as_slice())
            .map_err(|e| map_redb_storage_err(e, "persist git sequence high-water"))?;
    }
    wtx.commit()
        .map_err(|err| map_redb_commit_err(err, "commit git sequence high-water"))
}

fn persist_git_state_sequence_highwater(
    db: &RedbDatabase,
    seq_key: &'static str,
    last_id: i64,
) -> StorageResult<()> {
    if last_id <= 0 {
        return Ok(());
    }
    let wtx =
        begin_write_bounded_blocking(db, DatabaseRole::Repository, EXPORT_TMP_REDB_WRITE_TIMEOUT)?;
    {
        let mut seq = wtx
            .open_table(redb_state::GIT_STATE_SEQ_TABLE)
            .map_err(|e| map_redb_table_err(e, "open git_state_sequences"))?;
        let seq_bytes = encode_versioned(
            redb_state::GIT_STATE_CODEC_VERSION,
            &MigSeqCounter { last_id },
        )?;
        seq.insert(seq_key, seq_bytes.as_slice())
            .map_err(|e| map_redb_storage_err(e, "persist git_state sequence high-water"))?;
    }
    wtx.commit()
        .map_err(|err| map_redb_commit_err(err, "commit git_state sequence high-water"))
}

fn persist_capture_sequence_highwater(
    db: &RedbDatabase,
    seq_key: &'static str,
    last_id: i64,
) -> StorageResult<()> {
    if last_id <= 0 {
        return Ok(());
    }
    let wtx =
        begin_write_bounded_blocking(db, DatabaseRole::Capture, EXPORT_TMP_REDB_WRITE_TIMEOUT)?;
    {
        let mut seq = wtx
            .open_table(redb_cap::CAPTURE_SEQUENCES_TABLE)
            .map_err(|e| map_redb_table_err(e, "open capture_sequences"))?;
        let seq_bytes = encode_versioned(
            redb_cap::CAPTURE_CODEC_VERSION,
            &MigCaptureSequenceValue { last_id },
        )?;
        seq.insert(seq_key, seq_bytes.as_slice())
            .map_err(|e| map_redb_storage_err(e, "persist capture sequence high-water"))?;
    }
    wtx.commit()
        .map_err(|err| map_redb_commit_err(err, "commit capture sequence high-water"))
}

fn require_columns(
    cols: &BTreeSet<String>,
    table: &'static str,
    required: &[&str],
) -> StorageResult<()> {
    for col in required {
        if !cols.contains(*col) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration Phase 1 rejected incompatible sqlite schema: table `{table}` is missing required column `{col}`"
                ),
            });
        }
    }
    Ok(())
}

// ============================================================================
// Export All 61 Active Repository Source Tables (libra.db -> git.gen-<id>.redb.tmp)
// ============================================================================

async fn export_repository_tables_to_redb(
    conn: &mut SqliteConnection,
    db: &RedbDatabase,
    counts: &mut BTreeMap<&'static str, u64>,
    max_batch_rows: &mut usize,
) -> StorageResult<()> {
    let r = DatabaseRole::Repository;

    // 1. schema_versions
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "schema_versions").await?;
        let n = {
            require_columns(&cols, "schema_versions", &["version", "name", "applied_at"])?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "schema_versions",
                    select_cols: "version, name, applied_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok((
                        col_get::<i64>(row, 1, r, "schema_versions", "version")?,
                        MigSchemaReceiptStoredRow {
                            name: col_get(row, 2, r, "schema_versions", "name")?,
                            applied_at: col_get(row, 3, r, "schema_versions", "applied_at")?,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cfg::SCHEMA_VERSIONS_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open schema_versions"))?;
                    for (version, stored) in batch {
                        let bytes =
                            encode_versioned(redb_cfg::SCHEMA_RECEIPT_CODEC_VERSION, stored)?;
                        tbl.insert(*version, bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert schema_versions"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("schema_versions", n);
    }

    // 2. config (legacy 3-column table)
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "config").await?;
        let mut max_id = u64::try_from(read_sqlite_sequence_highwater(conn, r, "config").await?)
            .map_err(|_| StorageBackendError::BackendInternal {
                detail: "negative sqlite_sequence high-water for `config`".to_string(),
            })?;
        let n = {
            require_columns(
                &cols,
                "config",
                &["id", "configuration", "name", "key", "value"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "config",
                    select_cols: "id, configuration, name, key, value",
                    rowid_expr: "id",
                },
                |row| {
                    let id_i64: i64 = col_get(row, 1, r, "config", "id")?;
                    let id = u64::try_from(id_i64).map_err(|_| {
                        StorageBackendError::BackendInternal {
                            detail: "negative id in sqlite `config` table".to_string(),
                        }
                    })?;
                    Ok((
                        id,
                        LegacyConfigEntry {
                            configuration: col_get(row, 2, r, "config", "configuration")?,
                            name: col_get(row, 3, r, "config", "name")?,
                            key: col_get(row, 4, r, "config", "key")?,
                            value: col_get(row, 5, r, "config", "value")?,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cfg::CONFIG_LEGACY_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open config"))?;
                    for (id, entry) in batch {
                        if *id > max_id {
                            max_id = *id;
                        }
                        let bytes = encode_versioned(redb_cfg::CONFIG_LEGACY_CODEC_VERSION, entry)?;
                        tbl.insert(*id, bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert config"))?;
                    }
                    let mut seq = wtx
                        .open_table(redb_cfg::CONFIG_SEQ_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open config_sequences"))?;
                    let seq_bytes = encode_versioned(
                        redb_cfg::CONFIG_SEQ_CODEC_VERSION,
                        &MigConfigSequenceValue { last_id: max_id },
                    )?;
                    seq.insert(MIG_SEQ_KEY_CONFIG_LEGACY, seq_bytes.as_slice())
                        .map_err(|e| map_redb_storage_err(e, "update config sequence"))?;
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        persist_config_sequence_highwater(db, MIG_SEQ_KEY_CONFIG_LEGACY, max_id)?;
        counts.insert("config", n);
    }

    // 3. config_kv + config_kv_by_key
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "config_kv").await?;
        let mut max_id = u64::try_from(read_sqlite_sequence_highwater(conn, r, "config_kv").await?)
            .map_err(|_| StorageBackendError::BackendInternal {
                detail: "negative sqlite_sequence high-water for `config_kv`".to_string(),
            })?;
        let n = {
            require_columns(&cols, "config_kv", &["id", "key", "value", "encrypted"])?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "config_kv",
                    select_cols: "id, key, value, encrypted",
                    rowid_expr: "id",
                },
                |row| {
                    let id_i64: i64 = col_get(row, 1, r, "config_kv", "id")?;
                    let id = u64::try_from(id_i64).map_err(|_| {
                        StorageBackendError::BackendInternal {
                            detail: "negative id in sqlite `config_kv` table".to_string(),
                        }
                    })?;
                    let enc_i64: i64 = col_get(row, 4, r, "config_kv", "encrypted")?;
                    Ok((
                        id,
                        MigConfigKvStoredRow {
                            key: col_get(row, 2, r, "config_kv", "key")?,
                            value: col_get(row, 3, r, "config_kv", "value")?,
                            encrypted: enc_i64 != 0,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cfg::CONFIG_KV_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open config_kv"))?;
                    let mut idx = wtx
                        .open_table(redb_cfg::CONFIG_KV_BY_KEY_INDEX)
                        .map_err(|e| map_redb_table_err(e, "open config_kv_by_key"))?;
                    let marker = encode_versioned(
                        redb_cfg::CONFIG_KV_INDEX_CODEC_VERSION,
                        &MigConfigKvIndexMarker {},
                    )?;
                    for (id, entry) in batch {
                        if *id > max_id {
                            max_id = *id;
                        }
                        let bytes = encode_versioned(redb_cfg::CONFIG_KV_CODEC_VERSION, entry)?;
                        tbl.insert(*id, bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert config_kv"))?;
                        idx.insert((entry.key.as_str(), *id), marker.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert config_kv_by_key"))?;
                    }
                    let mut seq = wtx
                        .open_table(redb_cfg::CONFIG_SEQ_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open config_sequences"))?;
                    let seq_bytes = encode_versioned(
                        redb_cfg::CONFIG_SEQ_CODEC_VERSION,
                        &MigConfigSequenceValue { last_id: max_id },
                    )?;
                    seq.insert(MIG_SEQ_KEY_CONFIG_KV, seq_bytes.as_slice())
                        .map_err(|e| map_redb_storage_err(e, "update config_kv sequence"))?;
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        persist_config_sequence_highwater(db, MIG_SEQ_KEY_CONFIG_KV, max_id)?;
        counts.insert("config_kv", n);
    }

    // 4. metadata_kv
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "metadata_kv").await?;
        let n = {
            require_columns(
                &cols,
                "metadata_kv",
                &[
                    "scope",
                    "target",
                    "key",
                    "value",
                    "value_type",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "metadata_kv",
                    select_cols: "scope, target, key, value, value_type, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok((
                        col_get::<String>(row, 1, r, "metadata_kv", "scope")?,
                        col_get::<String>(row, 2, r, "metadata_kv", "target")?,
                        col_get::<String>(row, 3, r, "metadata_kv", "key")?,
                        MigMetadataStoredRow {
                            value: col_get(row, 4, r, "metadata_kv", "value")?,
                            value_type: col_get(row, 5, r, "metadata_kv", "value_type")?,
                            created_at: col_get(row, 6, r, "metadata_kv", "created_at")?,
                            updated_at: col_get(row, 7, r, "metadata_kv", "updated_at")?,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cfg::METADATA_KV_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open metadata_kv"))?;
                    for (scope, target, key, stored) in batch {
                        let bytes = encode_versioned(redb_cfg::METADATA_KV_CODEC_VERSION, stored)?;
                        tbl.insert(
                            (scope.as_str(), target.as_str(), key.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert metadata_kv"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("metadata_kv", n);
    }

    // 5. reference
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "reference").await?;
        let n = {
            require_columns(&cols, "reference", &["name", "kind", "commit", "remote"])?;
            let select = if cols.contains("worktree_id") {
                "name, kind, \"commit\", remote, worktree_id"
            } else {
                "name, kind, \"commit\", remote, NULL"
            };
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "reference",
                    select_cols: select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let name: Option<String> = col_get(row, 1, r, "reference", "name")?;
                    let kind: String = col_get(row, 2, r, "reference", "kind")?;
                    let commit: Option<String> = col_get(row, 3, r, "reference", "commit")?;
                    let remote: Option<String> = col_get(row, 4, r, "reference", "remote")?;
                    let worktree_id: Option<String> =
                        col_get(row, 5, r, "reference", "worktree_id")?;

                    let key = match kind.as_str() {
                        "Head" => match remote {
                            None => RefKey::LocalHead { worktree_id },
                            Some(remote) => {
                                if worktree_id.is_some() {
                                    return Err(StorageBackendError::BackendInternal {
                                        detail: "corrupt remote HEAD row carrying worktree_id"
                                            .to_string(),
                                    });
                                }
                                RefKey::RemoteHead { remote }
                            }
                        },
                        "Branch" => {
                            if worktree_id.is_some() {
                                return Err(StorageBackendError::BackendInternal {
                                    detail: "corrupt branch row carrying worktree_id".to_string(),
                                });
                            }
                            let branch_name = name.clone().ok_or_else(|| {
                                StorageBackendError::BackendInternal {
                                    detail: "corrupt branch row missing name".to_string(),
                                }
                            })?;
                            match remote {
                                None => RefKey::LocalBranch { name: branch_name },
                                Some(remote) => RefKey::RemoteBranch {
                                    remote,
                                    name: branch_name,
                                },
                            }
                        }
                        "Tag" => {
                            if remote.is_some() || worktree_id.is_some() {
                                return Err(StorageBackendError::BackendInternal {
                                    detail: "corrupt tag row carrying remote or worktree_id"
                                        .to_string(),
                                });
                            }
                            let tag_name = name.clone().ok_or_else(|| {
                                StorageBackendError::BackendInternal {
                                    detail: "corrupt tag row missing name".to_string(),
                                }
                            })?;
                            RefKey::Tag { name: tag_name }
                        }
                        _ => {
                            return Err(StorageBackendError::BackendInternal {
                                detail: "corrupt reference row with unknown kind".to_string(),
                            });
                        }
                    };

                    let target = match &key {
                        RefKey::LocalHead { .. } | RefKey::RemoteHead { .. } => {
                            if let Some(sym) = name {
                                if sym.is_empty() {
                                    return Err(StorageBackendError::BackendInternal {
                                        detail: "corrupt symbolic HEAD with empty branch name"
                                            .to_string(),
                                    });
                                }
                                RefTarget::Symbolic(sym)
                            } else if let Some(oid) = commit {
                                if oid.is_empty() {
                                    return Err(StorageBackendError::BackendInternal {
                                        detail: "corrupt detached HEAD with empty commit OID"
                                            .to_string(),
                                    });
                                }
                                RefTarget::Direct(oid)
                            } else {
                                return Err(StorageBackendError::BackendInternal {
                                    detail:
                                        "corrupt HEAD row missing both branch name and commit OID"
                                            .to_string(),
                                });
                            }
                        }
                        RefKey::LocalBranch { .. }
                        | RefKey::RemoteBranch { .. }
                        | RefKey::Tag { .. } => {
                            let oid =
                                commit.ok_or_else(|| StorageBackendError::BackendInternal {
                                    detail: "corrupt branch/tag row missing commit OID".to_string(),
                                })?;
                            if oid.is_empty() {
                                return Err(StorageBackendError::BackendInternal {
                                    detail: "corrupt branch/tag row with empty commit OID"
                                        .to_string(),
                                });
                            }
                            RefTarget::Direct(oid)
                        }
                    };

                    Ok((key, MigRefTargetStored { target }))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_git::REFERENCE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open reference"))?;
                    for (key, stored) in batch {
                        let enc_key = encode_mig_ref_key(key);
                        let bytes = encode_versioned(redb_git::GIT_REF_CODEC_VERSION, stored)?;
                        if tbl
                            .insert(enc_key, bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert reference"))?
                            .is_some()
                        {
                            return Err(StorageBackendError::BackendInternal {
                                detail: "ambiguous duplicate reference rows found in sqlite source"
                                    .to_string(),
                            });
                        }
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("reference", n);
    }

    // 6. reflog + reflog_by_ref + git_sequences
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "reflog").await?;
        let mut max_id = u64::try_from(read_sqlite_sequence_highwater(conn, r, "reflog").await?)
            .map_err(|_| StorageBackendError::BackendInternal {
                detail: "negative sqlite_sequence high-water for `reflog`".to_string(),
            })?;
        let n = {
            require_columns(
                &cols,
                "reflog",
                &[
                    "id",
                    "ref_name",
                    "old_oid",
                    "new_oid",
                    "committer_name",
                    "committer_email",
                    "timestamp",
                    "action",
                    "message",
                ],
            )?;
            let select = if cols.contains("worktree_id") {
                "id, ref_name, old_oid, new_oid, committer_name, committer_email, timestamp, action, message, worktree_id"
            } else {
                "id, ref_name, old_oid, new_oid, committer_name, committer_email, timestamp, action, message, NULL"
            };
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "reflog",
                    select_cols: select,
                    rowid_expr: "id",
                },
                |row| {
                    let id_i64: i64 = col_get(row, 1, r, "reflog", "id")?;
                    let id = u64::try_from(id_i64).map_err(|_| {
                        StorageBackendError::BackendInternal {
                            detail: "negative id in sqlite `reflog` table".to_string(),
                        }
                    })?;
                    Ok((
                        id,
                        MigReflogStoredRow {
                            ref_name: col_get(row, 2, r, "reflog", "ref_name")?,
                            old_oid: col_get(row, 3, r, "reflog", "old_oid")?,
                            new_oid: col_get(row, 4, r, "reflog", "new_oid")?,
                            committer_name: col_get(row, 5, r, "reflog", "committer_name")?,
                            committer_email: col_get(row, 6, r, "reflog", "committer_email")?,
                            timestamp: col_get(row, 7, r, "reflog", "timestamp")?,
                            action: col_get(row, 8, r, "reflog", "action")?,
                            message: col_get(row, 9, r, "reflog", "message")?,
                            worktree_id: col_get(row, 10, r, "reflog", "worktree_id")?,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_git::REFLOG_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open reflog"))?;
                    let mut idx = wtx
                        .open_table(redb_git::REFLOG_BY_REF_INDEX)
                        .map_err(|e| map_redb_table_err(e, "open reflog_by_ref"))?;
                    let marker = encode_versioned(
                        redb_git::GIT_REFLOG_INDEX_CODEC_VERSION,
                        &MigReflogIndexMarker,
                    )?;
                    for (id, stored) in batch {
                        if *id > max_id {
                            max_id = *id;
                        }
                        let bytes = encode_versioned(redb_git::GIT_REFLOG_CODEC_VERSION, stored)?;
                        tbl.insert(*id, bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert reflog"))?;
                        let wt = stored.worktree_id.as_deref().unwrap_or("");
                        idx.insert((stored.ref_name.as_str(), wt, *id), marker.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert reflog_by_ref"))?;
                    }
                    let mut seq = wtx
                        .open_table(redb_git::GIT_SEQ_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open git_sequences"))?;
                    let seq_bytes = encode_versioned(
                        redb_git::GIT_SEQ_CODEC_VERSION,
                        &MigGitSequenceValue { last_id: max_id },
                    )?;
                    seq.insert(MIG_SEQ_KEY_REFLOG, seq_bytes.as_slice())
                        .map_err(|e| map_redb_storage_err(e, "update reflog sequence"))?;
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        persist_git_sequence_highwater(db, MIG_SEQ_KEY_REFLOG, max_id)?;
        counts.insert("reflog", n);
    }

    // 7. notes
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "notes").await?;
        let n = {
            require_columns(&cols, "notes", &["notes_ref", "object", "blob"])?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "notes",
                    select_cols: "notes_ref, object, blob",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok((
                        col_get::<String>(row, 1, r, "notes", "notes_ref")?,
                        col_get::<String>(row, 2, r, "notes", "object")?,
                        MigNoteStoredRow {
                            blob: col_get(row, 3, r, "notes", "blob")?,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_git::NOTES_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open notes"))?;
                    for (notes_ref, object, stored) in batch {
                        let bytes = encode_versioned(redb_git::GIT_NOTES_CODEC_VERSION, stored)?;
                        tbl.insert((notes_ref.as_str(), object.as_str()), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert notes"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("notes", n);
    }

    // 8. object_index
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "object_index").await?;
        let n = {
            require_columns(
                &cols,
                "object_index",
                &["repo_id", "o_id", "o_type", "o_size", "created_at"],
            )?;
            let select = if cols.contains("is_synced") {
                "repo_id, o_id, o_type, o_size, created_at, is_synced"
            } else {
                "repo_id, o_id, o_type, o_size, created_at, 0"
            };
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "object_index",
                    select_cols: select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let is_synced: Option<i64> = col_get(row, 6, r, "object_index", "is_synced")?;
                    Ok((
                        col_get::<String>(row, 1, r, "object_index", "repo_id")?,
                        col_get::<String>(row, 2, r, "object_index", "o_id")?,
                        MigObjectIndexStoredRow {
                            o_type: col_get(row, 3, r, "object_index", "o_type")?,
                            o_size: col_get(row, 4, r, "object_index", "o_size")?,
                            created_at: col_get(row, 5, r, "object_index", "created_at")?,
                            is_synced: is_synced.unwrap_or(0) != 0,
                            pack_id: None,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_git::OBJECT_INDEX_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open object_index"))?;
                    for (repo_id, o_id, stored) in batch {
                        let bytes =
                            encode_versioned(redb_git::GIT_OBJECT_INDEX_CODEC_VERSION, stored)?;
                        tbl.insert((repo_id.as_str(), o_id.as_str()), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert object_index"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("object_index", n);
    }

    // 9. object_obliteration
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "object_obliteration").await?;
        let n = {
            require_columns(
                &cols,
                "object_obliteration",
                &[
                    "oid",
                    "hash_kind",
                    "state",
                    "reason",
                    "actor",
                    "approval_source",
                    "requested_at",
                    "tombstone_written_at",
                    "payload_deleted_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "object_obliteration",
                    select_cols: "oid, hash_kind, state, reason, actor, approval_source, requested_at, tombstone_written_at, payload_deleted_at, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let oid: String = col_get(row, 1, r, "object_obliteration", "oid")?;
                    let hk_str: String = col_get(row, 2, r, "object_obliteration", "hash_kind")?;
                    let state_str: String = col_get(row, 3, r, "object_obliteration", "state")?;
                    let hash_kind = ObjectHashKind::parse(&hk_str)?;
                    let state = ObliterationState::parse(&state_str)?;
                    Ok((
                        oid,
                        hash_kind,
                        MigObjectObliterationStoredRow {
                            state,
                            reason: col_get(row, 4, r, "object_obliteration", "reason")?,
                            actor: col_get(row, 5, r, "object_obliteration", "actor")?,
                            approval_source: col_get(
                                row,
                                6,
                                r,
                                "object_obliteration",
                                "approval_source",
                            )?,
                            requested_at: col_get(
                                row,
                                7,
                                r,
                                "object_obliteration",
                                "requested_at",
                            )?,
                            tombstone_written_at: col_get(
                                row,
                                8,
                                r,
                                "object_obliteration",
                                "tombstone_written_at",
                            )?,
                            payload_deleted_at: col_get(
                                row,
                                9,
                                r,
                                "object_obliteration",
                                "payload_deleted_at",
                            )?,
                            updated_at: col_get(row, 10, r, "object_obliteration", "updated_at")?,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_git::OBJECT_OBLITERATION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open object_obliteration"))?;
                    for (oid, hash_kind, stored) in batch {
                        let bytes = encode_versioned(
                            redb_git::GIT_OBJECT_OBLITERATION_CODEC_VERSION,
                            stored,
                        )?;
                        tbl.insert((oid.as_str(), hash_kind.as_str()), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert object_obliteration"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("object_obliteration", n);
    }

    // 10. revision_ordinal + revision_ordinal_by_oid
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "revision_ordinal").await?;
        let n = {
            require_columns(&cols, "revision_ordinal", &["ref_name", "ordinal", "oid"])?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "revision_ordinal",
                    select_cols: "ref_name, ordinal, oid",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok((
                        col_get::<String>(row, 1, r, "revision_ordinal", "ref_name")?,
                        col_get::<i64>(row, 2, r, "revision_ordinal", "ordinal")?,
                        col_get::<String>(row, 3, r, "revision_ordinal", "oid")?,
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_git::REVISION_ORDINAL_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open revision_ordinal"))?;
                    let mut by_oid = wtx
                        .open_table(redb_git::REVISION_ORDINAL_BY_OID_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open revision_ordinal_by_oid"))?;
                    for (ref_name, ordinal, oid) in batch {
                        let row_bytes = encode_versioned(
                            redb_git::GIT_REVISION_ORDINAL_CODEC_VERSION,
                            &MigRevisionOrdinalStoredRow { oid: oid.clone() },
                        )?;
                        tbl.insert((ref_name.as_str(), *ordinal), row_bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert revision_ordinal"))?;
                        let idx_bytes = encode_versioned(
                            redb_git::GIT_REVISION_ORDINAL_BY_OID_CODEC_VERSION,
                            &MigRevisionOrdinalByOidStoredRow { ordinal: *ordinal },
                        )?;
                        by_oid
                            .insert((ref_name.as_str(), oid.as_str()), idx_bytes.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert revision_ordinal_by_oid")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("revision_ordinal", n);
    }

    // 11. revision_ordinal_meta
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "revision_ordinal_meta").await?;
        let n = {
            require_columns(
                &cols,
                "revision_ordinal_meta",
                &[
                    "ref_name",
                    "tip_oid",
                    "replace_sig",
                    "max_ordinal",
                    "built_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "revision_ordinal_meta",
                    select_cols: "ref_name, tip_oid, replace_sig, max_ordinal, built_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok((
                        col_get::<String>(row, 1, r, "revision_ordinal_meta", "ref_name")?,
                        MigRevisionOrdinalMetaStoredRow {
                            tip_oid: col_get(row, 2, r, "revision_ordinal_meta", "tip_oid")?,
                            replace_sig: col_get(
                                row,
                                3,
                                r,
                                "revision_ordinal_meta",
                                "replace_sig",
                            )?,
                            max_ordinal: col_get(
                                row,
                                4,
                                r,
                                "revision_ordinal_meta",
                                "max_ordinal",
                            )?,
                            built_at: col_get(row, 5, r, "revision_ordinal_meta", "built_at")?,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_git::REVISION_ORDINAL_META_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open revision_ordinal_meta"))?;
                    for (ref_name, stored) in batch {
                        let bytes = encode_versioned(
                            redb_git::GIT_REVISION_ORDINAL_META_CODEC_VERSION,
                            stored,
                        )?;
                        tbl.insert(ref_name.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert revision_ordinal_meta"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("revision_ordinal_meta", n);
    }

    // 12..32: 21 RB-05 GitStateStore tables
    export_rb05_state_tables_to_redb(conn, db, counts, max_batch_rows).await?;

    // 33..61: 29 RB-05b AI / memory / governance / bridge tables
    export_rb05b_state_tables_to_redb(conn, db, counts, max_batch_rows).await?;

    Ok(())
}

async fn export_rb05_state_tables_to_redb(
    conn: &mut SqliteConnection,
    db: &RedbDatabase,
    counts: &mut BTreeMap<&'static str, u64>,
    max_batch_rows: &mut usize,
) -> StorageResult<()> {
    let r = DatabaseRole::Repository;
    let cv = redb_state::GIT_STATE_CODEC_VERSION;

    // 12. sequence_state
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "sequence_state").await?;
        let n = {
            require_columns(
                &cols,
                "sequence_state",
                &[
                    "kind",
                    "head_name",
                    "head_orig",
                    "current_oid",
                    "todo",
                    "payload",
                    "updated_at",
                ],
            )?;
            let select = if cols.contains("worktree_id") {
                "worktree_id, kind, head_name, head_orig, current_oid, todo, payload, updated_at"
            } else {
                "'', kind, head_name, head_orig, current_oid, todo, payload, updated_at"
            };
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "sequence_state",
                    select_cols: select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(SequenceStateRecord {
                        worktree_id: col_get(row, 1, r, "sequence_state", "worktree_id")?,
                        kind: col_get(row, 2, r, "sequence_state", "kind")?,
                        head_name: col_get(row, 3, r, "sequence_state", "head_name")?,
                        head_orig: col_get(row, 4, r, "sequence_state", "head_orig")?,
                        current_oid: col_get(row, 5, r, "sequence_state", "current_oid")?,
                        todo: col_get(row, 6, r, "sequence_state", "todo")?,
                        payload: col_get(row, 7, r, "sequence_state", "payload")?,
                        updated_at: col_get(row, 8, r, "sequence_state", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::SEQUENCE_STATE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open sequence_state"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.worktree_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert sequence_state"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("sequence_state", n);
    }

    // 13. rebase_state
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "rebase_state").await?;
        let n = {
            require_columns(
                &cols,
                "rebase_state",
                &[
                    "head_name",
                    "onto",
                    "orig_head",
                    "current_head",
                    "todo",
                    "done",
                    "stopped_sha",
                ],
            )?;
            let wt_expr = if cols.contains("worktree_id") {
                "worktree_id"
            } else {
                "''"
            };
            let ta_expr = if cols.contains("todo_actions") {
                "todo_actions"
            } else {
                "'[]'"
            };
            let as_expr = if cols.contains("autosquash") {
                "autosquash"
            } else {
                "0"
            };
            let em_expr = if cols.contains("empty_mode") {
                "empty_mode"
            } else {
                "'drop'"
            };
            let select = format!(
                "{wt_expr}, head_name, onto, orig_head, current_head, todo, {ta_expr}, done, stopped_sha, {as_expr}, {em_expr}"
            );
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "rebase_state",
                    select_cols: &select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let autosquash: i64 = col_get(row, 10, r, "rebase_state", "autosquash")?;
                    Ok(RebaseStateRecord {
                        worktree_id: col_get(row, 1, r, "rebase_state", "worktree_id")?,
                        head_name: col_get(row, 2, r, "rebase_state", "head_name")?,
                        onto: col_get(row, 3, r, "rebase_state", "onto")?,
                        orig_head: col_get(row, 4, r, "rebase_state", "orig_head")?,
                        current_head: col_get(row, 5, r, "rebase_state", "current_head")?,
                        todo: col_get(row, 6, r, "rebase_state", "todo")?,
                        todo_actions: col_get(row, 7, r, "rebase_state", "todo_actions")?,
                        done: col_get(row, 8, r, "rebase_state", "done")?,
                        stopped_sha: col_get(row, 9, r, "rebase_state", "stopped_sha")?,
                        autosquash: autosquash != 0,
                        empty_mode: col_get(row, 11, r, "rebase_state", "empty_mode")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::REBASE_STATE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open rebase_state"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.worktree_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert rebase_state"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("rebase_state", n);
    }

    // 14. bisect_state
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "bisect_state").await?;
        let n = {
            require_columns(
                &cols,
                "bisect_state",
                &[
                    "orig_head",
                    "orig_head_name",
                    "bad",
                    "good",
                    "current",
                    "skipped",
                    "steps",
                    "completed",
                    "first_parent",
                ],
            )?;
            let wt_expr = if cols.contains("worktree_id") {
                "worktree_id"
            } else {
                "''"
            };
            let select = format!(
                "{wt_expr}, orig_head, orig_head_name, bad, good, current, skipped, steps, completed, first_parent"
            );
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "bisect_state",
                    select_cols: &select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let completed: i64 = col_get(row, 9, r, "bisect_state", "completed")?;
                    let first_parent: i64 = col_get(row, 10, r, "bisect_state", "first_parent")?;
                    Ok(BisectStateRecord {
                        worktree_id: col_get(row, 1, r, "bisect_state", "worktree_id")?,
                        orig_head: col_get(row, 2, r, "bisect_state", "orig_head")?,
                        orig_head_name: col_get(row, 3, r, "bisect_state", "orig_head_name")?,
                        bad: col_get(row, 4, r, "bisect_state", "bad")?,
                        good: col_get(row, 5, r, "bisect_state", "good")?,
                        current: col_get(row, 6, r, "bisect_state", "current")?,
                        skipped: col_get(row, 7, r, "bisect_state", "skipped")?,
                        steps: col_get(row, 8, r, "bisect_state", "steps")?,
                        completed: completed != 0,
                        first_parent: first_parent != 0,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::BISECT_STATE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open bisect_state"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.worktree_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert bisect_state"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("bisect_state", n);
    }

    // 15. worktree_registry_capability
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, r, "worktree_registry_capability").await?;
        let n = {
            require_columns(&cols, "worktree_registry_capability", &["version"])?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "worktree_registry_capability",
                    select_cols: "version",
                    rowid_expr: "_rowid_",
                },
                |row| col_get::<i64>(row, 1, r, "worktree_registry_capability", "version"),
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::WORKTREE_REGISTRY_CAPABILITY_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open worktree_registry_capability"))?;
                    let marker = encode_versioned(cv, &MigUnitMarker)?;
                    for ver in batch {
                        tbl.insert(*ver, marker.as_slice()).map_err(|e| {
                            map_redb_storage_err(e, "insert worktree_registry_capability")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("worktree_registry_capability", n);
    }

    // 16. worktree_lifecycle
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "worktree_lifecycle").await?;
        let n = {
            require_columns(
                &cols,
                "worktree_lifecycle",
                &[
                    "worktree_id",
                    "state",
                    "path",
                    "reason",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "worktree_lifecycle",
                    select_cols: "worktree_id, state, path, reason, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let state_str: String = col_get(row, 2, r, "worktree_lifecycle", "state")?;
                    Ok(WorktreeLifecycleRecord {
                        worktree_id: col_get(row, 1, r, "worktree_lifecycle", "worktree_id")?,
                        state: WorktreeLifecycleState::parse(&state_str)?,
                        path: col_get(row, 3, r, "worktree_lifecycle", "path")?,
                        reason: col_get(row, 4, r, "worktree_lifecycle", "reason")?,
                        created_at: col_get(row, 5, r, "worktree_lifecycle", "created_at")?,
                        updated_at: col_get(row, 6, r, "worktree_lifecycle", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::WORKTREE_LIFECYCLE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open worktree_lifecycle"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.worktree_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert worktree_lifecycle"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("worktree_lifecycle", n);
    }

    // 17. worktree_intent_journal + git_state_sequences
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "worktree_intent_journal").await?;
        let mut max_id = read_sqlite_sequence_highwater(conn, r, "worktree_intent_journal").await?;
        let n = {
            require_columns(
                &cols,
                "worktree_intent_journal",
                &["id", "op", "worktree_id", "payload", "created_at"],
            )?;
            let stage_expr = if cols.contains("stage") {
                "stage"
            } else {
                "NULL"
            };
            let select = format!("id, op, worktree_id, payload, {stage_expr}, created_at");
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "worktree_intent_journal",
                    select_cols: &select,
                    rowid_expr: "id",
                },
                |row| {
                    let op_str: String = col_get(row, 2, r, "worktree_intent_journal", "op")?;
                    Ok(WorktreeIntentRecord {
                        id: col_get(row, 1, r, "worktree_intent_journal", "id")?,
                        op: WorktreeIntentOp::parse(&op_str)?,
                        worktree_id: col_get(row, 3, r, "worktree_intent_journal", "worktree_id")?,
                        payload: col_get(row, 4, r, "worktree_intent_journal", "payload")?,
                        stage: col_get(row, 5, r, "worktree_intent_journal", "stage")?,
                        created_at: col_get(row, 6, r, "worktree_intent_journal", "created_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::WORKTREE_INTENT_JOURNAL_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open worktree_intent_journal"))?;
                    for rec in batch {
                        if rec.id > max_id {
                            max_id = rec.id;
                        }
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.id, bytes.as_slice()).map_err(|e| {
                            map_redb_storage_err(e, "insert worktree_intent_journal")
                        })?;
                    }
                    let mut seq = wtx
                        .open_table(redb_state::GIT_STATE_SEQ_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open git_state_sequences"))?;
                    let seq_bytes = encode_versioned(cv, &MigSeqCounter { last_id: max_id })?;
                    seq.insert(MIG_SEQ_KEY_WORKTREE_INTENT, seq_bytes.as_slice())
                        .map_err(|e| map_redb_storage_err(e, "update worktree_intent sequence"))?;
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        persist_git_state_sequence_highwater(db, MIG_SEQ_KEY_WORKTREE_INTENT, max_id)?;
        counts.insert("worktree_intent_journal", n);
    }

    // 18. workspace_record
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "workspace_record").await?;
        let n = {
            require_columns(
                &cols,
                "workspace_record",
                &[
                    "workspace_id",
                    "repo_id",
                    "kind",
                    "worktree_id",
                    "path",
                    "owner_kind",
                    "owner_id",
                    "task_id",
                    "session_id",
                    "base_commit",
                    "branch",
                    "state",
                    "lease_owner",
                    "lease_fence",
                    "lease_expires_at",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "workspace_record",
                    select_cols: "workspace_id, repo_id, kind, worktree_id, path, owner_kind, owner_id, task_id, session_id, base_commit, branch, state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let kind_s: String = col_get(row, 3, r, "workspace_record", "kind")?;
                    let owner_s: String = col_get(row, 6, r, "workspace_record", "owner_kind")?;
                    let state_s: String = col_get(row, 12, r, "workspace_record", "state")?;
                    Ok(WorkspaceRecordRow {
                        workspace_id: col_get(row, 1, r, "workspace_record", "workspace_id")?,
                        repo_id: col_get(row, 2, r, "workspace_record", "repo_id")?,
                        kind: WorkspaceRecordKind::parse(&kind_s)?,
                        worktree_id: col_get(row, 4, r, "workspace_record", "worktree_id")?,
                        path: col_get(row, 5, r, "workspace_record", "path")?,
                        owner_kind: WorkspaceRecordOwnerKind::parse(&owner_s)?,
                        owner_id: col_get(row, 7, r, "workspace_record", "owner_id")?,
                        task_id: col_get(row, 8, r, "workspace_record", "task_id")?,
                        session_id: col_get(row, 9, r, "workspace_record", "session_id")?,
                        base_commit: col_get(row, 10, r, "workspace_record", "base_commit")?,
                        branch: col_get(row, 11, r, "workspace_record", "branch")?,
                        state: WorkspaceRecordState::parse(&state_s)?,
                        lease_owner: col_get(row, 13, r, "workspace_record", "lease_owner")?,
                        lease_fence: col_get(row, 14, r, "workspace_record", "lease_fence")?,
                        lease_expires_at: col_get(
                            row,
                            15,
                            r,
                            "workspace_record",
                            "lease_expires_at",
                        )?,
                        created_at: col_get(row, 16, r, "workspace_record", "created_at")?,
                        updated_at: col_get(row, 17, r, "workspace_record", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::WORKSPACE_RECORD_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open workspace_record"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.workspace_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert workspace_record"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("workspace_record", n);
    }

    // 19. operation
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "operation").await?;
        let n = {
            require_columns(
                &cols,
                "operation",
                &[
                    "op_id",
                    "repo_id",
                    "format_version",
                    "kind",
                    "status",
                    "command_name",
                    "description",
                    "args_digest",
                    "actor",
                    "worktree_id",
                    "scope_kind",
                    "scope_provenance",
                    "pre_view_oid",
                    "post_view_oid",
                    "restores_op_id",
                    "reverts_op_id",
                    "predecessor_map_oid",
                    "causal_context_id",
                    "restorable",
                    "control_slot",
                    "claim_owner",
                    "start_ts",
                    "end_ts",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "operation",
                    select_cols: "op_id, repo_id, format_version, kind, status, command_name, description, args_digest, actor, worktree_id, scope_kind, scope_provenance, pre_view_oid, post_view_oid, restores_op_id, reverts_op_id, predecessor_map_oid, causal_context_id, restorable, control_slot, claim_owner, start_ts, end_ts",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let sk_s: String = col_get(row, 11, r, "operation", "scope_kind")?;
                    let sp_s: String = col_get(row, 12, r, "operation", "scope_provenance")?;
                    let restorable: i64 = col_get(row, 19, r, "operation", "restorable")?;
                    Ok(OperationRecord {
                        op_id: col_get(row, 1, r, "operation", "op_id")?,
                        repo_id: col_get(row, 2, r, "operation", "repo_id")?,
                        format_version: col_get(row, 3, r, "operation", "format_version")?,
                        kind: col_get(row, 4, r, "operation", "kind")?,
                        status: col_get(row, 5, r, "operation", "status")?,
                        command_name: col_get(row, 6, r, "operation", "command_name")?,
                        description: col_get(row, 7, r, "operation", "description")?,
                        args_digest: col_get(row, 8, r, "operation", "args_digest")?,
                        actor: col_get(row, 9, r, "operation", "actor")?,
                        worktree_id: col_get(row, 10, r, "operation", "worktree_id")?,
                        scope_kind: OperationScopeKind::parse(&sk_s)?,
                        scope_provenance: OperationScopeProvenance::parse(&sp_s)?,
                        pre_view_oid: col_get(row, 13, r, "operation", "pre_view_oid")?,
                        post_view_oid: col_get(row, 14, r, "operation", "post_view_oid")?,
                        restores_op_id: col_get(row, 15, r, "operation", "restores_op_id")?,
                        reverts_op_id: col_get(row, 16, r, "operation", "reverts_op_id")?,
                        predecessor_map_oid: col_get(
                            row,
                            17,
                            r,
                            "operation",
                            "predecessor_map_oid",
                        )?,
                        causal_context_id: col_get(
                            row,
                            18,
                            r,
                            "operation",
                            "causal_context_id",
                        )?,
                        restorable: restorable != 0,
                        control_slot: col_get(row, 20, r, "operation", "control_slot")?,
                        claim_owner: col_get(row, 21, r, "operation", "claim_owner")?,
                        start_ts: col_get(row, 22, r, "operation", "start_ts")?,
                        end_ts: col_get(row, 23, r, "operation", "end_ts")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::OPERATION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open operation"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.op_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert operation"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("operation", n);
    }

    // 20. operation_parent
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "operation_parent").await?;
        let n = {
            require_columns(
                &cols,
                "operation_parent",
                &["op_id", "parent_op_id", "ordinal"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "operation_parent",
                    select_cols: "op_id, parent_op_id, ordinal",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok((
                        col_get::<String>(row, 1, r, "operation_parent", "op_id")?,
                        col_get::<String>(row, 2, r, "operation_parent", "parent_op_id")?,
                        col_get::<i64>(row, 3, r, "operation_parent", "ordinal")?,
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::OPERATION_PARENT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open operation_parent"))?;
                    for (op_id, parent_op_id, ordinal) in batch {
                        let bytes = encode_versioned(
                            cv,
                            &MigStringValue {
                                value: parent_op_id.clone(),
                            },
                        )?;
                        tbl.insert((op_id.as_str(), *ordinal), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert operation_parent"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("operation_parent", n);
    }

    // 21. operation_head
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "operation_head").await?;
        let n = {
            require_columns(
                &cols,
                "operation_head",
                &["repo_id", "scope_key", "op_id", "generation"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "operation_head",
                    select_cols: "repo_id, scope_key, op_id, generation",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let gen_i64: i64 = col_get(row, 4, r, "operation_head", "generation")?;
                    let generation = u64::try_from(gen_i64).map_err(|_| {
                        StorageBackendError::BackendInternal {
                            detail: "negative generation in sqlite `operation_head` table"
                                .to_string(),
                        }
                    })?;
                    Ok((
                        col_get::<String>(row, 1, r, "operation_head", "repo_id")?,
                        col_get::<String>(row, 2, r, "operation_head", "scope_key")?,
                        col_get::<String>(row, 3, r, "operation_head", "op_id")?,
                        generation,
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::OPERATION_HEAD_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open operation_head"))?;
                    for (repo_id, scope_key, op_id, generation) in batch {
                        let bytes = encode_versioned(cv, &MigU64Value { value: *generation })?;
                        tbl.insert(
                            (repo_id.as_str(), scope_key.as_str(), op_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert operation_head"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("operation_head", n);
    }

    // 22. operation_journal
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "operation_journal").await?;
        let n = {
            require_columns(
                &cols,
                "operation_journal",
                &[
                    "journal_id",
                    "op_id",
                    "phase",
                    "pre_view_oid",
                    "target_view_oid",
                    "owner",
                    "updated_at",
                    "recovery_payload",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "operation_journal",
                    select_cols: "journal_id, op_id, phase, pre_view_oid, target_view_oid, owner, updated_at, recovery_payload",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(OperationJournalRecord {
                        journal_id: col_get(row, 1, r, "operation_journal", "journal_id")?,
                        op_id: col_get(row, 2, r, "operation_journal", "op_id")?,
                        phase: col_get(row, 3, r, "operation_journal", "phase")?,
                        pre_view_oid: col_get(row, 4, r, "operation_journal", "pre_view_oid")?,
                        target_view_oid: col_get(
                            row,
                            5,
                            r,
                            "operation_journal",
                            "target_view_oid",
                        )?,
                        owner: col_get(row, 6, r, "operation_journal", "owner")?,
                        updated_at: col_get(row, 7, r, "operation_journal", "updated_at")?,
                        recovery_payload: col_get(
                            row,
                            8,
                            r,
                            "operation_journal",
                            "recovery_payload",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::OPERATION_JOURNAL_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open operation_journal"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.journal_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert operation_journal"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("operation_journal", n);
    }

    // 23. change_identity
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "change_identity").await?;
        let n = {
            require_columns(
                &cols,
                "change_identity",
                &[
                    "change_id",
                    "repo_id",
                    "origin",
                    "created_op_id",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "change_identity",
                    select_cols: "change_id, repo_id, origin, created_op_id, created_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(ChangeIdentityRecord {
                        change_id: col_get(row, 1, r, "change_identity", "change_id")?,
                        repo_id: col_get(row, 2, r, "change_identity", "repo_id")?,
                        origin: col_get(row, 3, r, "change_identity", "origin")?,
                        created_op_id: col_get(row, 4, r, "change_identity", "created_op_id")?,
                        created_at: col_get(row, 5, r, "change_identity", "created_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::CHANGE_IDENTITY_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open change_identity"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.change_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert change_identity"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("change_identity", n);
    }

    // 24. change_revision + change_revision_by_commit
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "change_revision").await?;
        let n = {
            require_columns(
                &cols,
                "change_revision",
                &[
                    "change_id",
                    "commit_oid",
                    "created_op_id",
                    "visibility",
                    "revision_ordinal",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "change_revision",
                    select_cols: "change_id, commit_oid, created_op_id, visibility, revision_ordinal",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(ChangeRevisionRecord {
                        change_id: col_get(row, 1, r, "change_revision", "change_id")?,
                        commit_oid: col_get(row, 2, r, "change_revision", "commit_oid")?,
                        created_op_id: col_get(row, 3, r, "change_revision", "created_op_id")?,
                        visibility: col_get(row, 4, r, "change_revision", "visibility")?,
                        revision_ordinal: col_get(
                            row,
                            5,
                            r,
                            "change_revision",
                            "revision_ordinal",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::CHANGE_REVISION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open change_revision"))?;
                    let mut idx = wtx
                        .open_table(redb_state::CHANGE_REVISION_BY_COMMIT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open change_revision_by_commit"))?;
                    let marker = encode_versioned(cv, &MigUnitMarker)?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.change_id.as_str(), rec.commit_oid.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert change_revision"))?;
                        idx.insert(
                            (rec.commit_oid.as_str(), rec.change_id.as_str()),
                            marker.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert change_revision_by_commit"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("change_revision", n);
    }

    // 25. change_predecessor
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "change_predecessor").await?;
        let n = {
            require_columns(
                &cols,
                "change_predecessor",
                &[
                    "successor_oid",
                    "predecessor_oid",
                    "op_id",
                    "relation_kind",
                    "ordinal",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "change_predecessor",
                    select_cols: "successor_oid, predecessor_oid, op_id, relation_kind, ordinal",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(ChangePredecessorRecord {
                        successor_oid: col_get(row, 1, r, "change_predecessor", "successor_oid")?,
                        predecessor_oid: col_get(
                            row,
                            2,
                            r,
                            "change_predecessor",
                            "predecessor_oid",
                        )?,
                        op_id: col_get(row, 3, r, "change_predecessor", "op_id")?,
                        relation_kind: col_get(row, 4, r, "change_predecessor", "relation_kind")?,
                        ordinal: col_get(row, 5, r, "change_predecessor", "ordinal")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::CHANGE_PREDECESSOR_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open change_predecessor"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.successor_oid.as_str(),
                                rec.predecessor_oid.as_str(),
                                rec.op_id.as_str(),
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert change_predecessor"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("change_predecessor", n);
    }

    // 26. ai_operation_link
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_operation_link").await?;
        let n = {
            require_columns(
                &cols,
                "ai_operation_link",
                &[
                    "operation_id",
                    "session_id",
                    "run_id",
                    "tool_invocation_id",
                    "intent_id",
                    "repo_id",
                    "worktree_id",
                    "workspace_id",
                    "lease_generation",
                    "config_provenance_digest",
                    "redaction_version",
                    "change_id",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_operation_link",
                    select_cols: "operation_id, session_id, run_id, tool_invocation_id, intent_id, repo_id, worktree_id, workspace_id, lease_generation, config_provenance_digest, redaction_version, change_id",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AiOperationLinkRecord {
                        operation_id: col_get(row, 1, r, "ai_operation_link", "operation_id")?,
                        session_id: col_get(row, 2, r, "ai_operation_link", "session_id")?,
                        run_id: col_get(row, 3, r, "ai_operation_link", "run_id")?,
                        tool_invocation_id: col_get(
                            row,
                            4,
                            r,
                            "ai_operation_link",
                            "tool_invocation_id",
                        )?,
                        intent_id: col_get(row, 5, r, "ai_operation_link", "intent_id")?,
                        repo_id: col_get(row, 6, r, "ai_operation_link", "repo_id")?,
                        worktree_id: col_get(row, 7, r, "ai_operation_link", "worktree_id")?,
                        workspace_id: col_get(row, 8, r, "ai_operation_link", "workspace_id")?,
                        lease_generation: col_get(
                            row,
                            9,
                            r,
                            "ai_operation_link",
                            "lease_generation",
                        )?,
                        config_provenance_digest: col_get(
                            row,
                            10,
                            r,
                            "ai_operation_link",
                            "config_provenance_digest",
                        )?,
                        redaction_version: col_get(
                            row,
                            11,
                            r,
                            "ai_operation_link",
                            "redaction_version",
                        )?,
                        change_id: col_get(row, 12, r, "ai_operation_link", "change_id")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_OPERATION_LINK_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_operation_link"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.operation_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert ai_operation_link"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_operation_link", n);
    }

    // 27. working_dirty
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "working_dirty").await?;
        let n = {
            require_columns(
                &cols,
                "working_dirty",
                &[
                    "worktree_id",
                    "path",
                    "kind",
                    "source",
                    "marked_at",
                    "verified_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "working_dirty",
                    select_cols: "worktree_id, path, kind, source, marked_at, verified_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(WorkingDirtyEntryRecord {
                        worktree_id: col_get(row, 1, r, "working_dirty", "worktree_id")?,
                        path: col_get(row, 2, r, "working_dirty", "path")?,
                        kind: col_get(row, 3, r, "working_dirty", "kind")?,
                        source: col_get(row, 4, r, "working_dirty", "source")?,
                        marked_at: col_get(row, 5, r, "working_dirty", "marked_at")?,
                        verified_at: col_get(row, 6, r, "working_dirty", "verified_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::WORKING_DIRTY_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open working_dirty"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.worktree_id.as_str(),
                                rec.path.as_str(),
                                rec.kind.as_str(),
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert working_dirty"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("working_dirty", n);
    }

    // 28. working_dirty_meta
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "working_dirty_meta").await?;
        let n = {
            require_columns(
                &cols,
                "working_dirty_meta",
                &[
                    "worktree_id",
                    "state",
                    "index_fingerprint",
                    "head_oid",
                    "scanned_at",
                    "scan_lock_pid",
                    "scan_lock_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "working_dirty_meta",
                    select_cols: "worktree_id, state, index_fingerprint, head_oid, scanned_at, scan_lock_pid, scan_lock_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(WorkingDirtyMetaRecord {
                        worktree_id: col_get(row, 1, r, "working_dirty_meta", "worktree_id")?,
                        state: col_get(row, 2, r, "working_dirty_meta", "state")?,
                        index_fingerprint: col_get(
                            row,
                            3,
                            r,
                            "working_dirty_meta",
                            "index_fingerprint",
                        )?,
                        head_oid: col_get(row, 4, r, "working_dirty_meta", "head_oid")?,
                        scanned_at: col_get(row, 5, r, "working_dirty_meta", "scanned_at")?,
                        scan_lock_pid: col_get(row, 6, r, "working_dirty_meta", "scan_lock_pid")?,
                        scan_lock_at: col_get(row, 7, r, "working_dirty_meta", "scan_lock_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::WORKING_DIRTY_META_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open working_dirty_meta"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.worktree_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert working_dirty_meta"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("working_dirty_meta", n);
    }

    // 29. layer
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "layer").await?;
        let n = {
            require_columns(
                &cols,
                "layer",
                &[
                    "name",
                    "source",
                    "priority",
                    "enabled",
                    "created_at",
                    "updated_at",
                ],
            )?;
            let wt_expr = if cols.contains("worktree_id") {
                "worktree_id"
            } else {
                "''"
            };
            let select =
                format!("{wt_expr}, name, source, priority, enabled, created_at, updated_at");
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "layer",
                    select_cols: &select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let enabled: i64 = col_get(row, 5, r, "layer", "enabled")?;
                    Ok(LayerRecord {
                        worktree_id: col_get(row, 1, r, "layer", "worktree_id")?,
                        name: col_get(row, 2, r, "layer", "name")?,
                        source: col_get(row, 3, r, "layer", "source")?,
                        priority: col_get(row, 4, r, "layer", "priority")?,
                        enabled: enabled != 0,
                        created_at: col_get(row, 6, r, "layer", "created_at")?,
                        updated_at: col_get(row, 7, r, "layer", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::LAYER_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open layer"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.worktree_id.as_str(), rec.name.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert layer"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("layer", n);
    }

    // 30. layer_path
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "layer_path").await?;
        let n = {
            require_columns(
                &cols,
                "layer_path",
                &["layer_name", "path", "content_hash", "materialized_at"],
            )?;
            let wt_expr = if cols.contains("worktree_id") {
                "worktree_id"
            } else {
                "''"
            };
            let select = format!("{wt_expr}, layer_name, path, content_hash, materialized_at");
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "layer_path",
                    select_cols: &select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(LayerPathRecord {
                        worktree_id: col_get(row, 1, r, "layer_path", "worktree_id")?,
                        layer_name: col_get(row, 2, r, "layer_path", "layer_name")?,
                        path: col_get(row, 3, r, "layer_path", "path")?,
                        content_hash: col_get(row, 4, r, "layer_path", "content_hash")?,
                        materialized_at: col_get(row, 5, r, "layer_path", "materialized_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::LAYER_PATH_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open layer_path"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.worktree_id.as_str(), rec.path.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert layer_path"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("layer_path", n);
    }

    // 31. sparse_view
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "sparse_view").await?;
        let n = {
            require_columns(&cols, "sparse_view", &["pattern", "ordinal"])?;
            let wt_expr = if cols.contains("worktree_id") {
                "worktree_id"
            } else {
                "''"
            };
            let select = format!("{wt_expr}, pattern, ordinal");
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "sparse_view",
                    select_cols: &select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok((
                        col_get::<String>(row, 1, r, "sparse_view", "worktree_id")?,
                        col_get::<String>(row, 2, r, "sparse_view", "pattern")?,
                        col_get::<i64>(row, 3, r, "sparse_view", "ordinal")?,
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::SPARSE_VIEW_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open sparse_view"))?;
                    for (wt, pattern, ordinal) in batch {
                        let bytes = encode_versioned(
                            cv,
                            &MigStringValue {
                                value: pattern.clone(),
                            },
                        )?;
                        tbl.insert((wt.as_str(), *ordinal), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert sparse_view"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("sparse_view", n);
    }

    // 32. sparse_view_meta
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "sparse_view_meta").await?;
        let n = {
            require_columns(&cols, "sparse_view_meta", &["worktree_id", "enabled"])?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "sparse_view_meta",
                    select_cols: "worktree_id, enabled",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let enabled: i64 = col_get(row, 2, r, "sparse_view_meta", "enabled")?;
                    Ok((
                        col_get::<String>(row, 1, r, "sparse_view_meta", "worktree_id")?,
                        enabled != 0,
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::SPARSE_VIEW_META_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open sparse_view_meta"))?;
                    for (wt, enabled) in batch {
                        let bytes = encode_versioned(cv, &MigBoolValue { value: *enabled })?;
                        tbl.insert(wt.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert sparse_view_meta"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("sparse_view_meta", n);
    }

    Ok(())
}

async fn export_rb05b_state_tables_to_redb(
    conn: &mut SqliteConnection,
    db: &RedbDatabase,
    counts: &mut BTreeMap<&'static str, u64>,
    max_batch_rows: &mut usize,
) -> StorageResult<()> {
    let r = DatabaseRole::Repository;
    let cv = redb_state::GIT_STATE_CODEC_VERSION;

    // 33. ai_thread
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_thread").await?;
        let n = {
            require_columns(
                &cols,
                "ai_thread",
                &[
                    "thread_id",
                    "title",
                    "owner_kind",
                    "owner_id",
                    "owner_display_name",
                    "current_intent_id",
                    "latest_intent_id",
                    "metadata_json",
                    "archived",
                    "version",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_thread",
                    select_cols: "thread_id, title, owner_kind, owner_id, owner_display_name, current_intent_id, latest_intent_id, metadata_json, archived, version, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let archived: i64 = col_get(row, 9, r, "ai_thread", "archived")?;
                    Ok(AiThreadRecord {
                        thread_id: col_get(row, 1, r, "ai_thread", "thread_id")?,
                        title: col_get(row, 2, r, "ai_thread", "title")?,
                        owner_kind: col_get(row, 3, r, "ai_thread", "owner_kind")?,
                        owner_id: col_get(row, 4, r, "ai_thread", "owner_id")?,
                        owner_display_name: col_get(
                            row,
                            5,
                            r,
                            "ai_thread",
                            "owner_display_name",
                        )?,
                        current_intent_id: col_get(row, 6, r, "ai_thread", "current_intent_id")?,
                        latest_intent_id: col_get(row, 7, r, "ai_thread", "latest_intent_id")?,
                        metadata_json: col_get(row, 8, r, "ai_thread", "metadata_json")?,
                        archived: archived != 0,
                        version: col_get(row, 10, r, "ai_thread", "version")?,
                        created_at: col_get(row, 11, r, "ai_thread", "created_at")?,
                        updated_at: col_get(row, 12, r, "ai_thread", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_THREAD_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_thread"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.thread_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert ai_thread"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_thread", n);
    }

    // 34. ai_thread_participant
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_thread_participant").await?;
        let n = {
            require_columns(
                &cols,
                "ai_thread_participant",
                &[
                    "thread_id",
                    "actor_kind",
                    "actor_id",
                    "actor_display_name",
                    "role",
                    "joined_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_thread_participant",
                    select_cols: "thread_id, actor_kind, actor_id, actor_display_name, role, joined_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AiThreadParticipantRecord {
                        thread_id: col_get(row, 1, r, "ai_thread_participant", "thread_id")?,
                        actor_kind: col_get(row, 2, r, "ai_thread_participant", "actor_kind")?,
                        actor_id: col_get(row, 3, r, "ai_thread_participant", "actor_id")?,
                        actor_display_name: col_get(
                            row,
                            4,
                            r,
                            "ai_thread_participant",
                            "actor_display_name",
                        )?,
                        role: col_get(row, 5, r, "ai_thread_participant", "role")?,
                        joined_at: col_get(row, 6, r, "ai_thread_participant", "joined_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_THREAD_PARTICIPANT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_thread_participant"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.thread_id.as_str(),
                                rec.actor_kind.as_str(),
                                rec.actor_id.as_str(),
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_thread_participant"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_thread_participant", n);
    }

    // 35. ai_thread_intent
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_thread_intent").await?;
        let n = {
            require_columns(
                &cols,
                "ai_thread_intent",
                &[
                    "thread_id",
                    "intent_id",
                    "ordinal",
                    "is_head",
                    "linked_at",
                    "link_reason",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_thread_intent",
                    select_cols: "thread_id, intent_id, ordinal, is_head, linked_at, link_reason",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let is_head: i64 = col_get(row, 4, r, "ai_thread_intent", "is_head")?;
                    Ok(AiThreadIntentRecord {
                        thread_id: col_get(row, 1, r, "ai_thread_intent", "thread_id")?,
                        intent_id: col_get(row, 2, r, "ai_thread_intent", "intent_id")?,
                        ordinal: col_get(row, 3, r, "ai_thread_intent", "ordinal")?,
                        is_head: is_head != 0,
                        linked_at: col_get(row, 5, r, "ai_thread_intent", "linked_at")?,
                        link_reason: col_get(row, 6, r, "ai_thread_intent", "link_reason")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_THREAD_INTENT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_thread_intent"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.thread_id.as_str(), rec.intent_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_thread_intent"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_thread_intent", n);
    }

    // 36. ai_thread_provider_metadata
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, r, "ai_thread_provider_metadata").await?;
        let n = {
            require_columns(
                &cols,
                "ai_thread_provider_metadata",
                &[
                    "thread_id",
                    "legacy_session_id",
                    "provider_thread_id",
                    "provider_kind",
                    "metadata_json",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_thread_provider_metadata",
                    select_cols: "thread_id, legacy_session_id, provider_thread_id, provider_kind, metadata_json, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AiThreadProviderMetadataRecord {
                        thread_id: col_get(
                            row,
                            1,
                            r,
                            "ai_thread_provider_metadata",
                            "thread_id",
                        )?,
                        legacy_session_id: col_get(
                            row,
                            2,
                            r,
                            "ai_thread_provider_metadata",
                            "legacy_session_id",
                        )?,
                        provider_thread_id: col_get(
                            row,
                            3,
                            r,
                            "ai_thread_provider_metadata",
                            "provider_thread_id",
                        )?,
                        provider_kind: col_get(
                            row,
                            4,
                            r,
                            "ai_thread_provider_metadata",
                            "provider_kind",
                        )?,
                        metadata_json: col_get(
                            row,
                            5,
                            r,
                            "ai_thread_provider_metadata",
                            "metadata_json",
                        )?,
                        updated_at: col_get(
                            row,
                            6,
                            r,
                            "ai_thread_provider_metadata",
                            "updated_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_THREAD_PROVIDER_METADATA_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_thread_provider_metadata"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.thread_id.as_str(), bytes.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert ai_thread_provider_metadata")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_thread_provider_metadata", n);
    }

    // 37. ai_scheduler_state
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_scheduler_state").await?;
        let n = {
            require_columns(
                &cols,
                "ai_scheduler_state",
                &[
                    "thread_id",
                    "selected_plan_id",
                    "active_task_id",
                    "active_run_id",
                    "metadata_json",
                    "version",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_scheduler_state",
                    select_cols: "thread_id, selected_plan_id, active_task_id, active_run_id, metadata_json, version, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AiSchedulerStateRecord {
                        thread_id: col_get(row, 1, r, "ai_scheduler_state", "thread_id")?,
                        selected_plan_id: col_get(
                            row,
                            2,
                            r,
                            "ai_scheduler_state",
                            "selected_plan_id",
                        )?,
                        active_task_id: col_get(
                            row,
                            3,
                            r,
                            "ai_scheduler_state",
                            "active_task_id",
                        )?,
                        active_run_id: col_get(row, 4, r, "ai_scheduler_state", "active_run_id")?,
                        metadata_json: col_get(row, 5, r, "ai_scheduler_state", "metadata_json")?,
                        version: col_get(row, 6, r, "ai_scheduler_state", "version")?,
                        updated_at: col_get(row, 7, r, "ai_scheduler_state", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_SCHEDULER_STATE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_scheduler_state"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.thread_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert ai_scheduler_state"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_scheduler_state", n);
    }

    // 38. ai_scheduler_plan_head
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_scheduler_plan_head").await?;
        let n = {
            require_columns(
                &cols,
                "ai_scheduler_plan_head",
                &["thread_id", "plan_id", "ordinal"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_scheduler_plan_head",
                    select_cols: "thread_id, plan_id, ordinal",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(AiSchedulerPlanHeadRecord {
                        thread_id: col_get(row, 1, r, "ai_scheduler_plan_head", "thread_id")?,
                        plan_id: col_get(row, 2, r, "ai_scheduler_plan_head", "plan_id")?,
                        ordinal: col_get(row, 3, r, "ai_scheduler_plan_head", "ordinal")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_SCHEDULER_PLAN_HEAD_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_scheduler_plan_head"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.thread_id.as_str(), rec.plan_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_scheduler_plan_head"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_scheduler_plan_head", n);
    }

    // 39. ai_scheduler_selected_plan
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, r, "ai_scheduler_selected_plan").await?;
        let n = {
            require_columns(
                &cols,
                "ai_scheduler_selected_plan",
                &["thread_id", "plan_id", "ordinal"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_scheduler_selected_plan",
                    select_cols: "thread_id, plan_id, ordinal",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(AiSchedulerSelectedPlanRecord {
                        thread_id: col_get(row, 1, r, "ai_scheduler_selected_plan", "thread_id")?,
                        plan_id: col_get(row, 2, r, "ai_scheduler_selected_plan", "plan_id")?,
                        ordinal: col_get(row, 3, r, "ai_scheduler_selected_plan", "ordinal")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_SCHEDULER_SELECTED_PLAN_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_scheduler_selected_plan"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.thread_id.as_str(), rec.plan_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| {
                            map_redb_storage_err(e, "insert ai_scheduler_selected_plan")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_scheduler_selected_plan", n);
    }

    // 40. ai_live_context_window
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_live_context_window").await?;
        let n = {
            require_columns(
                &cols,
                "ai_live_context_window",
                &[
                    "thread_id",
                    "context_frame_id",
                    "position",
                    "source_kind",
                    "pin_kind",
                    "inserted_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_live_context_window",
                    select_cols: "thread_id, context_frame_id, position, source_kind, pin_kind, inserted_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AiLiveContextWindowRecord {
                        thread_id: col_get(row, 1, r, "ai_live_context_window", "thread_id")?,
                        context_frame_id: col_get(
                            row,
                            2,
                            r,
                            "ai_live_context_window",
                            "context_frame_id",
                        )?,
                        position: col_get(row, 3, r, "ai_live_context_window", "position")?,
                        source_kind: col_get(row, 4, r, "ai_live_context_window", "source_kind")?,
                        pin_kind: col_get(row, 5, r, "ai_live_context_window", "pin_kind")?,
                        inserted_at: col_get(row, 6, r, "ai_live_context_window", "inserted_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_LIVE_CONTEXT_WINDOW_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_live_context_window"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.thread_id.as_str(), rec.context_frame_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_live_context_window"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_live_context_window", n);
    }

    // 41. ai_index_intent_plan
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_index_intent_plan").await?;
        let n = {
            require_columns(
                &cols,
                "ai_index_intent_plan",
                &["intent_id", "plan_id", "created_at"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_index_intent_plan",
                    select_cols: "intent_id, plan_id, created_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(AiIndexIntentPlanRecord {
                        intent_id: col_get(row, 1, r, "ai_index_intent_plan", "intent_id")?,
                        plan_id: col_get(row, 2, r, "ai_index_intent_plan", "plan_id")?,
                        created_at: col_get(row, 3, r, "ai_index_intent_plan", "created_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_INDEX_INTENT_PLAN_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_index_intent_plan"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.intent_id.as_str(), rec.plan_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_index_intent_plan"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_index_intent_plan", n);
    }

    // 42. ai_index_intent_task
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_index_intent_task").await?;
        let n = {
            require_columns(
                &cols,
                "ai_index_intent_task",
                &[
                    "intent_id",
                    "task_id",
                    "parent_task_id",
                    "origin_step_id",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_index_intent_task",
                    select_cols: "intent_id, task_id, parent_task_id, origin_step_id, created_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(AiIndexIntentTaskRecord {
                        intent_id: col_get(row, 1, r, "ai_index_intent_task", "intent_id")?,
                        task_id: col_get(row, 2, r, "ai_index_intent_task", "task_id")?,
                        parent_task_id: col_get(
                            row,
                            3,
                            r,
                            "ai_index_intent_task",
                            "parent_task_id",
                        )?,
                        origin_step_id: col_get(
                            row,
                            4,
                            r,
                            "ai_index_intent_task",
                            "origin_step_id",
                        )?,
                        created_at: col_get(row, 5, r, "ai_index_intent_task", "created_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_INDEX_INTENT_TASK_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_index_intent_task"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.intent_id.as_str(), rec.task_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_index_intent_task"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_index_intent_task", n);
    }

    // 43. ai_index_plan_step_task
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_index_plan_step_task").await?;
        let n = {
            require_columns(
                &cols,
                "ai_index_plan_step_task",
                &["plan_id", "task_id", "step_id", "created_at"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_index_plan_step_task",
                    select_cols: "plan_id, task_id, step_id, created_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(AiIndexPlanStepTaskRecord {
                        plan_id: col_get(row, 1, r, "ai_index_plan_step_task", "plan_id")?,
                        task_id: col_get(row, 2, r, "ai_index_plan_step_task", "task_id")?,
                        step_id: col_get(row, 3, r, "ai_index_plan_step_task", "step_id")?,
                        created_at: col_get(row, 4, r, "ai_index_plan_step_task", "created_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_INDEX_PLAN_STEP_TASK_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_index_plan_step_task"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.plan_id.as_str(), rec.task_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_index_plan_step_task"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_index_plan_step_task", n);
    }

    // 44. ai_index_task_run
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_index_task_run").await?;
        let n = {
            require_columns(
                &cols,
                "ai_index_task_run",
                &["task_id", "run_id", "is_latest", "created_at"],
            )?;
            let select = if cols.contains("base_commit_ref") {
                "task_id, run_id, is_latest, created_at, base_commit_ref"
            } else {
                "task_id, run_id, is_latest, created_at, NULL"
            };
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_index_task_run",
                    select_cols: select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let is_latest: i64 = col_get(row, 3, r, "ai_index_task_run", "is_latest")?;
                    Ok(AiIndexTaskRunRecord {
                        task_id: col_get(row, 1, r, "ai_index_task_run", "task_id")?,
                        run_id: col_get(row, 2, r, "ai_index_task_run", "run_id")?,
                        is_latest: is_latest != 0,
                        created_at: col_get(row, 4, r, "ai_index_task_run", "created_at")?,
                        base_commit_ref: col_get(
                            row,
                            5,
                            r,
                            "ai_index_task_run",
                            "base_commit_ref",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_INDEX_TASK_RUN_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_index_task_run"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.task_id.as_str(), rec.run_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_index_task_run"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_index_task_run", n);
    }

    // 45. ai_index_run_event
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_index_run_event").await?;
        let n = {
            require_columns(
                &cols,
                "ai_index_run_event",
                &[
                    "run_id",
                    "event_id",
                    "event_kind",
                    "is_latest",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_index_run_event",
                    select_cols: "run_id, event_id, event_kind, is_latest, created_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let is_latest: i64 = col_get(row, 4, r, "ai_index_run_event", "is_latest")?;
                    Ok(AiIndexRunEventRecord {
                        run_id: col_get(row, 1, r, "ai_index_run_event", "run_id")?,
                        event_id: col_get(row, 2, r, "ai_index_run_event", "event_id")?,
                        event_kind: col_get(row, 3, r, "ai_index_run_event", "event_kind")?,
                        is_latest: is_latest != 0,
                        created_at: col_get(row, 5, r, "ai_index_run_event", "created_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_INDEX_RUN_EVENT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_index_run_event"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.run_id.as_str(), rec.event_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_index_run_event"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_index_run_event", n);
    }

    // 46. ai_index_run_patchset
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_index_run_patchset").await?;
        let n = {
            require_columns(
                &cols,
                "ai_index_run_patchset",
                &[
                    "run_id",
                    "patchset_id",
                    "sequence",
                    "is_latest",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_index_run_patchset",
                    select_cols: "run_id, patchset_id, sequence, is_latest, created_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let is_latest: i64 = col_get(row, 4, r, "ai_index_run_patchset", "is_latest")?;
                    Ok(AiIndexRunPatchsetRecord {
                        run_id: col_get(row, 1, r, "ai_index_run_patchset", "run_id")?,
                        patchset_id: col_get(row, 2, r, "ai_index_run_patchset", "patchset_id")?,
                        sequence: col_get(row, 3, r, "ai_index_run_patchset", "sequence")?,
                        is_latest: is_latest != 0,
                        created_at: col_get(row, 5, r, "ai_index_run_patchset", "created_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_INDEX_RUN_PATCHSET_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_index_run_patchset"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.run_id.as_str(), rec.patchset_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert ai_index_run_patchset"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_index_run_patchset", n);
    }

    // 47. ai_index_intent_context_frame
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, r, "ai_index_intent_context_frame").await?;
        let n = {
            require_columns(
                &cols,
                "ai_index_intent_context_frame",
                &[
                    "intent_id",
                    "context_frame_id",
                    "relation_kind",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_index_intent_context_frame",
                    select_cols: "intent_id, context_frame_id, relation_kind, created_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(AiIndexIntentContextFrameRecord {
                        intent_id: col_get(
                            row,
                            1,
                            r,
                            "ai_index_intent_context_frame",
                            "intent_id",
                        )?,
                        context_frame_id: col_get(
                            row,
                            2,
                            r,
                            "ai_index_intent_context_frame",
                            "context_frame_id",
                        )?,
                        relation_kind: col_get(
                            row,
                            3,
                            r,
                            "ai_index_intent_context_frame",
                            "relation_kind",
                        )?,
                        created_at: col_get(
                            row,
                            4,
                            r,
                            "ai_index_intent_context_frame",
                            "created_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_INDEX_INTENT_CONTEXT_FRAME_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_index_intent_context_frame"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.intent_id.as_str(),
                                rec.context_frame_id.as_str(),
                                rec.relation_kind.as_str(),
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| {
                            map_redb_storage_err(e, "insert ai_index_intent_context_frame")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_index_intent_context_frame", n);
    }

    // 48. ai_validation_report
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_validation_report").await?;
        let n = {
            require_columns(
                &cols,
                "ai_validation_report",
                &[
                    "report_id",
                    "thread_id",
                    "run_id",
                    "policy_version",
                    "stale",
                    "is_latest",
                    "summary_json",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_validation_report",
                    select_cols: "report_id, thread_id, run_id, policy_version, stale, is_latest, summary_json, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let stale: i64 = col_get(row, 5, r, "ai_validation_report", "stale")?;
                    let is_latest: i64 = col_get(row, 6, r, "ai_validation_report", "is_latest")?;
                    Ok(AiValidationReportRecord {
                        report_id: col_get(row, 1, r, "ai_validation_report", "report_id")?,
                        thread_id: col_get(row, 2, r, "ai_validation_report", "thread_id")?,
                        run_id: col_get(row, 3, r, "ai_validation_report", "run_id")?,
                        policy_version: col_get(
                            row,
                            4,
                            r,
                            "ai_validation_report",
                            "policy_version",
                        )?,
                        stale: stale != 0,
                        is_latest: is_latest != 0,
                        summary_json: col_get(row, 7, r, "ai_validation_report", "summary_json")?,
                        created_at: col_get(row, 8, r, "ai_validation_report", "created_at")?,
                        updated_at: col_get(row, 9, r, "ai_validation_report", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_VALIDATION_REPORT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_validation_report"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.report_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert ai_validation_report"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_validation_report", n);
    }

    // 49. ai_risk_score_breakdown
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_risk_score_breakdown").await?;
        let n = {
            require_columns(
                &cols,
                "ai_risk_score_breakdown",
                &[
                    "breakdown_id",
                    "thread_id",
                    "validation_report_id",
                    "policy_version",
                    "stale",
                    "is_latest",
                    "summary_json",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_risk_score_breakdown",
                    select_cols: "breakdown_id, thread_id, validation_report_id, policy_version, stale, is_latest, summary_json, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let stale: i64 = col_get(row, 5, r, "ai_risk_score_breakdown", "stale")?;
                    let is_latest: i64 =
                        col_get(row, 6, r, "ai_risk_score_breakdown", "is_latest")?;
                    Ok(AiRiskScoreBreakdownRecord {
                        breakdown_id: col_get(
                            row,
                            1,
                            r,
                            "ai_risk_score_breakdown",
                            "breakdown_id",
                        )?,
                        thread_id: col_get(row, 2, r, "ai_risk_score_breakdown", "thread_id")?,
                        validation_report_id: col_get(
                            row,
                            3,
                            r,
                            "ai_risk_score_breakdown",
                            "validation_report_id",
                        )?,
                        policy_version: col_get(
                            row,
                            4,
                            r,
                            "ai_risk_score_breakdown",
                            "policy_version",
                        )?,
                        stale: stale != 0,
                        is_latest: is_latest != 0,
                        summary_json: col_get(
                            row,
                            7,
                            r,
                            "ai_risk_score_breakdown",
                            "summary_json",
                        )?,
                        created_at: col_get(row, 8, r, "ai_risk_score_breakdown", "created_at")?,
                        updated_at: col_get(row, 9, r, "ai_risk_score_breakdown", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_RISK_SCORE_BREAKDOWN_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_risk_score_breakdown"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.breakdown_id.as_str(), bytes.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert ai_risk_score_breakdown")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_risk_score_breakdown", n);
    }

    // 50. ai_decision_proposal
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_decision_proposal").await?;
        let n = {
            require_columns(
                &cols,
                "ai_decision_proposal",
                &[
                    "proposal_id",
                    "thread_id",
                    "validation_report_id",
                    "risk_score_breakdown_id",
                    "policy_version",
                    "stale",
                    "is_latest",
                    "summary_json",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_decision_proposal",
                    select_cols: "proposal_id, thread_id, validation_report_id, risk_score_breakdown_id, policy_version, stale, is_latest, summary_json, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let stale: i64 = col_get(row, 6, r, "ai_decision_proposal", "stale")?;
                    let is_latest: i64 = col_get(row, 7, r, "ai_decision_proposal", "is_latest")?;
                    Ok(AiDecisionProposalRecord {
                        proposal_id: col_get(row, 1, r, "ai_decision_proposal", "proposal_id")?,
                        thread_id: col_get(row, 2, r, "ai_decision_proposal", "thread_id")?,
                        validation_report_id: col_get(
                            row,
                            3,
                            r,
                            "ai_decision_proposal",
                            "validation_report_id",
                        )?,
                        risk_score_breakdown_id: col_get(
                            row,
                            4,
                            r,
                            "ai_decision_proposal",
                            "risk_score_breakdown_id",
                        )?,
                        policy_version: col_get(
                            row,
                            5,
                            r,
                            "ai_decision_proposal",
                            "policy_version",
                        )?,
                        stale: stale != 0,
                        is_latest: is_latest != 0,
                        summary_json: col_get(row, 8, r, "ai_decision_proposal", "summary_json")?,
                        created_at: col_get(row, 9, r, "ai_decision_proposal", "created_at")?,
                        updated_at: col_get(row, 10, r, "ai_decision_proposal", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_DECISION_PROPOSAL_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_decision_proposal"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.proposal_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert ai_decision_proposal"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_decision_proposal", n);
    }

    // 51. ai_final_decision
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "ai_final_decision").await?;
        let n = {
            require_columns(
                &cols,
                "ai_final_decision",
                &[
                    "decision_id",
                    "thread_id",
                    "decision_proposal_id",
                    "validation_report_id",
                    "policy_version",
                    "verdict",
                    "stale",
                    "is_latest",
                    "summary_json",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "ai_final_decision",
                    select_cols: "decision_id, thread_id, decision_proposal_id, validation_report_id, policy_version, verdict, stale, is_latest, summary_json, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let stale: i64 = col_get(row, 7, r, "ai_final_decision", "stale")?;
                    let is_latest: i64 = col_get(row, 8, r, "ai_final_decision", "is_latest")?;
                    Ok(AiFinalDecisionRecord {
                        decision_id: col_get(row, 1, r, "ai_final_decision", "decision_id")?,
                        thread_id: col_get(row, 2, r, "ai_final_decision", "thread_id")?,
                        decision_proposal_id: col_get(
                            row,
                            3,
                            r,
                            "ai_final_decision",
                            "decision_proposal_id",
                        )?,
                        validation_report_id: col_get(
                            row,
                            4,
                            r,
                            "ai_final_decision",
                            "validation_report_id",
                        )?,
                        policy_version: col_get(
                            row,
                            5,
                            r,
                            "ai_final_decision",
                            "policy_version",
                        )?,
                        verdict: col_get(row, 6, r, "ai_final_decision", "verdict")?,
                        stale: stale != 0,
                        is_latest: is_latest != 0,
                        summary_json: col_get(row, 9, r, "ai_final_decision", "summary_json")?,
                        created_at: col_get(row, 10, r, "ai_final_decision", "created_at")?,
                        updated_at: col_get(row, 11, r, "ai_final_decision", "updated_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AI_FINAL_DECISION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open ai_final_decision"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.decision_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert ai_final_decision"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("ai_final_decision", n);
    }

    // 52. approved_permission
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "approved_permission").await?;
        let n = {
            require_columns(
                &cols,
                "approved_permission",
                &[
                    "project_id",
                    "permission",
                    "pattern",
                    "created_at",
                    "source_worktree_id",
                    "source_session_id",
                    "source_workspace_id",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "approved_permission",
                    select_cols: "project_id, permission, pattern, created_at, source_worktree_id, source_session_id, source_workspace_id",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(ApprovedPermissionRecord {
                        project_id: col_get(row, 1, r, "approved_permission", "project_id")?,
                        permission: col_get(row, 2, r, "approved_permission", "permission")?,
                        pattern: col_get(row, 3, r, "approved_permission", "pattern")?,
                        created_at: col_get(row, 4, r, "approved_permission", "created_at")?,
                        source_worktree_id: col_get(
                            row,
                            5,
                            r,
                            "approved_permission",
                            "source_worktree_id",
                        )?,
                        source_session_id: col_get(
                            row,
                            6,
                            r,
                            "approved_permission",
                            "source_session_id",
                        )?,
                        source_workspace_id: col_get(
                            row,
                            7,
                            r,
                            "approved_permission",
                            "source_workspace_id",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::APPROVED_PERMISSION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open approved_permission"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.project_id.as_str(),
                                rec.permission.as_str(),
                                rec.pattern.as_str(),
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert approved_permission"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("approved_permission", n);
    }

    // 53. automation_log
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "automation_log").await?;
        let n = {
            require_columns(
                &cols,
                "automation_log",
                &[
                    "id",
                    "rule_id",
                    "trigger_kind",
                    "action_kind",
                    "status",
                    "message",
                    "started_at",
                    "finished_at",
                    "details_json",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "automation_log",
                    select_cols: "id, rule_id, trigger_kind, action_kind, status, message, started_at, finished_at, details_json",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AutomationLogRecord {
                        id: col_get(row, 1, r, "automation_log", "id")?,
                        rule_id: col_get(row, 2, r, "automation_log", "rule_id")?,
                        trigger_kind: col_get(row, 3, r, "automation_log", "trigger_kind")?,
                        action_kind: col_get(row, 4, r, "automation_log", "action_kind")?,
                        status: col_get(row, 5, r, "automation_log", "status")?,
                        message: col_get(row, 6, r, "automation_log", "message")?,
                        started_at: col_get(row, 7, r, "automation_log", "started_at")?,
                        finished_at: col_get(row, 8, r, "automation_log", "finished_at")?,
                        details_json: col_get(row, 9, r, "automation_log", "details_json")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AUTOMATION_LOG_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open automation_log"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert automation_log"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("automation_log", n);
    }

    // 54. source_call_log
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "source_call_log").await?;
        let n = {
            require_columns(
                &cols,
                "source_call_log",
                &[
                    "id",
                    "session_id",
                    "source_slug",
                    "tool_name",
                    "registered_tool_name",
                    "tool_call_id",
                    "credential_ref",
                    "latency_ms",
                    "input_bytes",
                    "output_bytes",
                    "cost_estimate_micros",
                    "approval_decision",
                    "state_namespace",
                    "success",
                    "created_at",
                    "agent_run_id",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "source_call_log",
                    select_cols: "id, session_id, source_slug, tool_name, registered_tool_name, tool_call_id, credential_ref, latency_ms, input_bytes, output_bytes, cost_estimate_micros, approval_decision, state_namespace, success, created_at, agent_run_id",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let success: i64 = col_get(row, 14, r, "source_call_log", "success")?;
                    Ok(SourceCallLogRecord {
                        id: col_get(row, 1, r, "source_call_log", "id")?,
                        session_id: col_get(row, 2, r, "source_call_log", "session_id")?,
                        source_slug: col_get(row, 3, r, "source_call_log", "source_slug")?,
                        tool_name: col_get(row, 4, r, "source_call_log", "tool_name")?,
                        registered_tool_name: col_get(
                            row,
                            5,
                            r,
                            "source_call_log",
                            "registered_tool_name",
                        )?,
                        tool_call_id: col_get(row, 6, r, "source_call_log", "tool_call_id")?,
                        credential_ref: col_get(row, 7, r, "source_call_log", "credential_ref")?,
                        latency_ms: col_get(row, 8, r, "source_call_log", "latency_ms")?,
                        input_bytes: col_get(row, 9, r, "source_call_log", "input_bytes")?,
                        output_bytes: col_get(row, 10, r, "source_call_log", "output_bytes")?,
                        cost_estimate_micros: col_get(
                            row,
                            11,
                            r,
                            "source_call_log",
                            "cost_estimate_micros",
                        )?,
                        approval_decision: col_get(
                            row,
                            12,
                            r,
                            "source_call_log",
                            "approval_decision",
                        )?,
                        state_namespace: col_get(
                            row,
                            13,
                            r,
                            "source_call_log",
                            "state_namespace",
                        )?,
                        success: success != 0,
                        created_at: col_get(row, 15, r, "source_call_log", "created_at")?,
                        agent_run_id: col_get(row, 16, r, "source_call_log", "agent_run_id")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::SOURCE_CALL_LOG_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open source_call_log"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert source_call_log"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("source_call_log", n);
    }

    // 55. memory_episode
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "memory_episode").await?;
        let n = {
            require_columns(
                &cols,
                "memory_episode",
                &[
                    "episode_id",
                    "repo_id",
                    "source_kind",
                    "source_key",
                    "outcome",
                    "actor",
                    "started_at",
                    "ended_at",
                    "anchor_commit",
                    "change_id",
                    "title",
                    "body",
                    "content_digest",
                    "producer",
                    "rules_version",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "memory_episode",
                    select_cols: "episode_id, repo_id, source_kind, source_key, outcome, actor, started_at, ended_at, anchor_commit, change_id, title, body, content_digest, producer, rules_version",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let sk_s: String = col_get(row, 3, r, "memory_episode", "source_kind")?;
                    let oc_s: String = col_get(row, 5, r, "memory_episode", "outcome")?;
                    Ok(MemoryEpisodeRecord {
                        episode_id: col_get(row, 1, r, "memory_episode", "episode_id")?,
                        repo_id: col_get(row, 2, r, "memory_episode", "repo_id")?,
                        source_kind: MemorySourceKind::parse(&sk_s)?,
                        source_key: col_get(row, 4, r, "memory_episode", "source_key")?,
                        outcome: MemoryOutcome::parse(&oc_s)?,
                        actor: col_get(row, 6, r, "memory_episode", "actor")?,
                        started_at: col_get(row, 7, r, "memory_episode", "started_at")?,
                        ended_at: col_get(row, 8, r, "memory_episode", "ended_at")?,
                        anchor_commit: col_get(row, 9, r, "memory_episode", "anchor_commit")?,
                        change_id: col_get(row, 10, r, "memory_episode", "change_id")?,
                        title: col_get(row, 11, r, "memory_episode", "title")?,
                        body: col_get(row, 12, r, "memory_episode", "body")?,
                        content_digest: col_get(row, 13, r, "memory_episode", "content_digest")?,
                        producer: col_get(row, 14, r, "memory_episode", "producer")?,
                        rules_version: col_get(row, 15, r, "memory_episode", "rules_version")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::MEMORY_EPISODE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open memory_episode"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.episode_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert memory_episode"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("memory_episode", n);
    }

    // 56. memory_episode_evidence
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "memory_episode_evidence").await?;
        let n = {
            require_columns(
                &cols,
                "memory_episode_evidence",
                &[
                    "episode_id",
                    "ordinal",
                    "kind",
                    "ref_id",
                    "link_confidence",
                    "resolution_status",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "memory_episode_evidence",
                    select_cols: "episode_id, ordinal, kind, ref_id, link_confidence, resolution_status",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let kind_s: String = col_get(row, 3, r, "memory_episode_evidence", "kind")?;
                    let conf_s: String =
                        col_get(row, 5, r, "memory_episode_evidence", "link_confidence")?;
                    let res_s: String =
                        col_get(row, 6, r, "memory_episode_evidence", "resolution_status")?;
                    Ok(MemoryEpisodeEvidenceRecord {
                        episode_id: col_get(row, 1, r, "memory_episode_evidence", "episode_id")?,
                        ordinal: col_get(row, 2, r, "memory_episode_evidence", "ordinal")?,
                        kind: MemoryEvidenceKind::parse(&kind_s)?,
                        ref_id: col_get(row, 4, r, "memory_episode_evidence", "ref_id")?,
                        link_confidence: MemoryLinkConfidence::parse(&conf_s)?,
                        resolution_status: MemoryResolutionStatus::parse(&res_s)?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::MEMORY_EPISODE_EVIDENCE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open memory_episode_evidence"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert((rec.episode_id.as_str(), rec.ordinal), bytes.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert memory_episode_evidence")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("memory_episode_evidence", n);
    }

    // 57. memory_projection_state
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "memory_projection_state").await?;
        let n = {
            require_columns(
                &cols,
                "memory_projection_state",
                &[
                    "repo_id",
                    "source_kind",
                    "cursor_json",
                    "fingerprint",
                    "rules_version",
                    "schema_version",
                    "horizon_truncated",
                    "revoked_count",
                    "aged_out_count",
                    "rebuilt_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "memory_projection_state",
                    select_cols: "repo_id, source_kind, cursor_json, fingerprint, rules_version, schema_version, horizon_truncated, revoked_count, aged_out_count, rebuilt_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let sk_s: String =
                        col_get(row, 2, r, "memory_projection_state", "source_kind")?;
                    let trunc: i64 =
                        col_get(row, 7, r, "memory_projection_state", "horizon_truncated")?;
                    Ok(MemoryProjectionStateRecord {
                        repo_id: col_get(row, 1, r, "memory_projection_state", "repo_id")?,
                        source_kind: MemorySourceKind::parse(&sk_s)?,
                        cursor_json: col_get(row, 3, r, "memory_projection_state", "cursor_json")?,
                        fingerprint: col_get(row, 4, r, "memory_projection_state", "fingerprint")?,
                        rules_version: col_get(
                            row,
                            5,
                            r,
                            "memory_projection_state",
                            "rules_version",
                        )?,
                        schema_version: col_get(
                            row,
                            6,
                            r,
                            "memory_projection_state",
                            "schema_version",
                        )?,
                        horizon_truncated: trunc != 0,
                        revoked_count: col_get(
                            row,
                            8,
                            r,
                            "memory_projection_state",
                            "revoked_count",
                        )?,
                        aged_out_count: col_get(
                            row,
                            9,
                            r,
                            "memory_projection_state",
                            "aged_out_count",
                        )?,
                        rebuilt_at: col_get(row, 10, r, "memory_projection_state", "rebuilt_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::MEMORY_PROJECTION_STATE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open memory_projection_state"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.repo_id.as_str(), rec.source_kind.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert memory_projection_state"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("memory_projection_state", n);
    }

    // 58. memory_episode_path + MEMORY_BY_PATH
    {
        let cols = require_sqlite_table_columns_on_conn(conn, r, "memory_episode_path").await?;
        let n = {
            require_columns(
                &cols,
                "memory_episode_path",
                &[
                    "episode_id",
                    "code_path",
                    "change_kind",
                    "blob_oid_at_end",
                    "ended_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "memory_episode_path",
                    select_cols: "episode_id, code_path, change_kind, blob_oid_at_end, ended_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let ck_s: String = col_get(row, 3, r, "memory_episode_path", "change_kind")?;
                    Ok(MemoryEpisodePathRecord {
                        episode_id: col_get(row, 1, r, "memory_episode_path", "episode_id")?,
                        code_path: col_get(row, 2, r, "memory_episode_path", "code_path")?,
                        change_kind: MemoryPathChangeKind::parse(&ck_s)?,
                        blob_oid_at_end: col_get(
                            row,
                            4,
                            r,
                            "memory_episode_path",
                            "blob_oid_at_end",
                        )?,
                        ended_at: col_get(row, 5, r, "memory_episode_path", "ended_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::MEMORY_EPISODE_PATH_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open memory_episode_path"))?;
                    let mut idx = wtx
                        .open_table(redb_state::MEMORY_BY_PATH_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open MEMORY_BY_PATH"))?;
                    let marker = encode_versioned(cv, &MigUnitMarker)?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.episode_id.as_str(), rec.code_path.as_slice()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert memory_episode_path"))?;
                        idx.insert(
                            (
                                rec.code_path.as_slice(),
                                invert_ended_at(rec.ended_at),
                                rec.episode_id.as_str(),
                            ),
                            marker.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert MEMORY_BY_PATH"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("memory_episode_path", n);
    }

    // 59. memory_episode_search_doc
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, r, "memory_episode_search_doc").await?;
        let n = {
            require_columns(
                &cols,
                "memory_episode_search_doc",
                &["episode_id", "title", "body", "paths_text"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "memory_episode_search_doc",
                    select_cols: "episode_id, title, body, paths_text",
                    rowid_expr: "rowid",
                },
                |row| {
                    Ok(MemoryEpisodeSearchDocRecord {
                        rowid: col_get(row, 0, r, "memory_episode_search_doc", "rowid")?,
                        episode_id: col_get(row, 1, r, "memory_episode_search_doc", "episode_id")?,
                        title: col_get(row, 2, r, "memory_episode_search_doc", "title")?,
                        body: col_get(row, 3, r, "memory_episode_search_doc", "body")?,
                        paths_text: col_get(row, 4, r, "memory_episode_search_doc", "paths_text")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::MEMORY_EPISODE_SEARCH_DOC_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open memory_episode_search_doc"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.rowid, bytes.as_slice()).map_err(|e| {
                            map_redb_storage_err(e, "insert memory_episode_search_doc")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("memory_episode_search_doc", n);
    }

    // 60. agent_bridge_publication (shared with RedbGitStore)
    {
        let cols = sqlite_table_columns_on_conn(conn, r, "agent_bridge_publication").await?;
        let n = if cols.is_empty() {
            0
        } else {
            require_columns(
                &cols,
                "agent_bridge_publication",
                &["operation_id", "commit_oid"],
            )?;
            let select = if cols.contains("phase") {
                "operation_id, commit_oid, phase"
            } else {
                "operation_id, commit_oid, 'published'"
            };
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "agent_bridge_publication",
                    select_cols: select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let phase_s: String = col_get(row, 3, r, "agent_bridge_publication", "phase")?;
                    Ok((
                        col_get::<String>(row, 1, r, "agent_bridge_publication", "operation_id")?,
                        MigBridgePublicationStoredRow {
                            commit_oid: col_get(
                                row,
                                2,
                                r,
                                "agent_bridge_publication",
                                "commit_oid",
                            )?,
                            phase: parse_bridge_publication_phase(&phase_s)?,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_git::BRIDGE_PUBLICATION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_bridge_publication"))?;
                    for (op_id, stored) in batch {
                        let bytes =
                            encode_versioned(redb_git::GIT_BRIDGE_PUB_CODEC_VERSION, stored)?;
                        tbl.insert(op_id.as_str(), bytes.as_slice()).map_err(|e| {
                            map_redb_storage_err(e, "insert agent_bridge_publication")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_bridge_publication", n);
    }

    // 61. agent_bridge_index_publication
    {
        let cols = sqlite_table_columns_on_conn(conn, r, "agent_bridge_index_publication").await?;
        let n = if cols.is_empty() {
            0
        } else {
            require_columns(
                &cols,
                "agent_bridge_index_publication",
                &["operation_id", "index_digest", "phase"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role: r,
                    target_role: r,
                    table: "agent_bridge_index_publication",
                    select_cols: "operation_id, index_digest, phase",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let phase_s: String =
                        col_get(row, 3, r, "agent_bridge_index_publication", "phase")?;
                    Ok((
                        col_get::<String>(
                            row,
                            1,
                            r,
                            "agent_bridge_index_publication",
                            "operation_id",
                        )?,
                        MigBridgeIndexPublicationStoredRow {
                            index_digest: col_get(
                                row,
                                2,
                                r,
                                "agent_bridge_index_publication",
                                "index_digest",
                            )?,
                            phase: parse_bridge_publication_phase(&phase_s)?,
                        },
                    ))
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_state::AGENT_BRIDGE_INDEX_PUBLICATION_TABLE)
                        .map_err(|e| {
                            map_redb_table_err(e, "open agent_bridge_index_publication")
                        })?;
                    for (op_id, stored) in batch {
                        let bytes = encode_versioned(cv, stored)?;
                        tbl.insert(op_id.as_str(), bytes.as_slice()).map_err(|e| {
                            map_redb_storage_err(e, "insert agent_bridge_index_publication")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_bridge_index_publication", n);
    }

    Ok(())
}

// ============================================================================
// Export All 23 (Layout A) or 25 (Layouts B/C) Capture Source Tables
// ============================================================================

struct CaptureExportOptions {
    source_role: DatabaseRole,
    include_capture_ledger_and_receipt: bool,
    seeded_schema_versions_count: u64,
    /// Minimum high-water marks read from inline `libra.db` `sqlite_sequence`
    /// in Layout B so capture sequences never move backwards even when the
    /// legacy catalog copy did not copy `sqlite_sequence`.
    inline_bridge_event_seq_floor: i64,
    inline_bridge_link_seq_floor: i64,
}

async fn export_capture_tables_to_redb(
    conn: &mut SqliteConnection,
    opts: CaptureExportOptions,
    db: &RedbDatabase,
    counts: &mut BTreeMap<&'static str, u64>,
    max_batch_rows: &mut usize,
) -> StorageResult<()> {
    let CaptureExportOptions {
        source_role,
        include_capture_ledger_and_receipt,
        seeded_schema_versions_count,
        inline_bridge_event_seq_floor,
        inline_bridge_link_seq_floor,
    } = opts;
    let tr = DatabaseRole::Capture;
    let cv = redb_cap::CAPTURE_CODEC_VERSION;

    // 1. schema_versions (Capture ledger — Layout B/C only; Layout A seeded initial receipt)
    if include_capture_ledger_and_receipt {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "schema_versions").await?;
        let n = {
            require_columns(&cols, "schema_versions", &["version", "name", "applied_at"])?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "schema_versions",
                    select_cols: "version, name, applied_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(CaptureSchemaVersionRecord {
                        version: col_get(row, 1, source_role, "schema_versions", "version")?,
                        name: col_get(row, 2, source_role, "schema_versions", "name")?,
                        applied_at: col_get(row, 3, source_role, "schema_versions", "applied_at")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::SCHEMA_VERSIONS_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open capture schema_versions"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.version, bytes.as_slice()).map_err(|e| {
                            map_redb_storage_err(e, "insert capture schema_versions")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("schema_versions", n);
    } else {
        counts.insert("schema_versions", seeded_schema_versions_count);
    }

    // 2. capture_catalog_copy_receipt (Layout B/C only; Layout A has 0 rows)
    if include_capture_ledger_and_receipt {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "capture_catalog_copy_receipt")
                .await?;
        let n = {
            require_columns(&cols, "capture_catalog_copy_receipt", &["id", "copied_at"])?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "capture_catalog_copy_receipt",
                    select_cols: "id, copied_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(CaptureCatalogCopyReceiptRecord {
                        id: col_get(row, 1, source_role, "capture_catalog_copy_receipt", "id")?,
                        copied_at: col_get(
                            row,
                            2,
                            source_role,
                            "capture_catalog_copy_receipt",
                            "copied_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::CAPTURE_CATALOG_COPY_RECEIPT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open capture_catalog_copy_receipt"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.id, bytes.as_slice()).map_err(|e| {
                            map_redb_storage_err(e, "insert capture_catalog_copy_receipt")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("capture_catalog_copy_receipt", n);
    } else {
        counts.insert("capture_catalog_copy_receipt", 0);
    }

    // 3. agent_audit_log
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_audit_log").await?;
        let n = {
            require_columns(
                &cols,
                "agent_audit_log",
                &[
                    "audit_id",
                    "timestamp",
                    "user_id",
                    "user_name",
                    "action",
                    "checkpoint_id",
                    "scope",
                    "export_path",
                    "justification",
                    "granted",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_audit_log",
                    select_cols: "audit_id, timestamp, user_id, user_name, action, checkpoint_id, scope, export_path, justification, granted",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let granted: i64 = col_get(row, 10, source_role, "agent_audit_log", "granted")?;
                    Ok(AgentAuditLogRecord {
                        audit_id: col_get(row, 1, source_role, "agent_audit_log", "audit_id")?,
                        timestamp: col_get(row, 2, source_role, "agent_audit_log", "timestamp")?,
                        user_id: col_get(row, 3, source_role, "agent_audit_log", "user_id")?,
                        user_name: col_get(row, 4, source_role, "agent_audit_log", "user_name")?,
                        action: col_get(row, 5, source_role, "agent_audit_log", "action")?,
                        checkpoint_id: col_get(
                            row,
                            6,
                            source_role,
                            "agent_audit_log",
                            "checkpoint_id",
                        )?,
                        scope: col_get(row, 7, source_role, "agent_audit_log", "scope")?,
                        export_path: col_get(
                            row,
                            8,
                            source_role,
                            "agent_audit_log",
                            "export_path",
                        )?,
                        justification: col_get(
                            row,
                            9,
                            source_role,
                            "agent_audit_log",
                            "justification",
                        )?,
                        granted: granted != 0,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_AUDIT_LOG_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_audit_log"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.audit_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_audit_log"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_audit_log", n);
    }

    // 4. agent_bridge_session
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_bridge_session").await?;
        let n = {
            require_columns(
                &cols,
                "agent_bridge_session",
                &[
                    "bridge_session_id",
                    "source",
                    "repository_id",
                    "worktree_id",
                    "workspace_id",
                    "parent_session_id",
                    "agent_id",
                    "session_state",
                    "schema_version",
                    "last_event_seq",
                    "last_acked_seq",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_bridge_session",
                    select_cols: "bridge_session_id, source, repository_id, worktree_id, workspace_id, parent_session_id, agent_id, session_state, schema_version, last_event_seq, last_acked_seq, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let st_s: String =
                        col_get(row, 8, source_role, "agent_bridge_session", "session_state")?;
                    Ok(AgentBridgeSessionRecord {
                        bridge_session_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_bridge_session",
                            "bridge_session_id",
                        )?,
                        source: col_get(row, 2, source_role, "agent_bridge_session", "source")?,
                        repository_id: col_get(
                            row,
                            3,
                            source_role,
                            "agent_bridge_session",
                            "repository_id",
                        )?,
                        worktree_id: col_get(
                            row,
                            4,
                            source_role,
                            "agent_bridge_session",
                            "worktree_id",
                        )?,
                        workspace_id: col_get(
                            row,
                            5,
                            source_role,
                            "agent_bridge_session",
                            "workspace_id",
                        )?,
                        parent_session_id: col_get(
                            row,
                            6,
                            source_role,
                            "agent_bridge_session",
                            "parent_session_id",
                        )?,
                        agent_id: col_get(
                            row,
                            7,
                            source_role,
                            "agent_bridge_session",
                            "agent_id",
                        )?,
                        session_state: BridgeSessionState::parse(&st_s)?,
                        schema_version: col_get(
                            row,
                            9,
                            source_role,
                            "agent_bridge_session",
                            "schema_version",
                        )?,
                        last_event_seq: col_get(
                            row,
                            10,
                            source_role,
                            "agent_bridge_session",
                            "last_event_seq",
                        )?,
                        last_acked_seq: col_get(
                            row,
                            11,
                            source_role,
                            "agent_bridge_session",
                            "last_acked_seq",
                        )?,
                        created_at: col_get(
                            row,
                            12,
                            source_role,
                            "agent_bridge_session",
                            "created_at",
                        )?,
                        updated_at: col_get(
                            row,
                            13,
                            source_role,
                            "agent_bridge_session",
                            "updated_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_BRIDGE_SESSION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_bridge_session"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.bridge_session_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_bridge_session"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_bridge_session", n);
    }

    // 5. agent_bridge_checkpoint
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_bridge_checkpoint")
                .await?;
        let n = {
            require_columns(
                &cols,
                "agent_bridge_checkpoint",
                &[
                    "checkpoint_id",
                    "bridge_session_id",
                    "agent_checkpoint_id",
                    "target_oid",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_bridge_checkpoint",
                    select_cols: "checkpoint_id, bridge_session_id, agent_checkpoint_id, target_oid, created_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AgentBridgeCheckpointRecord {
                        checkpoint_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_bridge_checkpoint",
                            "checkpoint_id",
                        )?,
                        bridge_session_id: col_get(
                            row,
                            2,
                            source_role,
                            "agent_bridge_checkpoint",
                            "bridge_session_id",
                        )?,
                        agent_checkpoint_id: col_get(
                            row,
                            3,
                            source_role,
                            "agent_bridge_checkpoint",
                            "agent_checkpoint_id",
                        )?,
                        target_oid: col_get(
                            row,
                            4,
                            source_role,
                            "agent_bridge_checkpoint",
                            "target_oid",
                        )?,
                        created_at: col_get(
                            row,
                            5,
                            source_role,
                            "agent_bridge_checkpoint",
                            "created_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_BRIDGE_CHECKPOINT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_bridge_checkpoint"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.checkpoint_id.as_str(), bytes.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert agent_bridge_checkpoint")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_bridge_checkpoint", n);
    }

    // 6. agent_bridge_event + bridge_events_by_seq + events_by_time + capture_sequences
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_bridge_event").await?;
        let mut max_event_id =
            read_sqlite_sequence_highwater(conn, source_role, "agent_bridge_event")
                .await?
                .max(inline_bridge_event_seq_floor);
        let n = {
            require_columns(
                &cols,
                "agent_bridge_event",
                &[
                    "event_id",
                    "bridge_session_id",
                    "event_seq",
                    "event_type",
                    "payload_sha256",
                    "payload",
                    "operation_id",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_bridge_event",
                    select_cols: "event_id, bridge_session_id, event_seq, event_type, payload_sha256, payload, operation_id, created_at",
                    rowid_expr: "event_id",
                },

                |row| {
                    Ok(AgentBridgeEventRecord {
                        event_id: col_get(row, 1, source_role, "agent_bridge_event", "event_id")?,
                        bridge_session_id: col_get(
                            row,
                            2,
                            source_role,
                            "agent_bridge_event",
                            "bridge_session_id",
                        )?,
                        event_seq: col_get(
                            row,
                            3,
                            source_role,
                            "agent_bridge_event",
                            "event_seq",
                        )?,
                        event_type: col_get(
                            row,
                            4,
                            source_role,
                            "agent_bridge_event",
                            "event_type",
                        )?,
                        payload_sha256: col_get(
                            row,
                            5,
                            source_role,
                            "agent_bridge_event",
                            "payload_sha256",
                        )?,
                        payload: col_get(row, 6, source_role, "agent_bridge_event", "payload")?,
                        operation_id: col_get(
                            row,
                            7,
                            source_role,
                            "agent_bridge_event",
                            "operation_id",
                        )?,
                        created_at: col_get(
                            row,
                            8,
                            source_role,
                            "agent_bridge_event",
                            "created_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_BRIDGE_EVENT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_bridge_event"))?;
                    let mut by_seq = wtx
                        .open_table(redb_cap::BRIDGE_EVENTS_BY_SEQ_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open bridge_events_by_seq"))?;
                    let mut by_time = wtx
                        .open_table(redb_cap::EVENTS_BY_TIME_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open events_by_time"))?;
                    for rec in batch {
                        if rec.event_id > max_event_id {
                            max_event_id = rec.event_id;
                        }
                        let rec_bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.event_id, rec_bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_bridge_event"))?;
                        let seq_bytes = encode_versioned(
                            cv,
                            &MigBridgeEventSeqIndexEntry {
                                event_id: rec.event_id,
                                payload_sha256: rec.payload_sha256.clone(),
                            },
                        )?;
                        if by_seq
                            .insert(
                                (rec.bridge_session_id.as_str(), rec.event_seq),
                                seq_bytes.as_slice(),
                            )
                            .map_err(|e| map_redb_storage_err(e, "insert bridge_events_by_seq"))?
                            .is_some()
                        {
                            return Err(StorageBackendError::BackendInternal {
                                detail: "duplicate (bridge_session_id, event_seq) in sqlite agent_bridge_event".to_string(),
                            });
                        }
                        let time_bytes = encode_versioned(
                            cv,
                            &MigEventTimeIndexEntry {
                                event_id: rec.event_id,
                            },
                        )?;
                        by_time
                            .insert(
                                (
                                    rec.created_at,
                                    rec.bridge_session_id.as_str(),
                                    rec.event_seq,
                                ),
                                time_bytes.as_slice(),
                            )
                            .map_err(|e| map_redb_storage_err(e, "insert events_by_time"))?;
                    }
                    let mut seq_tbl = wtx
                        .open_table(redb_cap::CAPTURE_SEQUENCES_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open capture_sequences"))?;
                    let seq_val = encode_versioned(
                        cv,
                        &MigCaptureSequenceValue {
                            last_id: max_event_id,
                        },
                    )?;
                    seq_tbl
                        .insert(MIG_SEQ_KEY_BRIDGE_EVENT, seq_val.as_slice())
                        .map_err(|e| map_redb_storage_err(e, "update bridge_event sequence"))?;
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        persist_capture_sequence_highwater(db, MIG_SEQ_KEY_BRIDGE_EVENT, max_event_id)?;
        counts.insert("agent_bridge_event", n);
    }

    // 7. agent_bridge_link + bridge_links_by_key + capture_sequences
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_bridge_link").await?;
        let mut max_link_id =
            read_sqlite_sequence_highwater(conn, source_role, "agent_bridge_link")
                .await?
                .max(inline_bridge_link_seq_floor);
        let n = {
            require_columns(
                &cols,
                "agent_bridge_link",
                &[
                    "link_id",
                    "bridge_session_id",
                    "source_type",
                    "source_id",
                    "target_type",
                    "target_id",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_bridge_link",
                    select_cols: "link_id, bridge_session_id, source_type, source_id, target_type, target_id, created_at",
                    rowid_expr: "link_id",
                },

                |row| {
                    let st_s: String =
                        col_get(row, 3, source_role, "agent_bridge_link", "source_type")?;
                    Ok(AgentBridgeLinkRecord {
                        link_id: col_get(row, 1, source_role, "agent_bridge_link", "link_id")?,
                        bridge_session_id: col_get(
                            row,
                            2,
                            source_role,
                            "agent_bridge_link",
                            "bridge_session_id",
                        )?,
                        source_type: BridgeLinkSourceType::parse(&st_s)?,
                        source_id: col_get(row, 4, source_role, "agent_bridge_link", "source_id")?,
                        target_type: col_get(
                            row,
                            5,
                            source_role,
                            "agent_bridge_link",
                            "target_type",
                        )?,
                        target_id: col_get(row, 6, source_role, "agent_bridge_link", "target_id")?,
                        created_at: col_get(
                            row,
                            7,
                            source_role,
                            "agent_bridge_link",
                            "created_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_BRIDGE_LINK_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_bridge_link"))?;
                    let mut by_key = wtx
                        .open_table(redb_cap::BRIDGE_LINKS_BY_KEY_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open bridge_links_by_key"))?;
                    for rec in batch {
                        if rec.link_id > max_link_id {
                            max_link_id = rec.link_id;
                        }
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.link_id, bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_bridge_link"))?;
                        let idx_bytes = encode_versioned(
                            cv,
                            &MigBridgeLinkIndexEntry {
                                link_id: rec.link_id,
                            },
                        )?;
                        if by_key
                            .insert(
                                (
                                    rec.source_type.as_str(),
                                    rec.source_id.as_str(),
                                    rec.target_type.as_str(),
                                    rec.target_id.as_str(),
                                ),
                                idx_bytes.as_slice(),
                            )
                            .map_err(|e| map_redb_storage_err(e, "insert bridge_links_by_key"))?
                            .is_some()
                        {
                            return Err(StorageBackendError::BackendInternal {
                                detail: "duplicate unique key in sqlite agent_bridge_link"
                                    .to_string(),
                            });
                        }
                    }
                    let mut seq_tbl = wtx
                        .open_table(redb_cap::CAPTURE_SEQUENCES_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open capture_sequences"))?;
                    let seq_val = encode_versioned(
                        cv,
                        &MigCaptureSequenceValue {
                            last_id: max_link_id,
                        },
                    )?;
                    seq_tbl
                        .insert(MIG_SEQ_KEY_BRIDGE_LINK, seq_val.as_slice())
                        .map_err(|e| map_redb_storage_err(e, "update bridge_link sequence"))?;
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        persist_capture_sequence_highwater(db, MIG_SEQ_KEY_BRIDGE_LINK, max_link_id)?;
        counts.insert("agent_bridge_link", n);
    }

    // 8. agent_bridge_operation
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_bridge_operation")
                .await?;
        let n = {
            require_columns(
                &cols,
                "agent_bridge_operation",
                &[
                    "operation_id",
                    "bridge_session_id",
                    "method",
                    "params_digest",
                    "repository_id",
                    "workspace_id",
                    "status",
                    "result_digest",
                    "created_at",
                    "completed_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_bridge_operation",
                    select_cols: "operation_id, bridge_session_id, method, params_digest, repository_id, workspace_id, status, result_digest, created_at, completed_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let status_s: String =
                        col_get(row, 7, source_role, "agent_bridge_operation", "status")?;
                    Ok(AgentBridgeOperationRecord {
                        operation_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_bridge_operation",
                            "operation_id",
                        )?,
                        bridge_session_id: col_get(
                            row,
                            2,
                            source_role,
                            "agent_bridge_operation",
                            "bridge_session_id",
                        )?,
                        method: col_get(row, 3, source_role, "agent_bridge_operation", "method")?,
                        params_digest: col_get(
                            row,
                            4,
                            source_role,
                            "agent_bridge_operation",
                            "params_digest",
                        )?,
                        repository_id: col_get(
                            row,
                            5,
                            source_role,
                            "agent_bridge_operation",
                            "repository_id",
                        )?,
                        workspace_id: col_get(
                            row,
                            6,
                            source_role,
                            "agent_bridge_operation",
                            "workspace_id",
                        )?,
                        status: BridgeOperationStatus::parse(&status_s)?,
                        result_digest: col_get(
                            row,
                            8,
                            source_role,
                            "agent_bridge_operation",
                            "result_digest",
                        )?,
                        created_at: col_get(
                            row,
                            9,
                            source_role,
                            "agent_bridge_operation",
                            "created_at",
                        )?,
                        completed_at: col_get(
                            row,
                            10,
                            source_role,
                            "agent_bridge_operation",
                            "completed_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_BRIDGE_OPERATION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_bridge_operation"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.operation_id.as_str(), bytes.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert agent_bridge_operation")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_bridge_operation", n);
    }

    // 9. agent_capture_cloud_base
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_capture_cloud_base")
                .await?;
        let n = {
            require_columns(
                &cols,
                "agent_capture_cloud_base",
                &["repo_id", "remote_generation", "updated_at"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_capture_cloud_base",
                    select_cols: "repo_id, remote_generation, updated_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(AgentCaptureCloudBaseRecord {
                        repo_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_capture_cloud_base",
                            "repo_id",
                        )?,
                        remote_generation: col_get(
                            row,
                            2,
                            source_role,
                            "agent_capture_cloud_base",
                            "remote_generation",
                        )?,
                        updated_at: col_get(
                            row,
                            3,
                            source_role,
                            "agent_capture_cloud_base",
                            "updated_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_CAPTURE_CLOUD_BASE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_capture_cloud_base"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.repo_id.as_str(), bytes.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert agent_capture_cloud_base")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_capture_cloud_base", n);
    }

    // 10. agent_capture_incarnation
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_capture_incarnation")
                .await?;
        let n = {
            require_columns(
                &cols,
                "agent_capture_incarnation",
                &[
                    "agent_kind",
                    "provider_session_id",
                    "next_session_sync_revision",
                    "source_namespace",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_capture_incarnation",
                    select_cols: "agent_kind, provider_session_id, next_session_sync_revision, source_namespace, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AgentCaptureIncarnationRecord {
                        agent_kind: col_get(
                            row,
                            1,
                            source_role,
                            "agent_capture_incarnation",
                            "agent_kind",
                        )?,
                        provider_session_id: col_get(
                            row,
                            2,
                            source_role,
                            "agent_capture_incarnation",
                            "provider_session_id",
                        )?,
                        next_session_sync_revision: col_get(
                            row,
                            3,
                            source_role,
                            "agent_capture_incarnation",
                            "next_session_sync_revision",
                        )?,
                        source_namespace: col_get(
                            row,
                            4,
                            source_role,
                            "agent_capture_incarnation",
                            "source_namespace",
                        )?,
                        updated_at: col_get(
                            row,
                            5,
                            source_role,
                            "agent_capture_incarnation",
                            "updated_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_CAPTURE_INCARNATION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_capture_incarnation"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (rec.agent_kind.as_str(), rec.provider_session_id.as_str()),
                            bytes.as_slice(),
                        )
                        .map_err(|e| {
                            map_redb_storage_err(e, "insert agent_capture_incarnation")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_capture_incarnation", n);
    }

    // 11. agent_session + sessions_by_time + sessions_by_provider
    {
        let cols = require_sqlite_table_columns_on_conn(conn, source_role, "agent_session").await?;
        let n = {
            require_columns(
                &cols,
                "agent_session",
                &[
                    "session_id",
                    "agent_kind",
                    "provider_session_id",
                    "thread_id",
                    "state",
                    "working_dir",
                    "worktree_id",
                    "parent_commit",
                    "parent_session_id",
                    "metadata_json",
                    "redaction_report",
                    "started_at",
                    "last_event_at",
                    "stopped_at",
                    "schema_version",
                    "repo_id",
                    "workspace_id",
                    "workspace_fence",
                    "scope_state",
                ],
            )?;
            let sr_expr = if cols.contains("sync_revision") {
                "sync_revision"
            } else {
                "0"
            };
            let select = format!(
                "session_id, agent_kind, provider_session_id, thread_id, state, working_dir, worktree_id, parent_commit, parent_session_id, metadata_json, redaction_report, started_at, last_event_at, stopped_at, schema_version, {sr_expr}, repo_id, workspace_id, workspace_fence, scope_state"
            );
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_session",
                    select_cols: &select,
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let ak_s: String = col_get(row, 2, source_role, "agent_session", "agent_kind")?;
                    let st_s: String = col_get(row, 5, source_role, "agent_session", "state")?;
                    let ss_s: String =
                        col_get(row, 20, source_role, "agent_session", "scope_state")?;
                    Ok(AgentSessionRecord {
                        session_id: col_get(row, 1, source_role, "agent_session", "session_id")?,
                        agent_kind: CaptureAgentKind::parse(&ak_s)?,
                        provider_session_id: col_get(
                            row,
                            3,
                            source_role,
                            "agent_session",
                            "provider_session_id",
                        )?,
                        thread_id: col_get(row, 4, source_role, "agent_session", "thread_id")?,
                        state: CaptureSessionState::parse(&st_s)?,
                        working_dir: col_get(row, 6, source_role, "agent_session", "working_dir")?,
                        worktree_id: col_get(row, 7, source_role, "agent_session", "worktree_id")?,
                        parent_commit: col_get(
                            row,
                            8,
                            source_role,
                            "agent_session",
                            "parent_commit",
                        )?,
                        parent_session_id: col_get(
                            row,
                            9,
                            source_role,
                            "agent_session",
                            "parent_session_id",
                        )?,
                        metadata_json: col_get(
                            row,
                            10,
                            source_role,
                            "agent_session",
                            "metadata_json",
                        )?,
                        redaction_report: col_get(
                            row,
                            11,
                            source_role,
                            "agent_session",
                            "redaction_report",
                        )?,
                        started_at: col_get(row, 12, source_role, "agent_session", "started_at")?,
                        last_event_at: col_get(
                            row,
                            13,
                            source_role,
                            "agent_session",
                            "last_event_at",
                        )?,
                        stopped_at: col_get(row, 14, source_role, "agent_session", "stopped_at")?,
                        schema_version: col_get(
                            row,
                            15,
                            source_role,
                            "agent_session",
                            "schema_version",
                        )?,
                        sync_revision: col_get(
                            row,
                            16,
                            source_role,
                            "agent_session",
                            "sync_revision",
                        )?,
                        repo_id: col_get(row, 17, source_role, "agent_session", "repo_id")?,
                        workspace_id: col_get(
                            row,
                            18,
                            source_role,
                            "agent_session",
                            "workspace_id",
                        )?,
                        workspace_fence: col_get(
                            row,
                            19,
                            source_role,
                            "agent_session",
                            "workspace_fence",
                        )?,
                        scope_state: CaptureScopeState::parse(&ss_s)?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_SESSION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_session"))?;
                    let mut by_time = wtx
                        .open_table(redb_cap::SESSIONS_BY_TIME_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open sessions_by_time"))?;
                    let mut by_provider = wtx
                        .open_table(redb_cap::SESSIONS_BY_PROVIDER_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open sessions_by_provider"))?;
                    let marker = encode_versioned(cv, &MigCaptureMarkerValue)?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.session_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_session"))?;
                        by_time
                            .insert(
                                (
                                    invert_capture_timestamp(rec.started_at),
                                    rec.session_id.as_str(),
                                ),
                                marker.as_slice(),
                            )
                            .map_err(|e| map_redb_storage_err(e, "insert sessions_by_time"))?;
                        let prov_bytes = encode_versioned(
                            cv,
                            &MigSessionProviderIndexEntry {
                                session_id: rec.session_id.clone(),
                            },
                        )?;
                        if by_provider
                            .insert(
                                (rec.agent_kind.as_str(), rec.provider_session_id.as_str()),
                                prov_bytes.as_slice(),
                            )
                            .map_err(|e| map_redb_storage_err(e, "insert sessions_by_provider"))?
                            .is_some()
                        {
                            return Err(StorageBackendError::BackendInternal {
                                detail: "duplicate (agent_kind, provider_session_id) in sqlite agent_session".to_string(),
                            });
                        }
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_session", n);
    }

    // 12. agent_checkpoint + checkpoints_by_session
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_checkpoint").await?;
        let n = {
            require_columns(
                &cols,
                "agent_checkpoint",
                &[
                    "checkpoint_id",
                    "session_id",
                    "parent_checkpoint_id",
                    "scope",
                    "parent_commit",
                    "tree_oid",
                    "metadata_blob_oid",
                    "traces_commit",
                    "tool_use_id",
                    "subagent_session_id",
                    "description",
                    "created_at",
                ],
            )?;
            let sr_expr = if cols.contains("sync_revision") {
                "sync_revision"
            } else {
                "0"
            };
            let select = format!(
                "checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit, tree_oid, metadata_blob_oid, traces_commit, tool_use_id, subagent_session_id, description, created_at, {sr_expr}"
            );
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_checkpoint",
                    select_cols: &select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let scope_s: String =
                        col_get(row, 4, source_role, "agent_checkpoint", "scope")?;
                    Ok(AgentCheckpointRecord {
                        checkpoint_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_checkpoint",
                            "checkpoint_id",
                        )?,
                        session_id: col_get(row, 2, source_role, "agent_checkpoint", "session_id")?,
                        parent_checkpoint_id: col_get(
                            row,
                            3,
                            source_role,
                            "agent_checkpoint",
                            "parent_checkpoint_id",
                        )?,
                        scope: CaptureCheckpointScope::parse(&scope_s)?,
                        parent_commit: col_get(
                            row,
                            5,
                            source_role,
                            "agent_checkpoint",
                            "parent_commit",
                        )?,
                        tree_oid: col_get(row, 6, source_role, "agent_checkpoint", "tree_oid")?,
                        metadata_blob_oid: col_get(
                            row,
                            7,
                            source_role,
                            "agent_checkpoint",
                            "metadata_blob_oid",
                        )?,
                        traces_commit: col_get(
                            row,
                            8,
                            source_role,
                            "agent_checkpoint",
                            "traces_commit",
                        )?,
                        tool_use_id: col_get(
                            row,
                            9,
                            source_role,
                            "agent_checkpoint",
                            "tool_use_id",
                        )?,
                        subagent_session_id: col_get(
                            row,
                            10,
                            source_role,
                            "agent_checkpoint",
                            "subagent_session_id",
                        )?,
                        description: col_get(
                            row,
                            11,
                            source_role,
                            "agent_checkpoint",
                            "description",
                        )?,
                        created_at: col_get(
                            row,
                            12,
                            source_role,
                            "agent_checkpoint",
                            "created_at",
                        )?,
                        sync_revision: col_get(
                            row,
                            13,
                            source_role,
                            "agent_checkpoint",
                            "sync_revision",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_CHECKPOINT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_checkpoint"))?;
                    let mut by_session = wtx
                        .open_table(redb_cap::CHECKPOINTS_BY_SESSION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open checkpoints_by_session"))?;
                    let marker = encode_versioned(cv, &MigCaptureMarkerValue)?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.checkpoint_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_checkpoint"))?;
                        by_session
                            .insert(
                                (
                                    rec.session_id.as_str(),
                                    rec.created_at,
                                    rec.checkpoint_id.as_str(),
                                ),
                                marker.as_slice(),
                            )
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert checkpoints_by_session")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_checkpoint", n);
    }

    // 13. agent_checkpoint_prune_tombstone
    {
        let cols = require_sqlite_table_columns_on_conn(
            conn,
            source_role,
            "agent_checkpoint_prune_tombstone",
        )
        .await?;
        let n = {
            require_columns(
                &cols,
                "agent_checkpoint_prune_tombstone",
                &["checkpoint_id", "session_id", "pruned_at"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_checkpoint_prune_tombstone",
                    select_cols: "checkpoint_id, session_id, pruned_at",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    Ok(AgentCheckpointPruneTombstoneRecord {
                        checkpoint_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_checkpoint_prune_tombstone",
                            "checkpoint_id",
                        )?,
                        session_id: col_get(
                            row,
                            2,
                            source_role,
                            "agent_checkpoint_prune_tombstone",
                            "session_id",
                        )?,
                        pruned_at: col_get(
                            row,
                            3,
                            source_role,
                            "agent_checkpoint_prune_tombstone",
                            "pruned_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_CHECKPOINT_PRUNE_TOMBSTONE_TABLE)
                        .map_err(|e| {
                            map_redb_table_err(e, "open agent_checkpoint_prune_tombstone")
                        })?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.checkpoint_id.as_str(), bytes.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert agent_checkpoint_prune_tombstone")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_checkpoint_prune_tombstone", n);
    }

    // 14. agent_coverage_claim
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_coverage_claim").await?;
        let n = {
            require_columns(
                &cols,
                "agent_coverage_claim",
                &[
                    "session_id",
                    "logical_turn_key",
                    "coverage_schema_version",
                    "coverage_digest",
                    "completeness",
                    "revision",
                    "state",
                    "attempt_checkpoint_id",
                    "owner",
                    "lease_expires_at",
                    "fence_token",
                    "checkpoint_id",
                    "traces_commit",
                    "source_channel",
                    "created_at",
                    "updated_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_coverage_claim",
                    select_cols: "session_id, logical_turn_key, coverage_schema_version, coverage_digest, completeness, revision, state, attempt_checkpoint_id, owner, lease_expires_at, fence_token, checkpoint_id, traces_commit, source_channel, created_at, updated_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let comp_s: String =
                        col_get(row, 5, source_role, "agent_coverage_claim", "completeness")?;
                    let state_s: String =
                        col_get(row, 7, source_role, "agent_coverage_claim", "state")?;
                    let chan_s: String = col_get(
                        row,
                        14,
                        source_role,
                        "agent_coverage_claim",
                        "source_channel",
                    )?;
                    Ok(AgentCoverageClaimRecord {
                        session_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_coverage_claim",
                            "session_id",
                        )?,
                        logical_turn_key: col_get(
                            row,
                            2,
                            source_role,
                            "agent_coverage_claim",
                            "logical_turn_key",
                        )?,
                        coverage_schema_version: col_get(
                            row,
                            3,
                            source_role,
                            "agent_coverage_claim",
                            "coverage_schema_version",
                        )?,
                        coverage_digest: col_get(
                            row,
                            4,
                            source_role,
                            "agent_coverage_claim",
                            "coverage_digest",
                        )?,
                        completeness: CaptureCompleteness::parse(&comp_s)?,
                        revision: col_get(row, 6, source_role, "agent_coverage_claim", "revision")?,
                        state: CaptureCoverageClaimState::parse(&state_s)?,
                        attempt_checkpoint_id: col_get(
                            row,
                            8,
                            source_role,
                            "agent_coverage_claim",
                            "attempt_checkpoint_id",
                        )?,
                        owner: col_get(row, 9, source_role, "agent_coverage_claim", "owner")?,
                        lease_expires_at: col_get(
                            row,
                            10,
                            source_role,
                            "agent_coverage_claim",
                            "lease_expires_at",
                        )?,
                        fence_token: col_get(
                            row,
                            11,
                            source_role,
                            "agent_coverage_claim",
                            "fence_token",
                        )?,
                        checkpoint_id: col_get(
                            row,
                            12,
                            source_role,
                            "agent_coverage_claim",
                            "checkpoint_id",
                        )?,
                        traces_commit: col_get(
                            row,
                            13,
                            source_role,
                            "agent_coverage_claim",
                            "traces_commit",
                        )?,
                        source_channel: CaptureSourceChannel::parse(&chan_s)?,
                        created_at: col_get(
                            row,
                            15,
                            source_role,
                            "agent_coverage_claim",
                            "created_at",
                        )?,
                        updated_at: col_get(
                            row,
                            16,
                            source_role,
                            "agent_coverage_claim",
                            "updated_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_COVERAGE_CLAIM_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_coverage_claim"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.session_id.as_str(),
                                rec.logical_turn_key.as_str(),
                                rec.coverage_schema_version,
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert agent_coverage_claim"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_coverage_claim", n);
    }

    // 15. agent_coverage_conflict
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_coverage_conflict")
                .await?;
        let n = {
            require_columns(
                &cols,
                "agent_coverage_conflict",
                &[
                    "session_id",
                    "logical_turn_key",
                    "coverage_schema_version",
                    "incumbent_revision",
                    "incumbent_digest",
                    "incumbent_checkpoint_id",
                    "incoming_digest",
                    "incoming_source_channel",
                    "incoming_observed_at",
                    "incoming_canonical_json",
                    "incoming_redaction_report_json",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_coverage_conflict",
                    select_cols: "session_id, logical_turn_key, coverage_schema_version, incumbent_revision, incumbent_digest, incumbent_checkpoint_id, incoming_digest, incoming_source_channel, incoming_observed_at, incoming_canonical_json, incoming_redaction_report_json",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let chan_s: String = col_get(
                        row,
                        8,
                        source_role,
                        "agent_coverage_conflict",
                        "incoming_source_channel",
                    )?;
                    Ok(AgentCoverageConflictRecord {
                        session_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_coverage_conflict",
                            "session_id",
                        )?,
                        logical_turn_key: col_get(
                            row,
                            2,
                            source_role,
                            "agent_coverage_conflict",
                            "logical_turn_key",
                        )?,
                        coverage_schema_version: col_get(
                            row,
                            3,
                            source_role,
                            "agent_coverage_conflict",
                            "coverage_schema_version",
                        )?,
                        incumbent_revision: col_get(
                            row,
                            4,
                            source_role,
                            "agent_coverage_conflict",
                            "incumbent_revision",
                        )?,
                        incumbent_digest: col_get(
                            row,
                            5,
                            source_role,
                            "agent_coverage_conflict",
                            "incumbent_digest",
                        )?,
                        incumbent_checkpoint_id: col_get(
                            row,
                            6,
                            source_role,
                            "agent_coverage_conflict",
                            "incumbent_checkpoint_id",
                        )?,
                        incoming_digest: col_get(
                            row,
                            7,
                            source_role,
                            "agent_coverage_conflict",
                            "incoming_digest",
                        )?,
                        incoming_source_channel: CaptureSourceChannel::parse(&chan_s)?,
                        incoming_observed_at: col_get(
                            row,
                            9,
                            source_role,
                            "agent_coverage_conflict",
                            "incoming_observed_at",
                        )?,
                        incoming_canonical_json: col_get(
                            row,
                            10,
                            source_role,
                            "agent_coverage_conflict",
                            "incoming_canonical_json",
                        )?,
                        incoming_redaction_report_json: col_get(
                            row,
                            11,
                            source_role,
                            "agent_coverage_conflict",
                            "incoming_redaction_report_json",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_COVERAGE_CONFLICT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_coverage_conflict"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.session_id.as_str(),
                                rec.logical_turn_key.as_str(),
                                rec.coverage_schema_version,
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert agent_coverage_conflict"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_coverage_conflict", n);
    }

    // 16. agent_coverage_revision
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_coverage_revision")
                .await?;
        let n = {
            require_columns(
                &cols,
                "agent_coverage_revision",
                &[
                    "session_id",
                    "logical_turn_key",
                    "coverage_schema_version",
                    "revision",
                    "checkpoint_id",
                    "coverage_digest",
                    "completeness",
                    "source_channel",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_coverage_revision",
                    select_cols: "session_id, logical_turn_key, coverage_schema_version, revision, checkpoint_id, coverage_digest, completeness, source_channel, created_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let comp_s: String = col_get(
                        row,
                        7,
                        source_role,
                        "agent_coverage_revision",
                        "completeness",
                    )?;
                    let chan_s: String = col_get(
                        row,
                        8,
                        source_role,
                        "agent_coverage_revision",
                        "source_channel",
                    )?;
                    Ok(AgentCoverageRevisionRecord {
                        session_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_coverage_revision",
                            "session_id",
                        )?,
                        logical_turn_key: col_get(
                            row,
                            2,
                            source_role,
                            "agent_coverage_revision",
                            "logical_turn_key",
                        )?,
                        coverage_schema_version: col_get(
                            row,
                            3,
                            source_role,
                            "agent_coverage_revision",
                            "coverage_schema_version",
                        )?,
                        revision: col_get(
                            row,
                            4,
                            source_role,
                            "agent_coverage_revision",
                            "revision",
                        )?,
                        checkpoint_id: col_get(
                            row,
                            5,
                            source_role,
                            "agent_coverage_revision",
                            "checkpoint_id",
                        )?,
                        coverage_digest: col_get(
                            row,
                            6,
                            source_role,
                            "agent_coverage_revision",
                            "coverage_digest",
                        )?,
                        completeness: CaptureCompleteness::parse(&comp_s)?,
                        source_channel: CaptureSourceChannel::parse(&chan_s)?,
                        created_at: col_get(
                            row,
                            9,
                            source_role,
                            "agent_coverage_revision",
                            "created_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_COVERAGE_REVISION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_coverage_revision"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.session_id.as_str(),
                                rec.logical_turn_key.as_str(),
                                rec.coverage_schema_version,
                                rec.revision,
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert agent_coverage_revision"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_coverage_revision", n);
    }

    // 17. agent_export_job
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_export_job").await?;
        let n = {
            require_columns(
                &cols,
                "agent_export_job",
                &[
                    "job_id",
                    "agent_kind",
                    "provider_session_id",
                    "owner",
                    "lease_expires_at",
                    "fence_token",
                    "observed_generation",
                    "processed_generation",
                    "state",
                    "last_error_code",
                    "created_at",
                    "updated_at",
                    "ttl_expires_at",
                    "repo_id",
                    "worktree_id",
                    "workspace_id",
                    "workspace_fence",
                    "scope_state",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_export_job",
                    select_cols: "job_id, agent_kind, provider_session_id, owner, lease_expires_at, fence_token, observed_generation, processed_generation, state, last_error_code, created_at, updated_at, ttl_expires_at, repo_id, worktree_id, workspace_id, workspace_fence, scope_state",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let st_s: String = col_get(row, 9, source_role, "agent_export_job", "state")?;
                    let ss_s: String =
                        col_get(row, 18, source_role, "agent_export_job", "scope_state")?;
                    Ok(AgentExportJobRecord {
                        job_id: col_get(row, 1, source_role, "agent_export_job", "job_id")?,
                        agent_kind: col_get(row, 2, source_role, "agent_export_job", "agent_kind")?,
                        provider_session_id: col_get(
                            row,
                            3,
                            source_role,
                            "agent_export_job",
                            "provider_session_id",
                        )?,
                        owner: col_get(row, 4, source_role, "agent_export_job", "owner")?,
                        lease_expires_at: col_get(
                            row,
                            5,
                            source_role,
                            "agent_export_job",
                            "lease_expires_at",
                        )?,
                        fence_token: col_get(
                            row,
                            6,
                            source_role,
                            "agent_export_job",
                            "fence_token",
                        )?,
                        observed_generation: col_get(
                            row,
                            7,
                            source_role,
                            "agent_export_job",
                            "observed_generation",
                        )?,
                        processed_generation: col_get(
                            row,
                            8,
                            source_role,
                            "agent_export_job",
                            "processed_generation",
                        )?,
                        state: CaptureExportJobState::parse(&st_s)?,
                        last_error_code: col_get(
                            row,
                            10,
                            source_role,
                            "agent_export_job",
                            "last_error_code",
                        )?,
                        created_at: col_get(
                            row,
                            11,
                            source_role,
                            "agent_export_job",
                            "created_at",
                        )?,
                        updated_at: col_get(
                            row,
                            12,
                            source_role,
                            "agent_export_job",
                            "updated_at",
                        )?,
                        ttl_expires_at: col_get(
                            row,
                            13,
                            source_role,
                            "agent_export_job",
                            "ttl_expires_at",
                        )?,
                        repo_id: col_get(row, 14, source_role, "agent_export_job", "repo_id")?,
                        worktree_id: col_get(
                            row,
                            15,
                            source_role,
                            "agent_export_job",
                            "worktree_id",
                        )?,
                        workspace_id: col_get(
                            row,
                            16,
                            source_role,
                            "agent_export_job",
                            "workspace_id",
                        )?,
                        workspace_fence: col_get(
                            row,
                            17,
                            source_role,
                            "agent_export_job",
                            "workspace_fence",
                        )?,
                        scope_state: CaptureScopeState::parse(&ss_s)?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_EXPORT_JOB_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_export_job"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.job_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_export_job"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_export_job", n);
    }

    // 18. agent_import_identity
    {
        let cols = require_sqlite_table_columns_on_conn(conn, source_role, "agent_import_identity")
            .await?;
        let n = {
            require_columns(
                &cols,
                "agent_import_identity",
                &[
                    "identity_id",
                    "agent_kind",
                    "provider_session_id",
                    "source_kind",
                    "source_id",
                    "schema_version",
                    "observed_digest",
                    "committed_digest",
                    "attempt_id",
                    "attempt_checkpoint_id",
                    "next_ordinal",
                    "state",
                    "owner",
                    "lease_expires_at",
                    "fence_token",
                    "last_error_code",
                    "created_at",
                    "updated_at",
                    "repo_id",
                    "worktree_id",
                    "workspace_id",
                    "workspace_fence",
                    "scope_state",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_import_identity",
                    select_cols: "identity_id, agent_kind, provider_session_id, source_kind, source_id, schema_version, observed_digest, committed_digest, attempt_id, attempt_checkpoint_id, next_ordinal, state, owner, lease_expires_at, fence_token, last_error_code, created_at, updated_at, repo_id, worktree_id, workspace_id, workspace_fence, scope_state",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let st_s: String =
                        col_get(row, 12, source_role, "agent_import_identity", "state")?;
                    let ss_s: String =
                        col_get(row, 23, source_role, "agent_import_identity", "scope_state")?;
                    Ok(AgentImportIdentityRecord {
                        identity_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_import_identity",
                            "identity_id",
                        )?,
                        agent_kind: col_get(
                            row,
                            2,
                            source_role,
                            "agent_import_identity",
                            "agent_kind",
                        )?,
                        provider_session_id: col_get(
                            row,
                            3,
                            source_role,
                            "agent_import_identity",
                            "provider_session_id",
                        )?,
                        source_kind: col_get(
                            row,
                            4,
                            source_role,
                            "agent_import_identity",
                            "source_kind",
                        )?,
                        source_id: col_get(
                            row,
                            5,
                            source_role,
                            "agent_import_identity",
                            "source_id",
                        )?,
                        schema_version: col_get(
                            row,
                            6,
                            source_role,
                            "agent_import_identity",
                            "schema_version",
                        )?,
                        observed_digest: col_get(
                            row,
                            7,
                            source_role,
                            "agent_import_identity",
                            "observed_digest",
                        )?,
                        committed_digest: col_get(
                            row,
                            8,
                            source_role,
                            "agent_import_identity",
                            "committed_digest",
                        )?,
                        attempt_id: col_get(
                            row,
                            9,
                            source_role,
                            "agent_import_identity",
                            "attempt_id",
                        )?,
                        attempt_checkpoint_id: col_get(
                            row,
                            10,
                            source_role,
                            "agent_import_identity",
                            "attempt_checkpoint_id",
                        )?,
                        next_ordinal: col_get(
                            row,
                            11,
                            source_role,
                            "agent_import_identity",
                            "next_ordinal",
                        )?,
                        state: CaptureImportIdentityState::parse(&st_s)?,
                        owner: col_get(row, 13, source_role, "agent_import_identity", "owner")?,
                        lease_expires_at: col_get(
                            row,
                            14,
                            source_role,
                            "agent_import_identity",
                            "lease_expires_at",
                        )?,
                        fence_token: col_get(
                            row,
                            15,
                            source_role,
                            "agent_import_identity",
                            "fence_token",
                        )?,
                        last_error_code: col_get(
                            row,
                            16,
                            source_role,
                            "agent_import_identity",
                            "last_error_code",
                        )?,
                        created_at: col_get(
                            row,
                            17,
                            source_role,
                            "agent_import_identity",
                            "created_at",
                        )?,
                        updated_at: col_get(
                            row,
                            18,
                            source_role,
                            "agent_import_identity",
                            "updated_at",
                        )?,
                        repo_id: col_get(row, 19, source_role, "agent_import_identity", "repo_id")?,
                        worktree_id: col_get(
                            row,
                            20,
                            source_role,
                            "agent_import_identity",
                            "worktree_id",
                        )?,
                        workspace_id: col_get(
                            row,
                            21,
                            source_role,
                            "agent_import_identity",
                            "workspace_id",
                        )?,
                        workspace_fence: col_get(
                            row,
                            22,
                            source_role,
                            "agent_import_identity",
                            "workspace_fence",
                        )?,
                        scope_state: CaptureScopeState::parse(&ss_s)?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_IMPORT_IDENTITY_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_import_identity"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.identity_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_import_identity"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_import_identity", n);
    }

    // 19. agent_import_tombstone + import_tombstones_by_provider + import_tombstones_by_session
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_import_tombstone")
                .await?;
        let n = {
            require_columns(
                &cols,
                "agent_import_tombstone",
                &[
                    "tombstone_id",
                    "agent_kind",
                    "provider_session_id",
                    "erased_session_id",
                    "source_fingerprint",
                    "erased_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_import_tombstone",
                    select_cols: "tombstone_id, agent_kind, provider_session_id, erased_session_id, source_fingerprint, erased_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AgentImportTombstoneRecord {
                        tombstone_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_import_tombstone",
                            "tombstone_id",
                        )?,
                        agent_kind: col_get(
                            row,
                            2,
                            source_role,
                            "agent_import_tombstone",
                            "agent_kind",
                        )?,
                        provider_session_id: col_get(
                            row,
                            3,
                            source_role,
                            "agent_import_tombstone",
                            "provider_session_id",
                        )?,
                        erased_session_id: col_get(
                            row,
                            4,
                            source_role,
                            "agent_import_tombstone",
                            "erased_session_id",
                        )?,
                        source_fingerprint: col_get(
                            row,
                            5,
                            source_role,
                            "agent_import_tombstone",
                            "source_fingerprint",
                        )?,
                        erased_at: col_get(
                            row,
                            6,
                            source_role,
                            "agent_import_tombstone",
                            "erased_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_IMPORT_TOMBSTONE_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_import_tombstone"))?;
                    let mut by_provider = wtx
                        .open_table(redb_cap::IMPORT_TOMBSTONES_BY_PROVIDER_TABLE)
                        .map_err(|e| {
                            map_redb_table_err(e, "open import_tombstones_by_provider")
                        })?;
                    let mut by_session = wtx
                        .open_table(redb_cap::IMPORT_TOMBSTONES_BY_SESSION_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open import_tombstones_by_session"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.tombstone_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_import_tombstone"))?;
                        let idx = encode_versioned(
                            cv,
                            &MigTombstoneIndexEntry {
                                tombstone_id: rec.tombstone_id.clone(),
                            },
                        )?;
                        by_provider
                            .insert(
                                (rec.agent_kind.as_str(), rec.provider_session_id.as_str()),
                                idx.as_slice(),
                            )
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert import_tombstones_by_provider")
                            })?;
                        by_session
                            .insert(rec.erased_session_id.as_str(), idx.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert import_tombstones_by_session")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_import_tombstone", n);
    }

    // 20. agent_session_commit (stored as MarkerValue keyed by (session_id, hash_kind, commit_oid))
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_session_commit").await?;
        let n = {
            require_columns(
                &cols,
                "agent_session_commit",
                &["session_id", "hash_kind", "commit_oid"],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_session_commit",
                    select_cols: "session_id, hash_kind, commit_oid",
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let hk_s: String =
                        col_get(row, 2, source_role, "agent_session_commit", "hash_kind")?;
                    Ok(AgentSessionCommitRecord {
                        session_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_session_commit",
                            "session_id",
                        )?,
                        hash_kind: CaptureHashKind::parse(&hk_s)?,
                        commit_oid: col_get(
                            row,
                            3,
                            source_role,
                            "agent_session_commit",
                            "commit_oid",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_SESSION_COMMIT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_session_commit"))?;
                    let marker = encode_versioned(cv, &MigCaptureMarkerValue)?;
                    for rec in batch {
                        tbl.insert(
                            (
                                rec.session_id.as_str(),
                                rec.hash_kind.as_str(),
                                rec.commit_oid.as_str(),
                            ),
                            marker.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert agent_session_commit"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_session_commit", n);
    }

    // 21. agent_subagent_content_claim
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_subagent_content_claim")
                .await?;
        let n = {
            require_columns(
                &cols,
                "agent_subagent_content_claim",
                &[
                    "parent_session_id",
                    "provider_kind",
                    "source_key",
                    "content_schema_version",
                    "current_revision",
                    "current_checkpoint_id",
                    "current_digest",
                    "state",
                    "attempt_digest",
                    "attempt_checkpoint_id",
                    "owner",
                    "lease_expires_at",
                    "fence_token",
                    "created_at",
                    "updated_at",
                ],
            )?;
            let rc_expr = if cols.contains("revision_cursor") {
                "revision_cursor"
            } else {
                "0"
            };
            let sr_expr = if cols.contains("sync_revision") {
                "sync_revision"
            } else {
                "0"
            };
            let select = format!(
                "parent_session_id, provider_kind, source_key, content_schema_version, current_revision, current_checkpoint_id, current_digest, state, attempt_digest, attempt_checkpoint_id, owner, lease_expires_at, fence_token, created_at, updated_at, {rc_expr}, {sr_expr}"
            );
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_subagent_content_claim",
                    select_cols: &select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let st_s: String =
                        col_get(row, 8, source_role, "agent_subagent_content_claim", "state")?;
                    Ok(AgentSubagentContentClaimRecord {
                        parent_session_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_subagent_content_claim",
                            "parent_session_id",
                        )?,
                        provider_kind: col_get(
                            row,
                            2,
                            source_role,
                            "agent_subagent_content_claim",
                            "provider_kind",
                        )?,
                        source_key: col_get(
                            row,
                            3,
                            source_role,
                            "agent_subagent_content_claim",
                            "source_key",
                        )?,
                        content_schema_version: col_get(
                            row,
                            4,
                            source_role,
                            "agent_subagent_content_claim",
                            "content_schema_version",
                        )?,
                        current_revision: col_get(
                            row,
                            5,
                            source_role,
                            "agent_subagent_content_claim",
                            "current_revision",
                        )?,
                        current_checkpoint_id: col_get(
                            row,
                            6,
                            source_role,
                            "agent_subagent_content_claim",
                            "current_checkpoint_id",
                        )?,
                        current_digest: col_get(
                            row,
                            7,
                            source_role,
                            "agent_subagent_content_claim",
                            "current_digest",
                        )?,
                        state: CaptureSubagentClaimState::parse(&st_s)?,
                        attempt_digest: col_get(
                            row,
                            9,
                            source_role,
                            "agent_subagent_content_claim",
                            "attempt_digest",
                        )?,
                        attempt_checkpoint_id: col_get(
                            row,
                            10,
                            source_role,
                            "agent_subagent_content_claim",
                            "attempt_checkpoint_id",
                        )?,
                        owner: col_get(
                            row,
                            11,
                            source_role,
                            "agent_subagent_content_claim",
                            "owner",
                        )?,
                        lease_expires_at: col_get(
                            row,
                            12,
                            source_role,
                            "agent_subagent_content_claim",
                            "lease_expires_at",
                        )?,
                        fence_token: col_get(
                            row,
                            13,
                            source_role,
                            "agent_subagent_content_claim",
                            "fence_token",
                        )?,
                        created_at: col_get(
                            row,
                            14,
                            source_role,
                            "agent_subagent_content_claim",
                            "created_at",
                        )?,
                        updated_at: col_get(
                            row,
                            15,
                            source_role,
                            "agent_subagent_content_claim",
                            "updated_at",
                        )?,
                        revision_cursor: col_get(
                            row,
                            16,
                            source_role,
                            "agent_subagent_content_claim",
                            "revision_cursor",
                        )?,
                        sync_revision: col_get(
                            row,
                            17,
                            source_role,
                            "agent_subagent_content_claim",
                            "sync_revision",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_SUBAGENT_CONTENT_CLAIM_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_subagent_content_claim"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.parent_session_id.as_str(),
                                rec.provider_kind.as_str(),
                                rec.source_key.as_str(),
                                rec.content_schema_version,
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| {
                            map_redb_storage_err(e, "insert agent_subagent_content_claim")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_subagent_content_claim", n);
    }

    // 22. agent_subagent_content_revision
    {
        let cols = require_sqlite_table_columns_on_conn(
            conn,
            source_role,
            "agent_subagent_content_revision",
        )
        .await?;
        let n = {
            require_columns(
                &cols,
                "agent_subagent_content_revision",
                &[
                    "parent_session_id",
                    "provider_kind",
                    "source_key",
                    "content_schema_version",
                    "revision",
                    "checkpoint_id",
                    "content_digest",
                    "source_channel",
                    "partial",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_subagent_content_revision",
                    select_cols: "parent_session_id, provider_kind, source_key, content_schema_version, revision, checkpoint_id, content_digest, source_channel, partial, created_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let chan_s: String = col_get(
                        row,
                        8,
                        source_role,
                        "agent_subagent_content_revision",
                        "source_channel",
                    )?;
                    let partial: i64 = col_get(
                        row,
                        9,
                        source_role,
                        "agent_subagent_content_revision",
                        "partial",
                    )?;
                    Ok(AgentSubagentContentRevisionRecord {
                        parent_session_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_subagent_content_revision",
                            "parent_session_id",
                        )?,
                        provider_kind: col_get(
                            row,
                            2,
                            source_role,
                            "agent_subagent_content_revision",
                            "provider_kind",
                        )?,
                        source_key: col_get(
                            row,
                            3,
                            source_role,
                            "agent_subagent_content_revision",
                            "source_key",
                        )?,
                        content_schema_version: col_get(
                            row,
                            4,
                            source_role,
                            "agent_subagent_content_revision",
                            "content_schema_version",
                        )?,
                        revision: col_get(
                            row,
                            5,
                            source_role,
                            "agent_subagent_content_revision",
                            "revision",
                        )?,
                        checkpoint_id: col_get(
                            row,
                            6,
                            source_role,
                            "agent_subagent_content_revision",
                            "checkpoint_id",
                        )?,
                        content_digest: col_get(
                            row,
                            7,
                            source_role,
                            "agent_subagent_content_revision",
                            "content_digest",
                        )?,
                        source_channel: CaptureSubagentSourceChannel::parse(&chan_s)?,
                        partial: partial != 0,
                        created_at: col_get(
                            row,
                            10,
                            source_role,
                            "agent_subagent_content_revision",
                            "created_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_SUBAGENT_CONTENT_REVISION_TABLE)
                        .map_err(|e| {
                            map_redb_table_err(e, "open agent_subagent_content_revision")
                        })?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(
                            (
                                rec.parent_session_id.as_str(),
                                rec.provider_kind.as_str(),
                                rec.source_key.as_str(),
                                rec.content_schema_version,
                                rec.revision,
                            ),
                            bytes.as_slice(),
                        )
                        .map_err(|e| {
                            map_redb_storage_err(e, "insert agent_subagent_content_revision")
                        })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_subagent_content_revision", n);
    }

    // 23. agent_subagent_link
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_subagent_link").await?;
        let n = {
            require_columns(
                &cols,
                "agent_subagent_link",
                &[
                    "content_checkpoint_id",
                    "parent_session_id",
                    "link_state",
                    "boundary_checkpoint_id",
                    "stable_subagent_id",
                    "created_at",
                    "updated_at",
                ],
            )?;
            let sr_expr = if cols.contains("sync_revision") {
                "sync_revision"
            } else {
                "0"
            };
            let select = format!(
                "content_checkpoint_id, parent_session_id, link_state, boundary_checkpoint_id, stable_subagent_id, created_at, updated_at, {sr_expr}"
            );
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_subagent_link",
                    select_cols: &select,
                    rowid_expr: "_rowid_",
                },
                |row| {
                    let ls_s: String =
                        col_get(row, 3, source_role, "agent_subagent_link", "link_state")?;
                    Ok(AgentSubagentLinkRecord {
                        content_checkpoint_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_subagent_link",
                            "content_checkpoint_id",
                        )?,
                        parent_session_id: col_get(
                            row,
                            2,
                            source_role,
                            "agent_subagent_link",
                            "parent_session_id",
                        )?,
                        link_state: CaptureSubagentLinkState::parse(&ls_s)?,
                        boundary_checkpoint_id: col_get(
                            row,
                            4,
                            source_role,
                            "agent_subagent_link",
                            "boundary_checkpoint_id",
                        )?,
                        stable_subagent_id: col_get(
                            row,
                            5,
                            source_role,
                            "agent_subagent_link",
                            "stable_subagent_id",
                        )?,
                        created_at: col_get(
                            row,
                            6,
                            source_role,
                            "agent_subagent_link",
                            "created_at",
                        )?,
                        updated_at: col_get(
                            row,
                            7,
                            source_role,
                            "agent_subagent_link",
                            "updated_at",
                        )?,
                        sync_revision: col_get(
                            row,
                            8,
                            source_role,
                            "agent_subagent_link",
                            "sync_revision",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_SUBAGENT_LINK_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_subagent_link"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.content_checkpoint_id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_subagent_link"))?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_subagent_link", n);
    }

    // 24. agent_usage_stats + usage_stats_by_session_event
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_usage_stats").await?;
        let n = {
            require_columns(
                &cols,
                "agent_usage_stats",
                &[
                    "id",
                    "session_id",
                    "thread_id",
                    "agent_run_id",
                    "run_id",
                    "provider",
                    "model",
                    "request_kind",
                    "intent",
                    "prompt_tokens",
                    "completion_tokens",
                    "cached_tokens",
                    "reasoning_tokens",
                    "total_tokens",
                    "tool_call_count",
                    "wall_clock_ms",
                    "provider_latency_ms",
                    "cost_estimate_micro_dollars",
                    "cost_usd",
                    "usage_estimated",
                    "started_at",
                    "finished_at",
                    "success",
                    "error_kind",
                    "schema_version",
                    "created_at",
                    "agent_name",
                    "repo_id",
                    "turn_id",
                    "event_id",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_usage_stats",
                    select_cols: "id, session_id, thread_id, agent_run_id, run_id, provider, model, request_kind, intent, prompt_tokens, completion_tokens, cached_tokens, reasoning_tokens, total_tokens, tool_call_count, wall_clock_ms, provider_latency_ms, cost_estimate_micro_dollars, cost_usd, usage_estimated, started_at, finished_at, success, error_kind, schema_version, created_at, agent_name, repo_id, turn_id, event_id",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    let est: i64 =
                        col_get(row, 20, source_role, "agent_usage_stats", "usage_estimated")?;
                    let ok_i64: i64 =
                        col_get(row, 23, source_role, "agent_usage_stats", "success")?;
                    Ok(AgentUsageStatsRecord {
                        id: col_get(row, 1, source_role, "agent_usage_stats", "id")?,
                        session_id: col_get(
                            row,
                            2,
                            source_role,
                            "agent_usage_stats",
                            "session_id",
                        )?,
                        thread_id: col_get(row, 3, source_role, "agent_usage_stats", "thread_id")?,
                        agent_run_id: col_get(
                            row,
                            4,
                            source_role,
                            "agent_usage_stats",
                            "agent_run_id",
                        )?,
                        run_id: col_get(row, 5, source_role, "agent_usage_stats", "run_id")?,
                        provider: col_get(row, 6, source_role, "agent_usage_stats", "provider")?,
                        model: col_get(row, 7, source_role, "agent_usage_stats", "model")?,
                        request_kind: col_get(
                            row,
                            8,
                            source_role,
                            "agent_usage_stats",
                            "request_kind",
                        )?,
                        intent: col_get(row, 9, source_role, "agent_usage_stats", "intent")?,
                        prompt_tokens: col_get(
                            row,
                            10,
                            source_role,
                            "agent_usage_stats",
                            "prompt_tokens",
                        )?,
                        completion_tokens: col_get(
                            row,
                            11,
                            source_role,
                            "agent_usage_stats",
                            "completion_tokens",
                        )?,
                        cached_tokens: col_get(
                            row,
                            12,
                            source_role,
                            "agent_usage_stats",
                            "cached_tokens",
                        )?,
                        reasoning_tokens: col_get(
                            row,
                            13,
                            source_role,
                            "agent_usage_stats",
                            "reasoning_tokens",
                        )?,
                        total_tokens: col_get(
                            row,
                            14,
                            source_role,
                            "agent_usage_stats",
                            "total_tokens",
                        )?,
                        tool_call_count: col_get(
                            row,
                            15,
                            source_role,
                            "agent_usage_stats",
                            "tool_call_count",
                        )?,
                        wall_clock_ms: col_get(
                            row,
                            16,
                            source_role,
                            "agent_usage_stats",
                            "wall_clock_ms",
                        )?,
                        provider_latency_ms: col_get(
                            row,
                            17,
                            source_role,
                            "agent_usage_stats",
                            "provider_latency_ms",
                        )?,
                        cost_estimate_micro_dollars: col_get(
                            row,
                            18,
                            source_role,
                            "agent_usage_stats",
                            "cost_estimate_micro_dollars",
                        )?,
                        cost_usd: col_get(row, 19, source_role, "agent_usage_stats", "cost_usd")?,
                        usage_estimated: est != 0,
                        started_at: col_get(
                            row,
                            21,
                            source_role,
                            "agent_usage_stats",
                            "started_at",
                        )?,
                        finished_at: col_get(
                            row,
                            22,
                            source_role,
                            "agent_usage_stats",
                            "finished_at",
                        )?,
                        success: ok_i64 != 0,
                        error_kind: col_get(
                            row,
                            24,
                            source_role,
                            "agent_usage_stats",
                            "error_kind",
                        )?,
                        schema_version: col_get(
                            row,
                            25,
                            source_role,
                            "agent_usage_stats",
                            "schema_version",
                        )?,
                        created_at: col_get(
                            row,
                            26,
                            source_role,
                            "agent_usage_stats",
                            "created_at",
                        )?,
                        agent_name: col_get(
                            row,
                            27,
                            source_role,
                            "agent_usage_stats",
                            "agent_name",
                        )?,
                        repo_id: col_get(row, 28, source_role, "agent_usage_stats", "repo_id")?,
                        turn_id: col_get(row, 29, source_role, "agent_usage_stats", "turn_id")?,
                        event_id: col_get(row, 30, source_role, "agent_usage_stats", "event_id")?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_USAGE_STATS_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_usage_stats"))?;
                    let mut by_se = wtx
                        .open_table(redb_cap::USAGE_STATS_BY_SESSION_EVENT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open usage_stats_by_session_event"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.id.as_str(), bytes.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert agent_usage_stats"))?;
                        if let (Some(sid), Some(eid)) = (&rec.session_id, &rec.event_id) {
                            let idx = encode_versioned(
                                cv,
                                &MigUsageStatsIndexEntry {
                                    usage_id: rec.id.clone(),
                                },
                            )?;
                            if by_se
                                .insert((sid.as_str(), eid.as_str()), idx.as_slice())
                                .map_err(|e| {
                                    map_redb_storage_err(e, "insert usage_stats_by_session_event")
                                })?
                                .is_some()
                            {
                                return Err(StorageBackendError::BackendInternal {
                                    detail: "duplicate (session_id, event_id) in sqlite agent_usage_stats".to_string(),
                                });
                            }
                        }
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_usage_stats", n);
    }

    // 25. agent_workspace_scope_audit
    {
        let cols =
            require_sqlite_table_columns_on_conn(conn, source_role, "agent_workspace_scope_audit")
                .await?;
        let n = {
            require_columns(
                &cols,
                "agent_workspace_scope_audit",
                &[
                    "audit_id",
                    "action",
                    "agent_kind",
                    "provider_session_id",
                    "repo_id",
                    "worktree_id",
                    "workspace_id",
                    "workspace_fence",
                    "actor",
                    "created_at",
                ],
            )?;
            stream_sqlite_table_to_redb(
                conn,
                db,
                StreamTableSpec {
                    source_role,
                    target_role: tr,
                    table: "agent_workspace_scope_audit",
                    select_cols: "audit_id, action, agent_kind, provider_session_id, repo_id, worktree_id, workspace_id, workspace_fence, actor, created_at",
                    rowid_expr: "_rowid_",
                },

                |row| {
                    Ok(AgentWorkspaceScopeAuditRecord {
                        audit_id: col_get(
                            row,
                            1,
                            source_role,
                            "agent_workspace_scope_audit",
                            "audit_id",
                        )?,
                        action: col_get(
                            row,
                            2,
                            source_role,
                            "agent_workspace_scope_audit",
                            "action",
                        )?,
                        agent_kind: col_get(
                            row,
                            3,
                            source_role,
                            "agent_workspace_scope_audit",
                            "agent_kind",
                        )?,
                        provider_session_id: col_get(
                            row,
                            4,
                            source_role,
                            "agent_workspace_scope_audit",
                            "provider_session_id",
                        )?,
                        repo_id: col_get(
                            row,
                            5,
                            source_role,
                            "agent_workspace_scope_audit",
                            "repo_id",
                        )?,
                        worktree_id: col_get(
                            row,
                            6,
                            source_role,
                            "agent_workspace_scope_audit",
                            "worktree_id",
                        )?,
                        workspace_id: col_get(
                            row,
                            7,
                            source_role,
                            "agent_workspace_scope_audit",
                            "workspace_id",
                        )?,
                        workspace_fence: col_get(
                            row,
                            8,
                            source_role,
                            "agent_workspace_scope_audit",
                            "workspace_fence",
                        )?,
                        actor: col_get(
                            row,
                            9,
                            source_role,
                            "agent_workspace_scope_audit",
                            "actor",
                        )?,
                        created_at: col_get(
                            row,
                            10,
                            source_role,
                            "agent_workspace_scope_audit",
                            "created_at",
                        )?,
                    })
                },
                |wtx, batch| {
                    let mut tbl = wtx
                        .open_table(redb_cap::AGENT_WORKSPACE_SCOPE_AUDIT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_workspace_scope_audit"))?;
                    for rec in batch {
                        let bytes = encode_versioned(cv, rec)?;
                        tbl.insert(rec.audit_id.as_str(), bytes.as_slice())
                            .map_err(|e| {
                                map_redb_storage_err(e, "insert agent_workspace_scope_audit")
                            })?;
                    }
                    Ok(())
                },
                max_batch_rows,
            )
            .await?
        };
        counts.insert("agent_workspace_scope_audit", n);
    }

    Ok(())
}

/// Owns a staged `.tmp` redb handle alongside its cleanup guard with strict
/// drop-order guarantee: `db` is declared BEFORE `guard` so Rust unconditionally
/// closes the underlying file handle BEFORE `guard`'s `Drop` attempts cleanup
/// on EVERY exit path (second-file creation failure via `?`, export error,
/// panic, or async future cancellation).
struct StagedRedbTmp {
    db: Option<RedbDatabase>,
    guard: TmpRedbFileGuard,
}

impl StagedRedbTmp {
    fn new(db: RedbDatabase, guard: TmpRedbFileGuard) -> Self {
        Self {
            db: Some(db),
            guard,
        }
    }

    fn db(&self) -> StorageResult<&RedbDatabase> {
        self.db
            .as_ref()
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "staged redb temporary database handle already closed".to_string(),
            })
    }

    fn close_and_verify_disarm(mut self, returned_path: &Path) -> StorageResult<()> {
        drop(self.db.take());
        self.guard.verify_still_bound(returned_path)?;
        self.guard.disarm();
        Ok(())
    }
}

/// Forward migration Phase 1 streaming export from the quiesced single-connection
/// `c_mig` SQLite source(s) to staged `.libra/redb/git.gen-<id>.redb.tmp` and
/// `.libra/private/capture.gen-<id>.redb.tmp` (`ADR-RB-07` #1, `RB-10a`).
///
/// Guarantees:
/// - **Zero extra live `.db` file descriptors:** all reads execute strictly over
///   the already quiesced `guard.sqlite.git` (and optional `guard.sqlite.capture`)
///   `c_mig` connections, preserving SQLite's OS `EXCLUSIVE` locks continuously.
/// - **Read-only 3-layout detection & fail-closed Layout B guard:** classifies
///   Layout A (unsplit `libra.db` 61+23), Layout B (dual DBs with inline tables
///   verified against `capture_catalog_copy_receipt` + 100% PK coverage), and
///   Layout C (detached `libra.db` 61 + `capture.db` 25) BEFORE creating any
///   `.tmp` file or staging directory.
/// - **Bounded streaming batches (`<= 500` rows/commit):** never loads full
///   tables into memory and keeps every secondary index and monotonic sequence
///   counter wire-compatible with `RedbConfigStore`, `RedbGitStore`,
///   `RedbGitStateStore`, and `RedbCaptureStore`.
/// - **Atomic `.tmp` cleanup on failure:** drops redb handles and unlinks only
///   the exact created `.tmp` inodes if any step fails, leaving source SQLite
///   files untouched.
pub async fn export_forward_phase1_to_redb_tmp(
    guard: &mut ForwardPhase1QuiesceGuard,
    common_storage: &Path,
    generation: u64,
) -> StorageResult<ForwardPhase1ExportReport> {
    if generation == 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "forward migration Phase 1 export requires generation >= 1".to_string(),
        });
    }
    if guard.sqlite.git.role() != DatabaseRole::Repository
        || guard.sqlite.git.journal_mode() != "delete"
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "forward migration Phase 1 export requires a DELETE-mode Repository quiesced connection".to_string(),
        });
    }
    if guard.sqlite.git.path() != git_sqlite_db_path(common_storage) {
        return Err(StorageBackendError::BackendInternal {
            detail:
                "forward migration Phase 1 guard does not match target repository storage directory"
                    .to_string(),
        });
    }
    if let Some(cap) = guard.sqlite.capture.as_ref() {
        if cap.role() != DatabaseRole::Capture || cap.journal_mode() != "delete" {
            return Err(StorageBackendError::BackendInternal {
                detail: "forward migration Phase 1 export requires a DELETE-mode Capture quiesced connection".to_string(),
            });
        }
        if cap.path() != capture_sqlite_db_path(common_storage) {
            return Err(StorageBackendError::BackendInternal {
                detail: "forward migration Phase 1 capture guard does not match target repository storage directory".to_string(),
            });
        }
    }

    // Step 1: Strictly read-only 3-layout classification, Layout B receipt +
    // PK coverage verification, and required source-table inventory check
    // BEFORE creating any staging directory or `.tmp` file.
    let layout = inspect_legacy_repository_layout(&mut guard.sqlite).await?;
    validate_forward_source_table_inventory(&mut guard.sqlite, layout).await?;

    let git_tmp_path = git_redb_tmp_path(common_storage, generation);
    let capture_tmp_path = capture_redb_tmp_path(common_storage, generation);
    let git_tmp_name = git_tmp_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "failed to format staged git redb filename".to_string(),
        })?;
    let cap_tmp_name = capture_tmp_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "failed to format staged capture redb filename".to_string(),
        })?;

    // Step 2: Create both staged `.tmp` redb files wrapped in `StagedRedbTmp`
    // owners that guarantee `db` drops BEFORE `guard` on EVERY exit path
    // (second-file creation `?`, export error, panic, or async cancellation).
    let (git_db, git_tmp_guard) = create_redb_tmp_database(
        common_storage,
        REDB_SUBDIR,
        git_tmp_name,
        0o755,
        0o644,
        false,
        DatabaseRole::Repository,
    )?;
    let git_staged = StagedRedbTmp::new(git_db, git_tmp_guard);
    let (cap_db, cap_tmp_guard) = create_redb_tmp_database(
        common_storage,
        PRIVATE_SUBDIR,
        cap_tmp_name,
        0o700,
        0o600,
        true,
        DatabaseRole::Capture,
    )?;
    let cap_staged = StagedRedbTmp::new(cap_db, cap_tmp_guard);

    let export_result = async {
        let git_db_ref = git_staged.db()?;
        let cap_db_ref = cap_staged.db()?;
        initialize_git_export_redb_schema(git_db_ref)?;
        let seeded_cap_sv = initialize_capture_export_redb_schema(
            cap_db_ref,
            layout == LegacyRepositoryLayout::LayoutA,
        )?;

        let mut git_table_counts = BTreeMap::new();
        let mut capture_table_counts = BTreeMap::new();
        let mut max_batch_rows = 0usize;

        // Export all 61 active repository tables from `libra.db`.
        export_repository_tables_to_redb(
            &mut guard.sqlite.git.conn,
            git_db_ref,
            &mut git_table_counts,
            &mut max_batch_rows,
        )
        .await?;

        // Export capture tables according to verified layout:
        // - Layout A: 23 inline `agent_*` tables from `libra.db` (+ seeded capture schema receipt).
        // - Layouts B & C: all 25 tables from authoritative `private/capture.db`.
        match layout {
            LegacyRepositoryLayout::LayoutA => {
                export_capture_tables_to_redb(
                    &mut guard.sqlite.git.conn,
                    CaptureExportOptions {
                        source_role: DatabaseRole::Repository,
                        include_capture_ledger_and_receipt: false,
                        seeded_schema_versions_count: seeded_cap_sv,
                        inline_bridge_event_seq_floor: 0,
                        inline_bridge_link_seq_floor: 0,
                    },
                    cap_db_ref,
                    &mut capture_table_counts,
                    &mut max_batch_rows,
                )
                .await?;
            }
            LegacyRepositoryLayout::LayoutB | LegacyRepositoryLayout::LayoutC => {
                // For Layout B, the legacy catalog copy copied rows from `libra.db`
                // into `capture.db` without copying `sqlite_sequence`. Read both
                // held connections' high-water marks so capture sequences never
                // move backwards even when inline rows were deleted before copy.
                let (inline_event_floor, inline_link_floor) =
                    if layout == LegacyRepositoryLayout::LayoutB {
                        (
                            read_sqlite_sequence_highwater(
                                &mut guard.sqlite.git.conn,
                                DatabaseRole::Repository,
                                "agent_bridge_event",
                            )
                            .await?,
                            read_sqlite_sequence_highwater(
                                &mut guard.sqlite.git.conn,
                                DatabaseRole::Repository,
                                "agent_bridge_link",
                            )
                            .await?,
                        )
                    } else {
                        (0, 0)
                    };
                let cap_conn = guard.sqlite.capture.as_mut().ok_or_else(|| {
                    StorageBackendError::BackendInternal {
                        detail: "Layout B/C export missing quiesced capture connection".to_string(),
                    }
                })?;
                export_capture_tables_to_redb(
                    &mut cap_conn.conn,
                    CaptureExportOptions {
                        source_role: DatabaseRole::Capture,
                        include_capture_ledger_and_receipt: true,
                        seeded_schema_versions_count: 0,
                        inline_bridge_event_seq_floor: inline_event_floor,
                        inline_bridge_link_seq_floor: inline_link_floor,
                    },
                    cap_db_ref,
                    &mut capture_table_counts,
                    &mut max_batch_rows,
                )
                .await?;
            }
        }

        if git_table_counts.len() != REPOSITORY_ACTIVE_SOURCE_TABLES_COUNT {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration Phase 1 exported {} repository tables, expected {REPOSITORY_ACTIVE_SOURCE_TABLES_COUNT}",
                    git_table_counts.len()
                ),
            });
        }
        if capture_table_counts.len() != CAPTURE_SOURCE_TABLES_COUNT {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration Phase 1 exported {} capture tables, expected {CAPTURE_SOURCE_TABLES_COUNT}",
                    capture_table_counts.len()
                ),
            });
        }

        Ok(ForwardPhase1ExportReport {
            layout,
            generation,
            git_tmp_path,
            capture_tmp_path,
            git_table_counts,
            capture_table_counts,
            max_batch_rows,
        })
    }
    .await;

    match export_result {
        Ok(report) => {
            // Close both `RedbDatabase` handles first, re-verify that both
            // returned pathnames still name the exact created inodes/file-IDs,
            // and only then disarm their cleanup guards.
            cap_staged.close_and_verify_disarm(&report.capture_tmp_path)?;
            git_staged.close_and_verify_disarm(&report.git_tmp_path)?;
            Ok(report)
        }
        Err(err) => Err(err),
    }
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
    use crate::internal::maintenance_lock::MaintenanceLock;

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
        // Non-SQLite OS lock-holder and busy-probe modes for `RB-10n`.
        match mode {
            "hold_flock_exclusive" => {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(db_path)
                    .expect("child open flock target");
                file.try_lock().expect("child acquire exclusive flock");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let mut sink = Vec::new();
                let _ = io::stdin().read_to_end(&mut sink);
                drop(file);
                return;
            }
            "hold_flock_blocking" => {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(false)
                    .open(db_path)
                    .expect("child open existing flock target for blocking lock");
                println!("LIBRA_RB10_OPENED");
                let _ = io::stdout().flush();
                file.lock().expect("child acquire blocking exclusive flock");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let mut sink = Vec::new();
                let _ = io::stdin().read_to_end(&mut sink);
                drop(file);
                return;
            }
            "hold_flock_shared" => {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(db_path)
                    .expect("child open shared flock target");
                file.try_lock_shared().expect("child acquire shared flock");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let mut sink = Vec::new();
                let _ = io::stdin().read_to_end(&mut sink);
                drop(file);
                return;
            }
            "probe_flock_busy" => {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(false)
                    .open(db_path)
                    .expect("child open existing flock target");
                match file.try_lock() {
                    Err(fs::TryLockError::WouldBlock) => {
                        println!("{RB10_BUSY_MARKER}");
                        let _ = io::stdout().flush();
                    }
                    Ok(()) => panic!("expected WouldBlock on held flock, acquired lock instead"),
                    Err(fs::TryLockError::Error(err)) => {
                        panic!("unexpected flock probe error: {err}")
                    }
                }
                return;
            }
            "hold_maintenance_shared" => {
                let guard = MaintenanceLock::shared(db_path)
                    .expect("child acquire shared maintenance.lock");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let mut sink = Vec::new();
                let _ = io::stdin().read_to_end(&mut sink);
                drop(guard);
                return;
            }
            "hold_maintenance_exclusive" => {
                let guard = MaintenanceLock::try_exclusive(db_path, Duration::ZERO)
                    .expect("child try_exclusive maintenance.lock")
                    .expect("child must acquire uncontended maintenance.lock");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let mut sink = Vec::new();
                let _ = io::stdin().read_to_end(&mut sink);
                drop(guard);
                return;
            }
            "probe_maintenance_shared_busy" => {
                let lock_path = db_path.join("maintenance.lock");
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(false)
                    .open(&lock_path)
                    .expect("child open existing maintenance.lock");
                match file.try_lock_shared() {
                    Err(fs::TryLockError::WouldBlock) => {
                        println!("{RB10_BUSY_MARKER}");
                        let _ = io::stdout().flush();
                    }
                    Ok(()) => {
                        panic!(
                            "expected WouldBlock on shared maintenance.lock probe, acquired lock"
                        )
                    }
                    Err(fs::TryLockError::Error(err)) => {
                        panic!("unexpected maintenance.lock probe error: {err}")
                    }
                }
                return;
            }
            _ => {}
        }

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

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum SnapshotNode {
        Dir,
        File(Vec<u8>),
        Symlink(PathBuf),
    }

    fn snapshot_directory_tree(root: &Path) -> std::collections::BTreeMap<PathBuf, SnapshotNode> {
        let mut out = std::collections::BTreeMap::new();
        if !root.exists() && fs::symlink_metadata(root).is_err() {
            return out;
        }
        fn walk(
            base: &Path,
            dir: &Path,
            out: &mut std::collections::BTreeMap<PathBuf, SnapshotNode>,
        ) {
            let entries = fs::read_dir(dir).expect("read directory for snapshot");
            for entry in entries {
                let entry = entry.expect("directory entry for snapshot");
                let path = entry.path();
                let rel = path
                    .strip_prefix(base)
                    .expect("strip snapshot base")
                    .to_path_buf();
                let meta = fs::symlink_metadata(&path).expect("lstat entry for snapshot");
                if meta.file_type().is_symlink() {
                    let target = fs::read_link(&path).expect("read symlink target for snapshot");
                    out.insert(rel, SnapshotNode::Symlink(target));
                } else if meta.file_type().is_dir() {
                    out.insert(rel, SnapshotNode::Dir);
                    walk(base, &path, out);
                } else {
                    let bytes = fs::read(&path).expect("read file bytes for snapshot");
                    out.insert(rel, SnapshotNode::File(bytes));
                }
            }
        }
        walk(root, root, &mut out);
        out
    }

    #[cfg(unix)]
    fn file_dev_ino(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt as _;
        let meta = fs::symlink_metadata(path).expect("lstat path for dev/ino");
        (meta.dev(), meta.ino())
    }

    async fn create_full_rb10n_fixture_repo(
        storage: &Path,
        create_writer_lock: bool,
        create_capacity_lock: bool,
    ) {
        use crate::storage::{
            capture_store::CaptureQueueLane,
            queue::{FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY, append_frame, init_capture_queue},
        };

        let git_db = git_sqlite_db_path(storage);
        let capture_db = capture_sqlite_db_path(storage);
        create_delete_mode_fixture_db(&git_db).await;
        create_delete_mode_fixture_db(&capture_db).await;

        let private_dir = storage.join(PRIVATE_SUBDIR);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&private_dir, fs::Permissions::from_mode(0o700))
                .expect("chmod 0700 private");
        }

        // Populate legacy `capture-spool/` with ready/ and quarantine/ events.
        let spool_dir = capture_spool_dir_path(storage);
        let spool_tmp = capture_spool_tmp_dir_path(storage);
        let spool_ready = spool_dir.join("ready");
        let spool_quarantine = spool_dir.join("quarantine");
        for d in [&spool_dir, &spool_tmp, &spool_ready, &spool_quarantine] {
            fs::create_dir_all(d).expect("create capture-spool subdir");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(d, fs::Permissions::from_mode(0o700))
                    .expect("chmod 0700 spool subdir");
            }
        }
        fs::write(
            spool_ready.join("1-0000000000000001-0001~dedup-ready-1"),
            b"{\"event\":\"ready-1\"}",
        )
        .expect("seed spool ready event");
        fs::write(
            spool_quarantine.join("1-0000000000000002-0001~dedup-quar-1"),
            b"{\"event\":\"quarantine-1\"}",
        )
        .expect("seed spool quarantine event");
        if create_capacity_lock {
            let cap_lock = capture_spool_capacity_lock_path(storage);
            fs::write(&cap_lock, b"").expect("seed capture-spool/capacity.lock");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&cap_lock, fs::Permissions::from_mode(0o600))
                    .expect("chmod 0600 capacity.lock");
            }
        }

        // Populate `capture-queue/` with genuine dual-lane LTM2 + committed frames.
        init_capture_queue(storage).expect("init capture-queue fixture");
        append_frame(
            storage,
            CaptureQueueLane::Priority,
            FRAME_FLAG_PRIORITY,
            "rb10n-prio-1",
            b"priority-payload-bytes",
        )
        .expect("append priority frame");
        append_frame(
            storage,
            CaptureQueueLane::Normal,
            FRAME_FLAG_NONE,
            "rb10n-norm-1",
            b"normal-payload-bytes",
        )
        .expect("append normal frame");
        if create_writer_lock {
            let writer_lock = capture_queue_writer_lock_path(storage);
            fs::write(&writer_lock, b"").expect("seed capture-queue/writer.lock");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&writer_lock, fs::Permissions::from_mode(0o600))
                    .expect("chmod 0600 writer.lock");
            }
        }
    }

    #[tokio::test]
    async fn forward_phase1_rejects_inflight_spool_tmp_and_queue_append_lock() {
        // Test BOTH when optional lock files (`writer.lock`, `capacity.lock`)
        // already exist AND when they are initially absent so AC3/AC4/AC7/AC8
        // prove zero file creation or byte mutation in `capture-spool/` and
        // `capture-queue/` under every realistic repository state.
        for (create_writer_lock, create_capacity_lock, variant_label) in [
            (true, true, "all-locks-present"),
            (false, false, "writer-and-capacity-locks-absent"),
        ] {
            // =================================================================
            // AC1-AC4: Active unexpired `capture-spool/tmp/` in-flight file
            // (simulating `spool.rs:119-145` after `capacity.lock` is released).
            // =================================================================
            {
                let dir = tempdir().expect("create RB-10n tmp contention dir");
                let storage = dir.path().join(".libra");
                create_full_rb10n_fixture_repo(&storage, create_writer_lock, create_capacity_lock)
                    .await;

                let git_db = git_sqlite_db_path(&storage);
                let capture_db = capture_sqlite_db_path(&storage);
                let spool_dir = capture_spool_dir_path(&storage);
                let queue_dir = capture_queue_dir_path(&storage);
                let inflight_tmp =
                    capture_spool_tmp_dir_path(&storage).join("1-0000000000000099-0001~inflight");
                fs::write(&inflight_tmp, b"{\"partial_envelope\":true}")
                    .expect("create unexpired in-flight capture-spool/tmp file");

                let before_git = fs::read(&git_db).expect("snapshot libra.db before AC1");
                let before_cap = fs::read(&capture_db).expect("snapshot capture.db before AC1");
                let before_spool = snapshot_directory_tree(&spool_dir);
                let before_queue = snapshot_directory_tree(&queue_dir);

                // AC1: must fail closed with `LBR-IO-001` (and zero path leakage).
                let err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("unexpired capture-spool/tmp file must fail closed");
                assert_eq!(
                    err.stable_code_str(),
                    "LBR-IO-001",
                    "AC1 ({variant_label}): expected LBR-IO-001, got {err}"
                );
                let rendered = err.to_string();
                assert!(
                    rendered.contains("capture-spool/tmp"),
                    "AC1 ({variant_label}): diagnostic must mention capture-spool/tmp, got: {rendered}"
                );
                assert!(
                    !rendered.contains(&storage.to_string_lossy().into_owned()),
                    "AC1 ({variant_label}): error must not leak private storage path: {rendered}"
                );

                // AC2: source SQLite `.db` files byte-for-byte identical.
                assert_eq!(
                    fs::read(&git_db).expect("snapshot libra.db after AC1"),
                    before_git,
                    "AC2 ({variant_label}): libra.db must be byte-for-byte unchanged"
                );
                assert_eq!(
                    fs::read(&capture_db).expect("snapshot capture.db after AC1"),
                    before_cap,
                    "AC2 ({variant_label}): capture.db must be byte-for-byte unchanged"
                );

                // AC3: `capture-spool/` full directory tree & file bytes identical.
                assert_eq!(
                    snapshot_directory_tree(&spool_dir),
                    before_spool,
                    "AC3 ({variant_label}): capture-spool/ must be byte-for-byte unchanged"
                );

                // AC4: `capture-queue/` full directory tree & file bytes identical.
                assert_eq!(
                    snapshot_directory_tree(&queue_dir),
                    before_queue,
                    "AC4 ({variant_label}): capture-queue/ must be byte-for-byte unchanged"
                );

                // Clock-skew regression: a future-timestamped `tmp/` file must
                // ALSO be treated as unexpired / in-flight and rejected.
                let future_file = OpenOptions::new()
                    .write(true)
                    .open(&inflight_tmp)
                    .expect("open inflight tmp for future mtime");
                future_file
                    .set_modified(SystemTime::now() + Duration::from_secs(3600))
                    .expect("set future mtime on inflight tmp");
                drop(future_file);
                let future_err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("future-mtime capture-spool/tmp file must fail closed");
                assert_eq!(future_err.stable_code_str(), "LBR-IO-001");
                assert_eq!(fs::read(&git_db).expect("git after future"), before_git);
                assert_eq!(fs::read(&capture_db).expect("cap after future"), before_cap);
                assert_eq!(snapshot_directory_tree(&spool_dir), before_spool);
                assert_eq!(snapshot_directory_tree(&queue_dir), before_queue);
            }

            // =================================================================
            // AC5-AC8: External OS process holding `capture-queue/append.lock`
            // (tested with BOTH a shared lock `try_lock_shared()` AND an
            // exclusive lock `try_lock()` holder!).
            // =================================================================
            for (append_mode, lock_kind_label) in [
                ("hold_flock_shared", "shared-holder"),
                ("hold_flock_exclusive", "exclusive-holder"),
            ] {
                let dir = tempdir().expect("create RB-10n append.lock contention dir");
                let storage = dir.path().join(".libra");
                create_full_rb10n_fixture_repo(&storage, create_writer_lock, create_capacity_lock)
                    .await;

                let git_db = git_sqlite_db_path(&storage);
                let capture_db = capture_sqlite_db_path(&storage);
                let spool_dir = capture_spool_dir_path(&storage);
                let queue_dir = capture_queue_dir_path(&storage);
                let append_lock = capture_queue_append_lock_path(&storage);

                let holder = ChildLockHolder::spawn(append_mode, &append_lock);

                let before_git = fs::read(&git_db).expect("snapshot libra.db before AC5");
                let before_cap = fs::read(&capture_db).expect("snapshot capture.db before AC5");
                let before_queue = snapshot_directory_tree(&queue_dir);
                let before_spool = snapshot_directory_tree(&spool_dir);

                // AC5: must fail closed with `LBR-IO-001` (and zero path leakage).
                let err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("contended capture-queue/append.lock must fail closed");
                assert_eq!(
                    err.stable_code_str(),
                    "LBR-IO-001",
                    "AC5 ({variant_label}/{lock_kind_label}): expected LBR-IO-001, got {err}"
                );
                let rendered = err.to_string();
                assert!(
                    rendered.contains("append.lock"),
                    "AC5 ({variant_label}/{lock_kind_label}): diagnostic must mention append.lock, got: {rendered}"
                );
                assert!(
                    !rendered.contains(&storage.to_string_lossy().into_owned()),
                    "AC5 ({variant_label}/{lock_kind_label}): error must not leak private storage path: {rendered}"
                );

                // AC6: source SQLite `.db` files byte-for-byte identical.
                assert_eq!(
                    fs::read(&git_db).expect("snapshot libra.db after AC5"),
                    before_git,
                    "AC6 ({variant_label}/{lock_kind_label}): libra.db must be byte-for-byte unchanged"
                );
                assert_eq!(
                    fs::read(&capture_db).expect("snapshot capture.db after AC5"),
                    before_cap,
                    "AC6 ({variant_label}/{lock_kind_label}): capture.db must be byte-for-byte unchanged"
                );

                // AC7: `capture-queue/` full directory tree & file bytes identical.
                assert_eq!(
                    snapshot_directory_tree(&queue_dir),
                    before_queue,
                    "AC7 ({variant_label}/{lock_kind_label}): capture-queue/ must be byte-for-byte unchanged"
                );

                // AC8: `capture-spool/` full directory tree & file bytes identical.
                assert_eq!(
                    snapshot_directory_tree(&spool_dir),
                    before_spool,
                    "AC8 ({variant_label}/{lock_kind_label}): capture-spool/ must be byte-for-byte unchanged"
                );

                holder.release_and_wait();
            }
        }

        // =====================================================================
        // Codex R2 P1 regression: NEVER unlink a newly created lock inode on
        // late rejection (zero POSIX lock-inode split-brain).
        //
        // Start with `capacity.lock` absent; while migration holds the newly
        // created `capacity.lock` just before a late rejection (`tmp/` race),
        // spawn waiting Producer 1 (`hold_flock_blocking`) that opens that
        // exact inode and blocks on `flock(LOCK_EX)`. When migration fails
        // closed (`LBR-IO-001`) and drops its lock WITHOUT unlinking
        // `capacity.lock`, Producer 1 acquires the lock on the live canonical
        // inode, and competing Producer 2 (`probe_flock_busy`) is strictly
        // blocked (`WouldBlock`) on the exact same inode!
        // =====================================================================
        {
            let dir = tempdir().expect("create RB-10n no-split-brain dir");
            let storage = dir.path().join(".libra");
            create_full_rb10n_fixture_repo(&storage, false, false).await;

            let git_db = git_sqlite_db_path(&storage);
            let capture_db = capture_sqlite_db_path(&storage);
            let before_git = fs::read(&git_db).expect("snap git before late race");
            let before_cap = fs::read(&capture_db).expect("snap cap before late race");

            let waiter_child: std::sync::Mutex<
                Option<(Child, ChildStdin, BufReader<ChildStdout>)>,
            > = std::sync::Mutex::new(None);
            let created_ino: std::sync::Mutex<Option<(u64, u64)>> = std::sync::Mutex::new(None);

            let spawn_waiting_producer_and_plant_tmp = |s: &Path| {
                let cap_lock = capture_spool_capacity_lock_path(s);
                assert!(
                    cap_lock.exists(),
                    "capacity.lock must already be created and locked in canonical order"
                );
                #[cfg(unix)]
                {
                    *created_ino.lock().expect("lock created_ino") = Some(file_dev_ino(&cap_lock));
                }
                // Spawn Producer 1 which opens the exact `capacity.lock` inode,
                // emits `LIBRA_RB10_OPENED`, and then blocks in `file.lock()`
                // waiting for migration to release it.
                let exe = std::env::current_exe().expect("locate test binary");
                let mut child = Command::new(exe)
                    .arg("--exact")
                    .arg("storage::migrate::tests::forward_phase1_rejects_writer_and_delete_active_reader")
                    .arg("--nocapture")
                    .env(RB10_CHILD_MODE_ENV, "hold_flock_blocking")
                    .env(RB10_CHILD_DB_ENV, &cap_lock)
                    .env("LIBRA_TEST", "1")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("spawn waiting Producer 1");
                let stdin = child.stdin.take().expect("capture waiter stdin");
                let stdout = child.stdout.take().expect("capture waiter stdout");
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    let n = reader.read_line(&mut line).expect("read waiter stdout");
                    assert!(n > 0, "waiting Producer 1 exited before LIBRA_RB10_OPENED");
                    if line.trim() == "LIBRA_RB10_OPENED" {
                        break;
                    }
                }
                *waiter_child.lock().expect("lock waiter_child") = Some((child, stdin, reader));

                // Trigger late rejection via an unexpired `capture-spool/tmp/` file.
                let raced_tmp =
                    capture_spool_tmp_dir_path(s).join("1-0000000000000099-0002~raced-inflight");
                fs::write(&raced_tmp, b"{\"raced_inflight\":true}").expect("plant raced tmp file");
            };

            let race_err = quiesce_forward_repository_phase1_inner(
                &storage,
                &Phase1NonSqliteTestHooks {
                    after_locks_before_tmp_recheck: Some(&spawn_waiting_producer_and_plant_tmp),
                },
            )
            .await
            .expect_err("late raced capture-spool/tmp file must fail closed");
            assert_eq!(race_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(fs::read(&git_db).expect("git after late race"), before_git);
            assert_eq!(
                fs::read(&capture_db).expect("cap after late race"),
                before_cap
            );

            // Migration released `capacity.lock` WITHOUT unlinking it -> waiting
            // Producer 1 now acquires the exclusive lock on that exact inode.
            let (mut child, stdin, mut reader) = waiter_child
                .lock()
                .expect("lock waiter_child")
                .take()
                .expect("waiting Producer 1 handle present");
            let mut line = String::new();
            loop {
                line.clear();
                let n = reader
                    .read_line(&mut line)
                    .expect("read Producer 1 READY marker");
                assert!(n > 0, "Producer 1 exited before acquiring capacity.lock");
                if line.trim() == RB10_READY_MARKER {
                    break;
                }
            }

            let cap_lock = capture_spool_capacity_lock_path(&storage);
            assert!(
                cap_lock.exists(),
                "capacity.lock must remain present at its canonical path (never unlinked)"
            );
            #[cfg(unix)]
            assert_eq!(
                Some(file_dev_ino(&cap_lock)),
                *created_ino.lock().expect("lock created_ino"),
                "capacity.lock on disk must be the exact same inode held by Producer 1"
            );

            // Competing subsequent Producer 2 probing `capacity.lock` MUST be
            // blocked (`WouldBlock`), proving zero lock-inode split-brain!
            run_child_busy_probe("probe_flock_busy", &cap_lock);

            // Cleanly release Producer 1.
            drop(stdin);
            let mut sink = String::new();
            let _ = reader.read_to_string(&mut sink);
            let status = child.wait().expect("wait Producer 1");
            assert!(status.success(), "Producer 1 exited abnormally: {status:?}");
        }

        // =====================================================================
        // Codex R1 Findings 2 & 3 regressions:
        // - Symlink and hard-linked `maintenance.lock` fail closed `LBR-IO-001`
        // - Same-process `MaintenanceLock::shared` contention fails closed
        // - Hard-linked `capture-spool/tmp` regular file (even containing the
        //   exact tombstone sentinel bytes!) and symlinked `capture-spool/tmp`
        //   both fail closed `LBR-IO-001` with zero disk mutation.
        // =====================================================================
        {
            let dir = tempdir().expect("create RB-10n link-safety dir");
            let storage = dir.path().join(".libra");
            create_full_rb10n_fixture_repo(&storage, true, true).await;

            // Same-process `MaintenanceLock::shared` contention must fail closed.
            {
                let shared_guard =
                    MaintenanceLock::shared(&storage).expect("acquire same-process shared lock");
                let err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("same-process shared MaintenanceLock must fail closed");
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                drop(shared_guard);
            }

            #[cfg(unix)]
            {
                let maint_path = storage.join("maintenance.lock");
                let _ = fs::remove_file(&maint_path);
                let decoy_target = dir.path().join("maint_decoy");
                fs::write(&decoy_target, b"decoy").expect("create decoy target");

                // Symlink `maintenance.lock`.
                std::os::unix::fs::symlink(&decoy_target, &maint_path)
                    .expect("plant symlink maintenance.lock");
                let sym_err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("symlink maintenance.lock must fail closed");
                assert_eq!(sym_err.stable_code_str(), "LBR-IO-001");
                fs::remove_file(&maint_path).expect("remove symlink maintenance.lock");

                // Hard-linked `maintenance.lock` (`nlink == 2`).
                fs::hard_link(&decoy_target, &maint_path)
                    .expect("plant hard-linked maintenance.lock");
                let hl_err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("hard-linked maintenance.lock must fail closed");
                assert_eq!(hl_err.stable_code_str(), "LBR-IO-001");
                fs::remove_file(&maint_path).expect("remove hard-linked maintenance.lock");

                // Hard-linked `capture-spool/tmp` tombstone sentinel (`nlink == 2`)
                // and symlinked `capture-spool/tmp` must both fail closed.
                let tmp_path = capture_spool_tmp_dir_path(&storage);
                fs::remove_dir(&tmp_path).expect("remove empty tmp dir for link test");
                let tomb_src = dir.path().join("tombstone_src");
                fs::write(&tomb_src, CAPTURE_SPOOL_TOMBSTONE_SENTINEL)
                    .expect("write tombstone sentinel source");

                fs::hard_link(&tomb_src, &tmp_path).expect("hard-link tombstone to tmp");
                let hl_tomb_err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("hard-linked capture-spool/tmp tombstone must fail closed");
                assert_eq!(hl_tomb_err.stable_code_str(), "LBR-IO-001");
                fs::remove_file(&tmp_path).expect("remove hard-linked tmp");

                std::os::unix::fs::symlink(&tomb_src, &tmp_path).expect("symlink tombstone to tmp");
                let sym_tomb_err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("symlinked capture-spool/tmp must fail closed");
                assert_eq!(sym_tomb_err.stable_code_str(), "LBR-IO-001");
                fs::remove_file(&tmp_path).expect("remove symlinked tmp");
            }
        }

        // =====================================================================
        // Additional lock contention regressions across ALL remaining non-SQLite
        // locks (`migration.lock`, `maintenance.lock` SHARED/EXCLUSIVE,
        // `capture-spool-consumer.lock`, `capture-queue/writer.lock`,
        // `capture-spool/capacity.lock`) + SQLite reader contention with absent
        // `writer.lock` -> ALL fail closed `LBR-IO-001` with 3-domain snapshots
        // 100% byte-for-byte unchanged.
        // =====================================================================
        {
            let dir = tempdir().expect("create RB-10n all-locks contention dir");
            let storage = dir.path().join(".libra");
            create_full_rb10n_fixture_repo(&storage, true, true).await;

            // Pre-create `migration.lock` and `capture-spool-consumer.lock` so
            // external OS children can lock the exact inodes.
            let mig_lock = migration_lock_path(&storage);
            let consumer_lock = capture_spool_consumer_lock_path(&storage);
            for p in [&mig_lock, &consumer_lock] {
                fs::write(p, b"").expect("seed lock file");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    fs::set_permissions(p, fs::Permissions::from_mode(0o600))
                        .expect("chmod 0600 lock file");
                }
            }

            let git_db = git_sqlite_db_path(&storage);
            let capture_db = capture_sqlite_db_path(&storage);
            let spool_dir = capture_spool_dir_path(&storage);
            let queue_dir = capture_queue_dir_path(&storage);

            let lock_cases: Vec<(&str, &str, PathBuf)> = vec![
                ("migration.lock", "hold_flock_exclusive", mig_lock.clone()),
                (
                    "maintenance.lock (SHARED)",
                    "hold_maintenance_shared",
                    storage.clone(),
                ),
                (
                    "maintenance.lock (EXCLUSIVE)",
                    "hold_maintenance_exclusive",
                    storage.clone(),
                ),
                (
                    "capture-spool-consumer.lock",
                    "hold_flock_exclusive",
                    consumer_lock.clone(),
                ),
                (
                    "capture-queue/writer.lock",
                    "hold_flock_exclusive",
                    capture_queue_writer_lock_path(&storage),
                ),
                (
                    "capture-spool/capacity.lock",
                    "hold_flock_exclusive",
                    capture_spool_capacity_lock_path(&storage),
                ),
                ("libra.db (DELETE reader)", "delete_reader", git_db.clone()),
            ];

            for (label, child_mode, target) in lock_cases {
                let holder = ChildLockHolder::spawn(child_mode, &target);
                let before_git = fs::read(&git_db).expect("snap git");
                let before_cap = fs::read(&capture_db).expect("snap cap");
                let before_spool = snapshot_directory_tree(&spool_dir);
                let before_queue = snapshot_directory_tree(&queue_dir);

                let err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("contended lock must fail closed");
                assert_eq!(err.stable_code_str(), "LBR-IO-001", "{label}");
                assert_eq!(fs::read(&git_db).expect("after git"), before_git, "{label}");
                assert_eq!(
                    fs::read(&capture_db).expect("after cap"),
                    before_cap,
                    "{label}"
                );
                assert_eq!(snapshot_directory_tree(&spool_dir), before_spool, "{label}");
                assert_eq!(snapshot_directory_tree(&queue_dir), before_queue, "{label}");
                holder.release_and_wait();
            }

            // Symlink inside `capture-spool/tmp/` must fail closed with zero mutation.
            #[cfg(unix)]
            {
                let sym_entry = capture_spool_tmp_dir_path(&storage).join("symlink-entry");
                std::os::unix::fs::symlink(&git_db, &sym_entry).expect("create symlink in tmp/");
                let before_spool = snapshot_directory_tree(&spool_dir);
                let sym_err = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect_err("symlink inside capture-spool/tmp must fail closed");
                assert_eq!(sym_err.stable_code_str(), "LBR-IO-001");
                assert_eq!(snapshot_directory_tree(&spool_dir), before_spool);
                fs::remove_file(&sym_entry).expect("remove symlink test entry");
            }
        }

        // =====================================================================
        // Positive control:
        // 1. Strictly expired `capture-spool/tmp/` file (> 24h) does NOT block
        //    quiescence and is NOT deleted during Phase 1.
        // 2. Uncontended quiescence median across 5 runs <= 50ms budget.
        // 3. `capacity.lock` and `append.lock` stay pinned in-place (identical
        //    inode before, during, and after quiescence — NEVER moved!).
        // 4. ALL 6 advisory locks + BOTH SQLite OS locks actively block
        //    independent spawned OS child processes while the guard is held.
        // =====================================================================
        {
            let dir = tempdir().expect("create RB-10n positive control dir");
            let storage = dir.path().join(".libra");
            create_full_rb10n_fixture_repo(&storage, true, true).await;

            let expired_tmp =
                capture_spool_tmp_dir_path(&storage).join("1-0000000000000000-0001~expired");
            let expired_bytes = b"{\"abandoned\":true}";
            fs::write(&expired_tmp, expired_bytes).expect("seed expired tmp file");
            let expired_file = OpenOptions::new()
                .write(true)
                .open(&expired_tmp)
                .expect("open expired tmp file");
            expired_file
                .set_modified(SystemTime::now() - Duration::from_secs(25 * 60 * 60))
                .expect("set mtime > 24h ago");
            drop(expired_file);

            let git_db = git_sqlite_db_path(&storage);
            let capture_db = capture_sqlite_db_path(&storage);
            let capacity_lock = capture_spool_capacity_lock_path(&storage);
            let append_lock = capture_queue_append_lock_path(&storage);
            #[cfg(unix)]
            let cap_ino_before = file_dev_ino(&capacity_lock);
            #[cfg(unix)]
            let app_ino_before = file_dev_ino(&append_lock);

            // 5-run median performance budget <= 50ms.
            let mut samples = Vec::with_capacity(5);
            for _ in 0..5 {
                let started = Instant::now();
                let guard = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect("uncontended Phase 1 quiescence must succeed");
                samples.push(started.elapsed());
                guard.close().await.expect("close timed Phase 1 guard");
            }
            samples.sort();
            let median = samples[samples.len() / 2];
            assert!(
                median <= Duration::from_millis(50),
                "RB-10n performance budget exceeded: median {median:?} > 50ms (samples={samples:?})"
            );

            // Hold guard and verify ALL 6 advisory locks + BOTH SQLite locks
            // from independent external OS child processes.
            let guard = quiesce_forward_repository_phase1(&storage)
                .await
                .expect("uncontended Phase 1 quiescence must succeed");

            assert_eq!(
                fs::read(&expired_tmp).expect("expired tmp file must remain untouched in Phase 1"),
                expired_bytes
            );
            #[cfg(unix)]
            {
                assert_eq!(
                    file_dev_ino(&capacity_lock),
                    cap_ino_before,
                    "capacity.lock must remain pinned in-place with identical inode"
                );
                assert_eq!(
                    file_dev_ino(&append_lock),
                    app_ino_before,
                    "append.lock must remain pinned in-place with identical inode"
                );
            }

            run_child_busy_probe("probe_flock_busy", &migration_lock_path(&storage));
            run_child_busy_probe("probe_maintenance_shared_busy", &storage);
            run_child_busy_probe(
                "probe_flock_busy",
                &capture_spool_consumer_lock_path(&storage),
            );
            run_child_busy_probe(
                "probe_flock_busy",
                &capture_queue_writer_lock_path(&storage),
            );
            run_child_busy_probe("probe_flock_busy", &capacity_lock);
            run_child_busy_probe("probe_flock_busy", &append_lock);
            run_child_busy_probe("probe_read_busy", &git_db);
            run_child_busy_probe("probe_write_busy", &git_db);
            run_child_busy_probe("probe_read_busy", &capture_db);
            run_child_busy_probe("probe_write_busy", &capture_db);

            guard.close().await.expect("close held Phase 1 guard");

            // After close, external lock acquisition succeeds again and in-place
            // lock inodes remain identical.
            #[cfg(unix)]
            {
                assert_eq!(file_dev_ino(&capacity_lock), cap_ino_before);
                assert_eq!(file_dev_ino(&append_lock), app_ino_before);
            }
            let post_holder = ChildLockHolder::spawn("hold_flock_exclusive", &append_lock);
            post_holder.release_and_wait();
        }

        // Pre-capture minimal SQLite-only repository (no `capture-spool/` or
        // `capture-queue/` initially): uncontended quiescence succeeds cleanly
        // and materializes private lock directories/files at 0700/0600.
        {
            let dir = tempdir().expect("create minimal SQLite-only repo dir");
            let storage = dir.path().join(".libra");
            create_delete_mode_fixture_db(&git_sqlite_db_path(&storage)).await;

            let guard = quiesce_forward_repository_phase1(&storage)
                .await
                .expect("minimal repo Phase 1 quiescence must succeed");
            run_child_busy_probe(
                "probe_flock_busy",
                &capture_spool_capacity_lock_path(&storage),
            );
            run_child_busy_probe(
                "probe_flock_busy",
                &capture_queue_append_lock_path(&storage),
            );
            guard
                .close()
                .await
                .expect("close minimal repo Phase 1 guard");
        }
    }

    // ========================================================================
    // RB-10a Tests: Offline WAL Checkpoint Flush & 3-Layout 61+25 Export
    // ========================================================================

    async fn switch_fixture_db_to_delete_mode(path: &Path) {
        let mut conn = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false),
        )
        .await
        .expect("connect fixture sqlite db for DELETE mode");
        let mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode = DELETE;")
            .fetch_one(&mut conn)
            .await
            .expect("switch fixture sqlite db to DELETE mode");
        assert_eq!(mode.to_ascii_lowercase(), "delete");
        conn.close().await.expect("close fixture sqlite db");
    }

    async fn populate_all_61_repository_tables(git_db: &Path) {
        use crate::storage::{
            SqliteGitStateStore, SqliteGitStore,
            config_store::{ConfigStore, MetadataStore},
            git_state_store::{GitStateStore, WorktreeIntentWrite},
            git_store::{
                GitStore, ObjectIndexRecord, ObliterationRequest, ReflogWrite, RevisionOrdinalMeta,
            },
            sqlite::SqliteConfigStore,
        };

        fs::create_dir_all(git_db.parent().expect("git_db parent")).expect("create .libra dir");
        let cfg = SqliteConfigStore::create(git_db, DatabaseRole::Repository)
            .await
            .expect("create repository sqlite db");
        // 1. schema_versions already populated by create_database_for_role
        // 2. config (legacy table)
        cfg.insert_legacy_entry(&LegacyConfigEntry {
            configuration: "user".to_string(),
            name: None,
            key: "legacy_name".to_string(),
            value: "Legacy Author".to_string(),
        })
        .await
        .expect("insert legacy config");
        // 3. config_kv (> 500 rows to exercise multi-batch streaming <= 500 rows/commit!)
        cfg.set("user.name", "Ada Lovelace", false)
            .await
            .expect("set user.name");
        cfg.set("vault.secret", "deadbeefcafe", true)
            .await
            .expect("set vault.secret");
        drop(cfg);

        // Bulk-insert 523 additional config_kv rows quickly in a single SQLite tx
        // (total 525 rows -> exercises 2 batches: 500 + 25).
        {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(git_db)
                    .create_if_missing(false),
            )
            .await
            .expect("open git_db for bulk config_kv");
            conn.execute("BEGIN IMMEDIATE").await.expect("begin tx");
            for i in 0..523usize {
                sea_orm::sqlx::query(
                    "INSERT INTO config_kv (key, value, encrypted) VALUES (?, ?, 0)",
                )
                .bind(format!("bulk.item.{i:04}"))
                .bind(format!("val-{i:04}"))
                .execute(&mut conn)
                .await
                .expect("insert bulk config_kv");
            }
            conn.execute("COMMIT").await.expect("commit tx");
            conn.close().await.expect("close bulk config_kv conn");
        }

        let cfg = SqliteConfigStore::open(git_db, DatabaseRole::Repository)
            .await
            .expect("reopen config store");
        // 4. metadata_kv
        cfg.set_metadata("branch", "main", "description", "primary branch", "text")
            .await
            .expect("set metadata_kv");
        drop(cfg);

        let git = SqliteGitStore::open(git_db)
            .await
            .expect("open SqliteGitStore");
        let oid_a = "1111111111111111111111111111111111111111";
        let oid_b = "2222222222222222222222222222222222222222";
        let oid_ob = "3333333333333333333333333333333333333333";
        let oid_blob = "4444444444444444444444444444444444444444";

        // 5. reference + 6. reflog
        git.update_ref_cas(
            &RefKey::LocalBranch {
                name: "main".to_string(),
            },
            None,
            Some(&RefTarget::Direct(oid_a.to_string())),
            &[ReflogWrite {
                ref_name: "refs/heads/main".to_string(),
                old_oid: "0".repeat(40),
                new_oid: oid_a.to_string(),
                timestamp: 1_795_400_001,
                committer_name: "Ada".to_string(),
                committer_email: "ada@example.com".to_string(),
                action: "commit".to_string(),
                message: "initial main commit".to_string(),
                worktree_id: None,
            }],
        )
        .await
        .expect("update main branch + reflog");
        git.update_ref_cas(
            &RefKey::LocalHead { worktree_id: None },
            None,
            Some(&RefTarget::Symbolic("main".to_string())),
            &[],
        )
        .await
        .expect("update main HEAD");
        git.update_ref_cas(
            &RefKey::LocalHead {
                worktree_id: Some("wt-linked-1".to_string()),
            },
            None,
            Some(&RefTarget::Direct(oid_a.to_string())),
            &[ReflogWrite {
                ref_name: "HEAD".to_string(),
                old_oid: "0".repeat(40),
                new_oid: oid_a.to_string(),
                timestamp: 1_795_400_002,
                committer_name: "Ada".to_string(),
                committer_email: "ada@example.com".to_string(),
                action: "checkout".to_string(),
                message: "checkout linked worktree".to_string(),
                worktree_id: Some("wt-linked-1".to_string()),
            }],
        )
        .await
        .expect("update linked HEAD + linked reflog");
        git.update_ref_cas(
            &RefKey::Tag {
                name: "v1.0".to_string(),
            },
            None,
            Some(&RefTarget::Direct(oid_a.to_string())),
            &[],
        )
        .await
        .expect("create tag");

        // 7. notes
        git.put_note("refs/notes/commits", oid_a, oid_blob, true)
            .await
            .expect("put note");

        // 8. object_index
        git.upsert_object_index_batch(&[ObjectIndexRecord {
            repo_id: "repo-1".to_string(),
            o_id: oid_a.to_string(),
            o_type: "commit".to_string(),
            o_size: 256,
            created_at: 1_795_400_003,
            is_synced: true,
        }])
        .await
        .expect("upsert object_index");

        // 9. object_obliteration
        git.begin_object_obliteration(&ObliterationRequest {
            oid: oid_ob.to_string(),
            hash_kind: ObjectHashKind::Sha1,
            reason: Some("gdpr".to_string()),
            actor: Some("admin".to_string()),
            approval_source: Some("ticket-1".to_string()),
            recorded_at: "2026-11-21T00:00:00Z".to_string(),
        })
        .await
        .expect("begin obliteration");

        // 10. revision_ordinal + 11. revision_ordinal_meta
        git.replace_revision_ordinals(
            &RevisionOrdinalMeta {
                ref_name: "refs/heads/main".to_string(),
                tip_oid: oid_b.to_string(),
                replace_sig: String::new(),
                max_ordinal: 2,
                built_at: "2026-11-21T00:00:01Z".to_string(),
            },
            &[(1, oid_a.to_string()), (2, oid_b.to_string())],
        )
        .await
        .expect("replace revision_ordinals");

        // 60. agent_bridge_publication
        git.prepare_bridge_publication("op-bridge-1", oid_a)
            .await
            .expect("prepare bridge publication");
        drop(git);

        let st = SqliteGitStateStore::open(git_db)
            .await
            .expect("open SqliteGitStateStore");

        // 12. sequence_state
        st.put_sequence_state(&SequenceStateRecord {
            worktree_id: String::new(),
            kind: "cherry-pick".to_string(),
            head_name: "refs/heads/main".to_string(),
            head_orig: oid_a.to_string(),
            current_oid: oid_b.to_string(),
            todo: "[]".to_string(),
            payload: "{}".to_string(),
            updated_at: "2026-11-21T00:00:02Z".to_string(),
        })
        .await
        .expect("put sequence_state");

        // 13. rebase_state
        st.put_rebase_state(&RebaseStateRecord {
            worktree_id: String::new(),
            head_name: "feature".to_string(),
            onto: oid_a.to_string(),
            orig_head: oid_b.to_string(),
            current_head: oid_a.to_string(),
            todo: "[]".to_string(),
            todo_actions: "[]".to_string(),
            done: "[]".to_string(),
            stopped_sha: None,
            autosquash: true,
            empty_mode: "keep".to_string(),
        })
        .await
        .expect("put rebase_state");

        // 14. bisect_state
        st.put_bisect_state(&BisectStateRecord {
            worktree_id: String::new(),
            orig_head: oid_b.to_string(),
            orig_head_name: Some("main".to_string()),
            bad: Some(oid_b.to_string()),
            good: format!("[\"{oid_a}\"]"),
            current: Some(oid_a.to_string()),
            skipped: None,
            steps: Some(1),
            completed: false,
            first_parent: true,
        })
        .await
        .expect("put bisect_state");

        // 15. worktree_registry_capability
        st.record_worktree_registry_capability(7)
            .await
            .expect("record worktree capability");

        // 16. worktree_lifecycle
        st.upsert_worktree_lifecycle(&WorktreeLifecycleRecord {
            worktree_id: "wt-1".to_string(),
            state: WorktreeLifecycleState::DetachedFromRegistry,
            path: "/work/wt-1".to_string(),
            reason: None,
            created_at: 1_795_400_010,
            updated_at: 1_795_400_011,
        })
        .await
        .expect("upsert worktree_lifecycle");

        // 17. worktree_intent_journal
        st.append_worktree_intent(&WorktreeIntentWrite {
            op: WorktreeIntentOp::Add,
            worktree_id: Some("wt-1".to_string()),
            payload: "{\"path\":\"/work/wt-1\"}".to_string(),
            stage: Some("staged".to_string()),
            created_at: 1_795_400_012,
        })
        .await
        .expect("append worktree_intent");

        // 18. workspace_record
        st.acquire_workspace_lease(&WorkspaceRecordRow {
            workspace_id: "ws-1".to_string(),
            repo_id: "repo-1".to_string(),
            kind: WorkspaceRecordKind::Linked,
            worktree_id: Some("wt-1".to_string()),
            path: "/work/ws-1".to_string(),
            owner_kind: WorkspaceRecordOwnerKind::Agent,
            owner_id: Some("agent-1".to_string()),
            task_id: Some("task-1".to_string()),
            session_id: Some("sess-1".to_string()),
            base_commit: Some(oid_a.to_string()),
            branch: Some("main".to_string()),
            state: WorkspaceRecordState::Active,
            lease_owner: Some("worker-1".to_string()),
            lease_fence: 1,
            lease_expires_at: Some(1_895_400_000),
            created_at: 1_795_400_013,
            updated_at: 1_795_400_014,
        })
        .await
        .expect("acquire workspace_record");

        // 19. operation + 20. operation_parent + 21. operation_head + 22. operation_journal
        let op_root = OperationRecord {
            op_id: "op-root-1".to_string(),
            repo_id: "repo-1".to_string(),
            format_version: 2,
            kind: "commit".to_string(),
            status: "committed".to_string(),
            command_name: Some("commit".to_string()),
            description: Some("root op".to_string()),
            args_digest: Some("sha256:aaa".to_string()),
            actor: Some("ada".to_string()),
            worktree_id: None,
            scope_kind: OperationScopeKind::Repository,
            scope_provenance: OperationScopeProvenance::Declared,
            pre_view_oid: oid_a.to_string(),
            post_view_oid: oid_b.to_string(),
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
            causal_context_id: None,
            restorable: true,
            control_slot: None,
            claim_owner: None,
            start_ts: 1_795_400_020,
            end_ts: Some(1_795_400_021),
        };
        st.append_operation_atomic(&op_root, &[], None)
            .await
            .expect("append root op");

        let op_child = OperationRecord {
            op_id: "op-child-2".to_string(),
            repo_id: "repo-1".to_string(),
            format_version: 2,
            kind: "commit".to_string(),
            status: "committed".to_string(),
            command_name: Some("commit".to_string()),
            description: Some("child op".to_string()),
            args_digest: None,
            actor: Some("ada".to_string()),
            worktree_id: Some("wt-1".to_string()),
            scope_kind: OperationScopeKind::Linked,
            scope_provenance: OperationScopeProvenance::Declared,
            pre_view_oid: oid_b.to_string(),
            post_view_oid: oid_a.to_string(),
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
            causal_context_id: None,
            restorable: true,
            control_slot: None,
            claim_owner: None,
            start_ts: 1_795_400_022,
            end_ts: Some(1_795_400_023),
        };
        let jrn = OperationJournalRecord {
            journal_id: "jrn-1".to_string(),
            op_id: "op-child-2".to_string(),
            phase: "committed".to_string(),
            pre_view_oid: Some(oid_b.to_string()),
            target_view_oid: Some(oid_a.to_string()),
            owner: "worker-1".to_string(),
            updated_at: 1_795_400_024,
            recovery_payload: Some("{}".to_string()),
        };
        st.append_operation_and_converge_heads(
            &op_child,
            &["op-root-1".to_string()],
            Some(&jrn),
            "repo",
            &[],
            &["op-child-2".to_string()],
        )
        .await
        .expect("append child op + journal + heads");

        // 23. change_identity + 24. change_revision + 25. change_predecessor
        st.upsert_change_identity(&ChangeIdentityRecord {
            change_id: "chg-1".to_string(),
            repo_id: "repo-1".to_string(),
            origin: "commit".to_string(),
            created_op_id: "op-root-1".to_string(),
            created_at: 1_795_400_030,
        })
        .await
        .expect("upsert change_identity");
        st.upsert_change_revision(&ChangeRevisionRecord {
            change_id: "chg-1".to_string(),
            commit_oid: oid_b.to_string(),
            created_op_id: "op-child-2".to_string(),
            visibility: "visible".to_string(),
            revision_ordinal: 1,
        })
        .await
        .expect("upsert change_revision");
        st.upsert_change_predecessor(&ChangePredecessorRecord {
            successor_oid: oid_b.to_string(),
            predecessor_oid: oid_a.to_string(),
            op_id: "op-child-2".to_string(),
            relation_kind: "amend".to_string(),
            ordinal: 0,
        })
        .await
        .expect("upsert change_predecessor");

        // 26. ai_operation_link
        st.upsert_ai_operation_link(&AiOperationLinkRecord {
            operation_id: "op-child-2".to_string(),
            session_id: Some("sess-1".to_string()),
            run_id: Some("run-1".to_string()),
            tool_invocation_id: Some("tool-1".to_string()),
            intent_id: Some("intent-1".to_string()),
            repo_id: "repo-1".to_string(),
            worktree_id: Some("wt-1".to_string()),
            workspace_id: Some("ws-1".to_string()),
            lease_generation: Some(1),
            config_provenance_digest: Some("sha256:cfg".to_string()),
            redaction_version: "v1".to_string(),
            change_id: Some("chg-1".to_string()),
        })
        .await
        .expect("upsert ai_operation_link");

        // 27. working_dirty + 28. working_dirty_meta
        st.replace_working_dirty_snapshot(
            &WorkingDirtyMetaRecord {
                worktree_id: "wt-1".to_string(),
                state: "fresh".to_string(),
                index_fingerprint: Some("fp-1".to_string()),
                head_oid: Some(oid_b.to_string()),
                scanned_at: Some("2026-11-21T00:00:03Z".to_string()),
                scan_lock_pid: None,
                scan_lock_at: None,
            },
            &[WorkingDirtyEntryRecord {
                worktree_id: "wt-1".to_string(),
                path: "src/main.rs".to_string(),
                kind: "modified".to_string(),
                source: "scanner".to_string(),
                marked_at: "2026-11-21T00:00:03Z".to_string(),
                verified_at: Some("2026-11-21T00:00:04Z".to_string()),
            }],
        )
        .await
        .expect("replace working_dirty");

        // 29. layer + 30. layer_path
        st.upsert_layer(&LayerRecord {
            worktree_id: "wt-1".to_string(),
            name: "overlay-1".to_string(),
            source: "local".to_string(),
            priority: 10,
            enabled: true,
            created_at: "2026-11-21T00:00:05Z".to_string(),
            updated_at: "2026-11-21T00:00:06Z".to_string(),
        })
        .await
        .expect("upsert layer");
        st.upsert_layer_path(&LayerPathRecord {
            worktree_id: "wt-1".to_string(),
            layer_name: "overlay-1".to_string(),
            path: "README.md".to_string(),
            content_hash: oid_blob.to_string(),
            materialized_at: "2026-11-21T00:00:07Z".to_string(),
        })
        .await
        .expect("upsert layer_path");

        // 31. sparse_view + 32. sparse_view_meta
        st.replace_sparse_view("wt-1", true, &["src/".to_string(), "docs/".to_string()])
            .await
            .expect("replace sparse_view");

        // 33..51: 19 `ai_*` tables
        st.upsert_ai_thread(&AiThreadRecord {
            thread_id: "thread-1".to_string(),
            title: Some("Migration thread".to_string()),
            owner_kind: "user".to_string(),
            owner_id: "u-1".to_string(),
            owner_display_name: Some("Ada".to_string()),
            current_intent_id: Some("intent-1".to_string()),
            latest_intent_id: Some("intent-1".to_string()),
            metadata_json: Some("{}".to_string()),
            archived: false,
            version: 1,
            created_at: 1_795_400_100,
            updated_at: 1_795_400_101,
        })
        .await
        .expect("upsert ai_thread");
        st.upsert_ai_thread_participant(&AiThreadParticipantRecord {
            thread_id: "thread-1".to_string(),
            actor_kind: "agent".to_string(),
            actor_id: "agent-1".to_string(),
            actor_display_name: Some("Antigravity".to_string()),
            role: "assistant".to_string(),
            joined_at: 1_795_400_102,
        })
        .await
        .expect("upsert ai_thread_participant");
        st.upsert_ai_thread_intent(&AiThreadIntentRecord {
            thread_id: "thread-1".to_string(),
            intent_id: "intent-1".to_string(),
            ordinal: 0,
            is_head: true,
            linked_at: 1_795_400_103,
            link_reason: "initial".to_string(),
        })
        .await
        .expect("upsert ai_thread_intent");
        st.upsert_ai_thread_provider_metadata(&AiThreadProviderMetadataRecord {
            thread_id: "thread-1".to_string(),
            legacy_session_id: Some("sess-1".to_string()),
            provider_thread_id: Some("prov-th-1".to_string()),
            provider_kind: Some("codex".to_string()),
            metadata_json: Some("{\"v\":1}".to_string()),
            updated_at: 1_795_400_104,
        })
        .await
        .expect("upsert ai_thread_provider_metadata");
        st.upsert_ai_scheduler_state(&AiSchedulerStateRecord {
            thread_id: "thread-1".to_string(),
            selected_plan_id: Some("plan-1".to_string()),
            active_task_id: Some("task-1".to_string()),
            active_run_id: Some("run-1".to_string()),
            metadata_json: None,
            version: 2,
            updated_at: 1_795_400_105,
        })
        .await
        .expect("upsert ai_scheduler_state");
        st.replace_ai_scheduler_plan_heads(
            "thread-1",
            &[AiSchedulerPlanHeadRecord {
                thread_id: "thread-1".to_string(),
                plan_id: "plan-1".to_string(),
                ordinal: 0,
            }],
        )
        .await
        .expect("replace ai_scheduler_plan_heads");
        st.replace_ai_scheduler_selected_plans(
            "thread-1",
            &[AiSchedulerSelectedPlanRecord {
                thread_id: "thread-1".to_string(),
                plan_id: "plan-1".to_string(),
                ordinal: 0,
            }],
        )
        .await
        .expect("replace ai_scheduler_selected_plans");
        st.replace_ai_live_context_window(
            "thread-1",
            &[AiLiveContextWindowRecord {
                thread_id: "thread-1".to_string(),
                context_frame_id: "frame-1".to_string(),
                position: 0,
                source_kind: "user".to_string(),
                pin_kind: Some("pinned".to_string()),
                inserted_at: 1_795_400_106,
            }],
        )
        .await
        .expect("replace ai_live_context_window");
        st.upsert_ai_index_intent_plan(&AiIndexIntentPlanRecord {
            intent_id: "intent-1".to_string(),
            plan_id: "plan-1".to_string(),
            created_at: 1_795_400_107,
        })
        .await
        .expect("upsert ai_index_intent_plan");
        st.upsert_ai_index_intent_task(&AiIndexIntentTaskRecord {
            intent_id: "intent-1".to_string(),
            task_id: "task-1".to_string(),
            parent_task_id: None,
            origin_step_id: Some("step-1".to_string()),
            created_at: 1_795_400_108,
        })
        .await
        .expect("upsert ai_index_intent_task");
        st.upsert_ai_index_plan_step_task(&AiIndexPlanStepTaskRecord {
            plan_id: "plan-1".to_string(),
            task_id: "task-1".to_string(),
            step_id: "step-1".to_string(),
            created_at: 1_795_400_109,
        })
        .await
        .expect("upsert ai_index_plan_step_task");
        st.upsert_ai_index_task_run(&AiIndexTaskRunRecord {
            task_id: "task-1".to_string(),
            run_id: "run-1".to_string(),
            is_latest: true,
            created_at: 1_795_400_110,
            base_commit_ref: Some(oid_a.to_string()),
        })
        .await
        .expect("upsert ai_index_task_run");
        st.upsert_ai_index_run_event(&AiIndexRunEventRecord {
            run_id: "run-1".to_string(),
            event_id: "ev-1".to_string(),
            event_kind: "tool".to_string(),
            is_latest: true,
            created_at: 1_795_400_111,
        })
        .await
        .expect("upsert ai_index_run_event");
        st.upsert_ai_index_run_patchset(&AiIndexRunPatchsetRecord {
            run_id: "run-1".to_string(),
            patchset_id: "ps-1".to_string(),
            sequence: 1,
            is_latest: true,
            created_at: 1_795_400_112,
        })
        .await
        .expect("upsert ai_index_run_patchset");
        st.upsert_ai_index_intent_context_frame(&AiIndexIntentContextFrameRecord {
            intent_id: "intent-1".to_string(),
            context_frame_id: "frame-1".to_string(),
            relation_kind: "input".to_string(),
            created_at: 1_795_400_113,
        })
        .await
        .expect("upsert ai_index_intent_context_frame");
        st.upsert_ai_validation_report(&AiValidationReportRecord {
            report_id: "vr-1".to_string(),
            thread_id: "thread-1".to_string(),
            run_id: Some("run-1".to_string()),
            policy_version: "p1".to_string(),
            stale: false,
            is_latest: true,
            summary_json: "{\"ok\":true}".to_string(),
            created_at: 1_795_400_114,
            updated_at: 1_795_400_115,
        })
        .await
        .expect("upsert ai_validation_report");
        st.upsert_ai_risk_score_breakdown(&AiRiskScoreBreakdownRecord {
            breakdown_id: "rs-1".to_string(),
            thread_id: "thread-1".to_string(),
            validation_report_id: Some("vr-1".to_string()),
            policy_version: "p1".to_string(),
            stale: false,
            is_latest: true,
            summary_json: "{\"score\":5}".to_string(),
            created_at: 1_795_400_116,
            updated_at: 1_795_400_117,
        })
        .await
        .expect("upsert ai_risk_score_breakdown");
        st.upsert_ai_decision_proposal(&AiDecisionProposalRecord {
            proposal_id: "dp-1".to_string(),
            thread_id: "thread-1".to_string(),
            validation_report_id: Some("vr-1".to_string()),
            risk_score_breakdown_id: Some("rs-1".to_string()),
            policy_version: "p1".to_string(),
            stale: false,
            is_latest: true,
            summary_json: "{\"proposal\":\"approve\"}".to_string(),
            created_at: 1_795_400_118,
            updated_at: 1_795_400_119,
        })
        .await
        .expect("upsert ai_decision_proposal");
        st.upsert_ai_final_decision(&AiFinalDecisionRecord {
            decision_id: "fd-1".to_string(),
            thread_id: "thread-1".to_string(),
            decision_proposal_id: Some("dp-1".to_string()),
            validation_report_id: Some("vr-1".to_string()),
            policy_version: "p1".to_string(),
            verdict: "approved".to_string(),
            stale: false,
            is_latest: true,
            summary_json: "{\"verdict\":\"approved\"}".to_string(),
            created_at: 1_795_400_120,
            updated_at: 1_795_400_121,
        })
        .await
        .expect("upsert ai_final_decision");

        // 52..54: governance & audit tables
        st.upsert_approved_permission(&ApprovedPermissionRecord {
            project_id: "proj-1".to_string(),
            permission: "fs.write".to_string(),
            pattern: "src/**".to_string(),
            created_at: 1_795_400_122,
            source_worktree_id: "wt-1".to_string(),
            source_session_id: "sess-1".to_string(),
            source_workspace_id: "ws-1".to_string(),
        })
        .await
        .expect("upsert approved_permission");
        st.append_automation_log(&AutomationLogRecord {
            id: "auto-1".to_string(),
            rule_id: "rule-1".to_string(),
            trigger_kind: "post_commit".to_string(),
            action_kind: "notify".to_string(),
            status: "ok".to_string(),
            message: "done".to_string(),
            started_at: "2026-11-21T00:01:00Z".to_string(),
            finished_at: "2026-11-21T00:01:01Z".to_string(),
            details_json: "{}".to_string(),
        })
        .await
        .expect("append automation_log");
        st.append_source_call_log(&SourceCallLogRecord {
            id: "call-1".to_string(),
            session_id: "sess-1".to_string(),
            source_slug: "github".to_string(),
            tool_name: "get_pr".to_string(),
            registered_tool_name: "github.get_pr".to_string(),
            tool_call_id: "tc-1".to_string(),
            credential_ref: Some("cred-1".to_string()),
            latency_ms: Some(42),
            input_bytes: 128,
            output_bytes: 512,
            cost_estimate_micros: Some(10),
            approval_decision: Some("approved".to_string()),
            state_namespace: "default".to_string(),
            success: true,
            created_at: "2026-11-21T00:01:02Z".to_string(),
            agent_run_id: Some("run-1".to_string()),
        })
        .await
        .expect("append source_call_log");

        // 55..59: 5 memory_* tables
        st.upsert_memory_episode(&MemoryEpisodeRecord {
            episode_id: "ep-1".to_string(),
            repo_id: "repo-1".to_string(),
            source_kind: MemorySourceKind::Commit,
            source_key: oid_a.to_string(),
            outcome: MemoryOutcome::Succeeded,
            actor: Some("ada".to_string()),
            started_at: 1_795_400_200,
            ended_at: 1_795_400_210,
            anchor_commit: Some(oid_a.to_string()),
            change_id: Some("chg-1".to_string()),
            title: "Initial Episode".to_string(),
            body: "Episode body".to_string(),
            content_digest: "sha256:ep1".to_string(),
            producer: "indexer".to_string(),
            rules_version: 1,
        })
        .await
        .expect("upsert memory_episode");
        st.replace_memory_episode_evidence(
            "ep-1",
            &[MemoryEpisodeEvidenceRecord {
                episode_id: "ep-1".to_string(),
                ordinal: 0,
                kind: MemoryEvidenceKind::Commit,
                ref_id: oid_a.to_string(),
                link_confidence: MemoryLinkConfidence::Identity,
                resolution_status: MemoryResolutionStatus::Resolved,
            }],
        )
        .await
        .expect("replace memory_episode_evidence");
        st.upsert_memory_projection_state(&MemoryProjectionStateRecord {
            repo_id: "repo-1".to_string(),
            source_kind: MemorySourceKind::Commit,
            cursor_json: "{\"tip\":1}".to_string(),
            fingerprint: "fp-mem-1".to_string(),
            rules_version: 1,
            schema_version: 1,
            horizon_truncated: false,
            revoked_count: 0,
            aged_out_count: 0,
            rebuilt_at: 1_795_400_220,
        })
        .await
        .expect("upsert memory_projection_state");
        st.upsert_memory_episode_path(&MemoryEpisodePathRecord {
            episode_id: "ep-1".to_string(),
            code_path: b"src/storage/migrate.rs".to_vec(),
            change_kind: MemoryPathChangeKind::Modified,
            blob_oid_at_end: Some(oid_blob.to_string()),
            ended_at: 1_795_400_210,
        })
        .await
        .expect("upsert memory_episode_path");
        st.upsert_memory_episode_search_doc(
            "ep-1",
            "Initial Episode",
            "Episode body",
            "src/storage/migrate.rs",
        )
        .await
        .expect("upsert memory_episode_search_doc");

        // 61. agent_bridge_index_publication
        st.prepare_bridge_index_publication("op-bridge-idx-1", "sha256:idx-digest")
            .await
            .expect("prepare bridge index publication");
        drop(st);
    }

    async fn populate_capture_rows_via_sql(
        db_path: &Path,
        extra_session_commits: usize,
        extra_session_id: Option<&str>,
    ) {
        let mut conn = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(db_path)
                .create_if_missing(false),
        )
        .await
        .expect("connect sqlite db for capture row population");
        conn.execute("PRAGMA foreign_keys = ON;")
            .await
            .expect("enable foreign keys");
        conn.execute("BEGIN IMMEDIATE").await.expect("begin tx");

        let oid_a = "1111111111111111111111111111111111111111";
        let oid_b = "2222222222222222222222222222222222222222";
        let oid_c = "4444444444444444444444444444444444444444";
        let digest = "a".repeat(64);
        let digest_b = "c".repeat(64);
        let ns = "b".repeat(32);

        // agent_session
        sea_orm::sqlx::query(
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, thread_id, state, working_dir, worktree_id, parent_commit, parent_session_id, metadata_json, redaction_report, started_at, last_event_at, stopped_at, schema_version, sync_revision, repo_id, workspace_id, workspace_fence, scope_state) VALUES ('sess-1', 'codex', 'prov-sess-1', 'thread-1', 'active', '/work', 'wt-1', ?, NULL, '{}', '{}', 1795400500, 1795400510, NULL, 1, 2, 'repo-1', 'ws-1', 1, 'scoped')",
        )
        .bind(oid_a)
        .execute(&mut conn)
        .await
        .expect("insert agent_session");

        if let Some(extra_sid) = extra_session_id {
            sea_orm::sqlx::query(
                "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, thread_id, state, working_dir, worktree_id, parent_commit, parent_session_id, metadata_json, redaction_report, started_at, last_event_at, stopped_at, schema_version, sync_revision, repo_id, workspace_id, workspace_fence, scope_state) VALUES (?, 'codex', ?, NULL, 'stopped', '/work', 'wt-1', ?, NULL, '{}', '{}', 1795400900, 1795400910, 1795400920, 1, 1, 'repo-1', 'ws-1', 1, 'scoped')",
            )
            .bind(extra_sid)
            .bind(format!("prov-{extra_sid}"))
            .bind(oid_b)
            .execute(&mut conn)
            .await
            .expect("insert extra post-copy agent_session");
        }

        // agent_checkpoint
        sea_orm::sqlx::query(
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit, tree_oid, metadata_blob_oid, traces_commit, tool_use_id, subagent_session_id, description, created_at, sync_revision) VALUES ('cp-1', 'sess-1', NULL, 'committed', ?, ?, ?, ?, NULL, NULL, 'turn 1', 1795400505, 1)",
        )
        .bind(oid_a)
        .bind(oid_b)
        .bind(oid_c)
        .bind(oid_b)
        .execute(&mut conn)
        .await
        .expect("insert agent_checkpoint");

        // agent_audit_log
        sea_orm::sqlx::query(
            "INSERT INTO agent_audit_log (audit_id, timestamp, user_id, user_name, action, checkpoint_id, scope, export_path, justification, granted) VALUES ('aud-1', '2026-11-21T00:05:00Z', 'u1', 'Ada', 'export', 'cp-1', 'committed', NULL, 'audit', 1)",
        )
        .execute(&mut conn)
        .await
        .expect("insert agent_audit_log");

        // agent_bridge_session
        sea_orm::sqlx::query(
            "INSERT INTO agent_bridge_session (bridge_session_id, source, repository_id, worktree_id, workspace_id, parent_session_id, agent_id, session_state, schema_version, last_event_seq, last_acked_seq, created_at, updated_at) VALUES ('bs-1', 'deepseek-harness', 'repo-1', 'wt-1', 'ws-1', NULL, 'agent-1', 'open', 1, 1, 1, 1795400600, 1795400601)",
        )
        .execute(&mut conn)
        .await
        .expect("insert agent_bridge_session");

        // agent_bridge_checkpoint
        sea_orm::sqlx::query(
            "INSERT INTO agent_bridge_checkpoint (checkpoint_id, bridge_session_id, agent_checkpoint_id, target_oid, created_at) VALUES ('bcp-1', 'bs-1', 'cp-1', ?, 1795400602)",
        )
        .bind(oid_a)
        .execute(&mut conn)
        .await
        .expect("insert agent_bridge_checkpoint");

        // agent_bridge_event
        sea_orm::sqlx::query(
            "INSERT INTO agent_bridge_event (event_id, bridge_session_id, event_seq, event_type, payload_sha256, payload, operation_id, created_at) VALUES (1, 'bs-1', 1, 'message', ?, '{\"msg\":\"hi\"}', 'op-bridge-1', 1795400603)",
        )
        .bind(&digest)
        .execute(&mut conn)
        .await
        .expect("insert agent_bridge_event");

        // agent_bridge_link
        sea_orm::sqlx::query(
            "INSERT INTO agent_bridge_link (link_id, bridge_session_id, source_type, source_id, target_type, target_id, created_at) VALUES (1, 'bs-1', 'operation', 'op-bridge-1', 'commit', ?, 1795400604)",
        )
        .bind(oid_a)
        .execute(&mut conn)
        .await
        .expect("insert agent_bridge_link");

        // agent_bridge_operation
        sea_orm::sqlx::query(
            "INSERT INTO agent_bridge_operation (operation_id, bridge_session_id, method, params_digest, repository_id, workspace_id, status, result_digest, created_at, completed_at) VALUES ('op-bridge-1', 'bs-1', 'commit', ?, 'repo-1', 'ws-1', 'applied', ?, 1795400605, 1795400606)",
        )
        .bind(&digest)
        .bind(&digest)
        .execute(&mut conn)
        .await
        .expect("insert agent_bridge_operation");

        // agent_capture_cloud_base
        sea_orm::sqlx::query(
            "INSERT INTO agent_capture_cloud_base (repo_id, remote_generation, updated_at) VALUES ('repo-1', 3, 1795400607)",
        )
        .execute(&mut conn)
        .await
        .expect("insert agent_capture_cloud_base");

        // agent_capture_incarnation
        sea_orm::sqlx::query(
            "INSERT INTO agent_capture_incarnation (agent_kind, provider_session_id, next_session_sync_revision, source_namespace, updated_at) VALUES ('codex', 'prov-sess-1', 4, ?, 1795400608)",
        )
        .bind(&ns)
        .execute(&mut conn)
        .await
        .expect("insert agent_capture_incarnation");

        // agent_checkpoint_prune_tombstone
        sea_orm::sqlx::query(
            "INSERT INTO agent_checkpoint_prune_tombstone (checkpoint_id, session_id, pruned_at) VALUES ('cp-pruned-1', 'sess-1', 1795400609)",
        )
        .execute(&mut conn)
        .await
        .expect("insert agent_checkpoint_prune_tombstone");

        // agent_coverage_claim
        sea_orm::sqlx::query(
            "INSERT INTO agent_coverage_claim (session_id, logical_turn_key, coverage_schema_version, coverage_digest, completeness, revision, state, attempt_checkpoint_id, owner, lease_expires_at, fence_token, checkpoint_id, traces_commit, source_channel, created_at, updated_at) VALUES ('sess-1', 'turn-1', 1, ?, 'complete', 1, 'catalog_committed', NULL, NULL, NULL, 1, 'cp-1', ?, 'live', 1795400610, 1795400611)",
        )
        .bind(&digest)
        .bind(oid_b)
        .execute(&mut conn)
        .await
        .expect("insert agent_coverage_claim");

        // agent_coverage_conflict
        sea_orm::sqlx::query(
            "INSERT INTO agent_coverage_conflict (session_id, logical_turn_key, coverage_schema_version, incumbent_revision, incumbent_digest, incumbent_checkpoint_id, incoming_digest, incoming_source_channel, incoming_observed_at, incoming_canonical_json, incoming_redaction_report_json) VALUES ('sess-1', 'turn-1', 1, 1, ?, 'cp-1', ?, 'import', 1795400612, '{}', '{}')",
        )
        .bind(&digest)
        .bind(&digest_b)
        .execute(&mut conn)
        .await
        .expect("insert agent_coverage_conflict");

        // agent_coverage_revision
        sea_orm::sqlx::query(
            "INSERT INTO agent_coverage_revision (session_id, logical_turn_key, coverage_schema_version, revision, checkpoint_id, coverage_digest, completeness, source_channel, created_at) VALUES ('sess-1', 'turn-1', 1, 1, 'cp-1', ?, 'complete', 'live', 1795400613)",
        )
        .bind(&digest)
        .execute(&mut conn)
        .await
        .expect("insert agent_coverage_revision");

        // agent_export_job
        sea_orm::sqlx::query(
            "INSERT INTO agent_export_job (job_id, agent_kind, provider_session_id, owner, lease_expires_at, fence_token, observed_generation, processed_generation, state, last_error_code, created_at, updated_at, ttl_expires_at, repo_id, worktree_id, workspace_id, workspace_fence, scope_state) VALUES ('job-1', 'codex', 'prov-sess-1', 'worker-1', 1895400000, 1, 2, 1, 'idle', NULL, 1795400614, 1795400615, 1895400999, 'repo-1', 'wt-1', 'ws-1', 1, 'scoped')",
        )
        .execute(&mut conn)
        .await
        .expect("insert agent_export_job");

        // agent_import_identity
        sea_orm::sqlx::query(
            "INSERT INTO agent_import_identity (identity_id, agent_kind, provider_session_id, source_kind, source_id, schema_version, observed_digest, committed_digest, attempt_id, attempt_checkpoint_id, next_ordinal, state, owner, lease_expires_at, fence_token, last_error_code, created_at, updated_at, repo_id, worktree_id, workspace_id, workspace_fence, scope_state) VALUES ('imp-1', 'codex', 'prov-sess-1', 'jsonl', 'src-1', 1, ?, ?, NULL, NULL, 2, 'committed', NULL, NULL, NULL, NULL, 1795400616, 1795400617, 'repo-1', 'wt-1', 'ws-1', 1, 'scoped')",
        )
        .bind(&digest)
        .bind(&digest)
        .execute(&mut conn)
        .await
        .expect("insert agent_import_identity");

        // agent_import_tombstone (for a distinct erased session so it doesn't conflict with active sess-1)
        sea_orm::sqlx::query(
            "INSERT INTO agent_import_tombstone (tombstone_id, agent_kind, provider_session_id, erased_session_id, source_fingerprint, erased_at) VALUES ('tomb-1', 'codex', 'prov-erased-1', 'sess-erased-1', 'fp-1', 1795400618)",
        )
        .execute(&mut conn)
        .await
        .expect("insert agent_import_tombstone");

        // agent_session_commit (1 base row + optional >500 rows for multi-page Layout B key probe!)
        sea_orm::sqlx::query(
            "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) VALUES ('sess-1', 'sha1', ?)",
        )
        .bind(oid_a)
        .execute(&mut conn)
        .await
        .expect("insert base agent_session_commit");
        for i in 0..extra_session_commits {
            let commit_hex = format!("{i:040x}");
            sea_orm::sqlx::query(
                "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) VALUES ('sess-1', 'sha1', ?)",
            )
            .bind(commit_hex)
            .execute(&mut conn)
            .await
            .expect("insert extra agent_session_commit");
        }

        // agent_subagent_content_claim
        sea_orm::sqlx::query(
            "INSERT INTO agent_subagent_content_claim (parent_session_id, provider_kind, source_key, content_schema_version, current_revision, current_checkpoint_id, current_digest, state, attempt_digest, attempt_checkpoint_id, owner, lease_expires_at, fence_token, created_at, updated_at, revision_cursor, sync_revision) VALUES ('sess-1', 'codex', 'sub-key-1', 1, 1, 'cp-1', ?, 'idle', NULL, NULL, NULL, NULL, 1, 1795400619, 1795400620, 1, 1)",
        )
        .bind(&digest)
        .execute(&mut conn)
        .await
        .expect("insert agent_subagent_content_claim");

        // agent_subagent_content_revision
        sea_orm::sqlx::query(
            "INSERT INTO agent_subagent_content_revision (parent_session_id, provider_kind, source_key, content_schema_version, revision, checkpoint_id, content_digest, source_channel, partial, created_at) VALUES ('sess-1', 'codex', 'sub-key-1', 1, 1, 'cp-1', ?, 'live', 0, 1795400621)",
        )
        .bind(&digest)
        .execute(&mut conn)
        .await
        .expect("insert agent_subagent_content_revision");

        // agent_subagent_link
        sea_orm::sqlx::query(
            "INSERT INTO agent_subagent_link (content_checkpoint_id, parent_session_id, link_state, boundary_checkpoint_id, stable_subagent_id, created_at, updated_at, sync_revision) VALUES ('cp-1', 'sess-1', 'resolved', 'cp-1', 'sub-stable-1', 1795400622, 1795400623, 1)",
        )
        .execute(&mut conn)
        .await
        .expect("insert agent_subagent_link");

        // agent_usage_stats
        sea_orm::sqlx::query(
            "INSERT INTO agent_usage_stats (id, session_id, thread_id, agent_run_id, run_id, provider, model, request_kind, intent, prompt_tokens, completion_tokens, cached_tokens, reasoning_tokens, total_tokens, tool_call_count, wall_clock_ms, provider_latency_ms, cost_estimate_micro_dollars, cost_usd, usage_estimated, started_at, finished_at, success, error_kind, schema_version, created_at, agent_name, repo_id, turn_id, event_id) VALUES ('usage-1', 'sess-1', 'thread-1', 'run-1', 'run-1', 'openai', 'gpt-5', 'chat', 'code', 100, 50, 20, 10, 150, 2, 1200, 900, 3500, 0.0035, 0, '2026-11-21T00:05:00Z', '2026-11-21T00:05:01Z', 1, NULL, 1, '2026-11-21T00:05:01Z', 'codex', 'repo-1', 'turn-1', 'ev-usage-1')",
        )
        .execute(&mut conn)
        .await
        .expect("insert agent_usage_stats");

        // agent_workspace_scope_audit
        sea_orm::sqlx::query(
            "INSERT INTO agent_workspace_scope_audit (audit_id, action, agent_kind, provider_session_id, repo_id, worktree_id, workspace_id, workspace_fence, actor, created_at) VALUES ('wsa-1', 'adopt_legacy_capture_scope', 'codex', 'prov-sess-1', 'repo-1', 'wt-1', 'ws-1', 1, 'ada', 1795400624)",
        )
        .execute(&mut conn)
        .await
        .expect("insert agent_workspace_scope_audit");

        conn.execute("COMMIT").await.expect("commit capture rows");
        conn.close().await.expect("close capture population conn");
    }

    async fn assert_all_61_repo_tables_in_redb(git_db: &Path, git_tmp_path: &Path) {
        use crate::storage::{
            RedbGitStateStore, RedbGitStore, SqliteGitStateStore, SqliteGitStore,
            config_store::{ConfigStore, MetadataStore},
            git_state_store::GitStateStore,
            git_store::GitStore,
            redb::RedbConfigStore,
            sqlite::SqliteConfigStore,
        };

        let oid_a = "1111111111111111111111111111111111111111";
        let oid_b = "2222222222222222222222222222222222222222";
        let oid_ob = "3333333333333333333333333333333333333333";

        // 1..4 ConfigStore + MetadataStore — full record-for-record SQLite vs Redb comparison
        let sq_cfg = SqliteConfigStore::open_read_only(git_db, DatabaseRole::Repository)
            .await
            .expect("open source git sqlite via SqliteConfigStore::open_read_only");
        let cfg = RedbConfigStore::open_read_only(git_tmp_path, DatabaseRole::Repository)
            .expect("open exported git redb via RedbConfigStore::open_read_only");
        assert_eq!(
            cfg.list_schema_receipts()
                .await
                .expect("redb schema receipts"),
            sq_cfg
                .list_schema_receipts()
                .await
                .expect("sqlite schema receipts"),
            "schema_versions must match SQLite source field-for-field"
        );
        assert_eq!(
            cfg.list_legacy_entries().await.expect("redb legacy config"),
            sq_cfg
                .list_legacy_entries()
                .await
                .expect("sqlite legacy config"),
            "config (legacy) must match SQLite source field-for-field"
        );
        assert_eq!(
            cfg.get("user.name").await.expect("redb user.name"),
            sq_cfg.get("user.name").await.expect("sqlite user.name"),
        );
        assert_eq!(
            cfg.get("vault.secret").await.expect("redb vault.secret"),
            sq_cfg
                .get("vault.secret")
                .await
                .expect("sqlite vault.secret"),
        );
        assert_eq!(
            cfg.get("bulk.item.0000").await.expect("redb bulk 0000"),
            sq_cfg
                .get("bulk.item.0000")
                .await
                .expect("sqlite bulk 0000"),
        );
        assert_eq!(
            cfg.get("bulk.item.0522").await.expect("redb bulk 0522"),
            sq_cfg
                .get("bulk.item.0522")
                .await
                .expect("sqlite bulk 0522"),
        );
        let mut redb_all_kv = cfg.list_all().await.expect("redb list_all config_kv");
        let mut sq_all_kv = sq_cfg.list_all().await.expect("sqlite list_all config_kv");
        redb_all_kv.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.value.cmp(&b.value)));
        sq_all_kv.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.value.cmp(&b.value)));
        assert_eq!(redb_all_kv.len(), 525);
        assert_eq!(
            redb_all_kv, sq_all_kv,
            "all 525 config_kv rows must match SQLite source field-for-field"
        );
        assert_eq!(
            cfg.get_metadata("branch", "main", "description")
                .await
                .expect("redb metadata_kv"),
            sq_cfg
                .get_metadata("branch", "main", "description")
                .await
                .expect("sqlite metadata_kv"),
            "metadata_kv must match SQLite source field-for-field"
        );
        drop(cfg);
        drop(sq_cfg);

        // 5..11 + 60 GitStore — full record-for-record SQLite vs Redb comparison + all secondary indices
        let sq_git = SqliteGitStore::open_read_only(git_db)
            .await
            .expect("open source git sqlite via SqliteGitStore::open_read_only");
        let git = RedbGitStore::open_read_only(git_tmp_path)
            .expect("open exported git redb via RedbGitStore::open_read_only");
        assert_eq!(
            git.list_refs(None).await.expect("redb list_refs(None)"),
            sq_git
                .list_refs(None)
                .await
                .expect("sqlite list_refs(None)"),
            "reference (main scope) must match SQLite source field-for-field"
        );
        assert_eq!(
            git.list_refs(Some("wt-linked-1"))
                .await
                .expect("redb list_refs(wt-linked-1)"),
            sq_git
                .list_refs(Some("wt-linked-1"))
                .await
                .expect("sqlite list_refs(wt-linked-1)"),
            "reference (linked worktree) must match SQLite source field-for-field"
        );
        assert_eq!(
            git.list_all_local_heads()
                .await
                .expect("redb list_all_local_heads"),
            sq_git
                .list_all_local_heads()
                .await
                .expect("sqlite list_all_local_heads"),
        );
        assert_eq!(
            git.list_reflog("refs/heads/main", None, None)
                .await
                .expect("redb main reflog"),
            sq_git
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("sqlite main reflog"),
            "reflog + reflog_by_ref (main) must match SQLite source field-for-field"
        );
        assert_eq!(
            git.list_reflog("HEAD", Some("wt-linked-1"), None)
                .await
                .expect("redb linked HEAD reflog"),
            sq_git
                .list_reflog("HEAD", Some("wt-linked-1"), None)
                .await
                .expect("sqlite linked HEAD reflog"),
            "reflog + reflog_by_ref (linked) must match SQLite source field-for-field"
        );
        assert_eq!(
            git.list_notes("refs/notes/commits")
                .await
                .expect("redb list_notes"),
            sq_git
                .list_notes("refs/notes/commits")
                .await
                .expect("sqlite list_notes"),
            "notes must match SQLite source field-for-field"
        );
        assert_eq!(
            git.get_object_index("repo-1", oid_a)
                .await
                .expect("redb object_index"),
            sq_git
                .get_object_index("repo-1", oid_a)
                .await
                .expect("sqlite object_index"),
            "object_index must match SQLite source field-for-field"
        );
        assert_eq!(
            git.get_object_obliteration(oid_ob)
                .await
                .expect("redb object_obliteration"),
            sq_git
                .get_object_obliteration(oid_ob)
                .await
                .expect("sqlite object_obliteration"),
            "object_obliteration must match SQLite source field-for-field"
        );
        assert_eq!(
            git.get_revision_ordinal_meta("refs/heads/main")
                .await
                .expect("redb rev ordinal meta"),
            sq_git
                .get_revision_ordinal_meta("refs/heads/main")
                .await
                .expect("sqlite rev ordinal meta"),
            "revision_ordinal_meta must match SQLite source field-for-field"
        );
        assert_eq!(
            git.find_revision_oid_by_ordinal("refs/heads/main", 1)
                .await
                .expect("redb ordinal 1"),
            sq_git
                .find_revision_oid_by_ordinal("refs/heads/main", 1)
                .await
                .expect("sqlite ordinal 1"),
        );
        assert_eq!(
            git.find_revision_oid_by_ordinal("refs/heads/main", 2)
                .await
                .expect("redb ordinal 2"),
            sq_git
                .find_revision_oid_by_ordinal("refs/heads/main", 2)
                .await
                .expect("sqlite ordinal 2"),
        );
        assert_eq!(
            git.find_revision_ordinal_by_oid("refs/heads/main", oid_a)
                .await
                .expect("redb rev by oid_a"),
            Some(1)
        );
        assert_eq!(
            git.find_revision_ordinal_by_oid("refs/heads/main", oid_b)
                .await
                .expect("redb rev by oid_b"),
            Some(2)
        );
        assert_eq!(
            git.list_bridge_publications()
                .await
                .expect("redb bridge_publication"),
            sq_git
                .list_bridge_publications()
                .await
                .expect("sqlite bridge_publication"),
            "agent_bridge_publication must match SQLite source field-for-field"
        );
        drop(git);
        drop(sq_git);

        // 12..59 + 61 GitStateStore — full record-for-record SQLite vs Redb comparison + secondary indices
        let sq_st = SqliteGitStateStore::open_read_only(git_db)
            .await
            .expect("open source git sqlite via SqliteGitStateStore::open_read_only");
        let st = RedbGitStateStore::open_read_only(git_tmp_path)
            .expect("open exported git redb via RedbGitStateStore::open_read_only");
        assert_eq!(
            st.get_sequence_state("")
                .await
                .expect("redb sequence_state"),
            sq_st
                .get_sequence_state("")
                .await
                .expect("sqlite sequence_state"),
        );
        assert_eq!(
            st.get_rebase_state("").await.expect("redb rebase_state"),
            sq_st
                .get_rebase_state("")
                .await
                .expect("sqlite rebase_state"),
        );
        assert_eq!(
            st.get_bisect_state("").await.expect("redb bisect_state"),
            sq_st
                .get_bisect_state("")
                .await
                .expect("sqlite bisect_state"),
        );
        assert_eq!(
            st.list_worktree_registry_capabilities()
                .await
                .expect("redb worktree_registry_capability"),
            sq_st
                .list_worktree_registry_capabilities()
                .await
                .expect("sqlite worktree_registry_capability"),
        );
        assert_eq!(
            st.get_worktree_lifecycle("wt-1")
                .await
                .expect("redb worktree_lifecycle"),
            sq_st
                .get_worktree_lifecycle("wt-1")
                .await
                .expect("sqlite worktree_lifecycle"),
        );
        assert_eq!(
            st.list_worktree_intents()
                .await
                .expect("redb worktree_intent_journal"),
            sq_st
                .list_worktree_intents()
                .await
                .expect("sqlite worktree_intent_journal"),
        );
        assert_eq!(
            st.get_workspace_record("ws-1")
                .await
                .expect("redb workspace_record"),
            sq_st
                .get_workspace_record("ws-1")
                .await
                .expect("sqlite workspace_record"),
        );
        assert_eq!(
            st.get_operation("repo-1", "op-root-1")
                .await
                .expect("redb op-root-1"),
            sq_st
                .get_operation("repo-1", "op-root-1")
                .await
                .expect("sqlite op-root-1"),
        );
        assert_eq!(
            st.get_operation("repo-1", "op-child-2")
                .await
                .expect("redb op-child-2"),
            sq_st
                .get_operation("repo-1", "op-child-2")
                .await
                .expect("sqlite op-child-2"),
        );
        assert_eq!(
            st.get_operation_heads("repo-1", "repo")
                .await
                .expect("redb operation_head"),
            sq_st
                .get_operation_heads("repo-1", "repo")
                .await
                .expect("sqlite operation_head"),
        );
        assert_eq!(
            st.list_operation_journals("op-child-2")
                .await
                .expect("redb operation_journal"),
            sq_st
                .list_operation_journals("op-child-2")
                .await
                .expect("sqlite operation_journal"),
        );
        assert_eq!(
            st.get_change_identity("chg-1")
                .await
                .expect("redb change_identity"),
            sq_st
                .get_change_identity("chg-1")
                .await
                .expect("sqlite change_identity"),
        );
        assert_eq!(
            st.find_change_revisions_by_commit(oid_b)
                .await
                .expect("redb change_revision_by_commit"),
            sq_st
                .find_change_revisions_by_commit(oid_b)
                .await
                .expect("sqlite change_revision_by_commit"),
        );
        assert_eq!(
            st.list_change_predecessors(oid_b)
                .await
                .expect("redb change_predecessor"),
            sq_st
                .list_change_predecessors(oid_b)
                .await
                .expect("sqlite change_predecessor"),
        );
        assert_eq!(
            st.get_ai_operation_link("op-child-2")
                .await
                .expect("redb ai_operation_link"),
            sq_st
                .get_ai_operation_link("op-child-2")
                .await
                .expect("sqlite ai_operation_link"),
        );
        assert_eq!(
            st.get_working_dirty_meta("wt-1")
                .await
                .expect("redb working_dirty_meta"),
            sq_st
                .get_working_dirty_meta("wt-1")
                .await
                .expect("sqlite working_dirty_meta"),
        );
        assert_eq!(
            st.list_working_dirty_entries("wt-1")
                .await
                .expect("redb working_dirty"),
            sq_st
                .list_working_dirty_entries("wt-1")
                .await
                .expect("sqlite working_dirty"),
        );
        assert_eq!(
            st.list_layers("wt-1").await.expect("redb layer"),
            sq_st.list_layers("wt-1").await.expect("sqlite layer"),
        );
        assert_eq!(
            st.list_layer_paths("wt-1", Some("overlay-1"))
                .await
                .expect("redb layer_path"),
            sq_st
                .list_layer_paths("wt-1", Some("overlay-1"))
                .await
                .expect("sqlite layer_path"),
        );
        assert_eq!(
            st.get_sparse_view("wt-1").await.expect("redb sparse_view"),
            sq_st
                .get_sparse_view("wt-1")
                .await
                .expect("sqlite sparse_view"),
        );
        assert_eq!(
            st.get_ai_thread("thread-1").await.expect("redb ai_thread"),
            sq_st
                .get_ai_thread("thread-1")
                .await
                .expect("sqlite ai_thread"),
        );
        assert_eq!(
            st.list_ai_thread_participants("thread-1")
                .await
                .expect("redb ai_thread_participant"),
            sq_st
                .list_ai_thread_participants("thread-1")
                .await
                .expect("sqlite ai_thread_participant"),
        );
        assert_eq!(
            st.list_ai_thread_intents("thread-1")
                .await
                .expect("redb ai_thread_intent"),
            sq_st
                .list_ai_thread_intents("thread-1")
                .await
                .expect("sqlite ai_thread_intent"),
        );
        assert_eq!(
            st.get_ai_thread_provider_metadata("thread-1")
                .await
                .expect("redb ai_thread_provider_metadata"),
            sq_st
                .get_ai_thread_provider_metadata("thread-1")
                .await
                .expect("sqlite ai_thread_provider_metadata"),
        );
        assert_eq!(
            st.get_ai_scheduler_state("thread-1")
                .await
                .expect("redb ai_scheduler_state"),
            sq_st
                .get_ai_scheduler_state("thread-1")
                .await
                .expect("sqlite ai_scheduler_state"),
        );
        assert_eq!(
            st.list_ai_scheduler_plan_heads("thread-1")
                .await
                .expect("redb ai_scheduler_plan_head"),
            sq_st
                .list_ai_scheduler_plan_heads("thread-1")
                .await
                .expect("sqlite ai_scheduler_plan_head"),
        );
        assert_eq!(
            st.list_ai_scheduler_selected_plans("thread-1")
                .await
                .expect("redb ai_scheduler_selected_plan"),
            sq_st
                .list_ai_scheduler_selected_plans("thread-1")
                .await
                .expect("sqlite ai_scheduler_selected_plan"),
        );
        assert_eq!(
            st.list_ai_live_context_window("thread-1")
                .await
                .expect("redb ai_live_context_window"),
            sq_st
                .list_ai_live_context_window("thread-1")
                .await
                .expect("sqlite ai_live_context_window"),
        );
        assert_eq!(
            st.list_ai_index_intent_plans("intent-1")
                .await
                .expect("redb ai_index_intent_plan"),
            sq_st
                .list_ai_index_intent_plans("intent-1")
                .await
                .expect("sqlite ai_index_intent_plan"),
        );
        assert_eq!(
            st.list_ai_index_intent_tasks("intent-1")
                .await
                .expect("redb ai_index_intent_task"),
            sq_st
                .list_ai_index_intent_tasks("intent-1")
                .await
                .expect("sqlite ai_index_intent_task"),
        );
        assert_eq!(
            st.list_ai_index_plan_step_tasks("plan-1")
                .await
                .expect("redb ai_index_plan_step_task"),
            sq_st
                .list_ai_index_plan_step_tasks("plan-1")
                .await
                .expect("sqlite ai_index_plan_step_task"),
        );
        assert_eq!(
            st.list_ai_index_task_runs("task-1")
                .await
                .expect("redb ai_index_task_run"),
            sq_st
                .list_ai_index_task_runs("task-1")
                .await
                .expect("sqlite ai_index_task_run"),
        );
        assert_eq!(
            st.list_ai_index_run_events("run-1")
                .await
                .expect("redb ai_index_run_event"),
            sq_st
                .list_ai_index_run_events("run-1")
                .await
                .expect("sqlite ai_index_run_event"),
        );
        assert_eq!(
            st.list_ai_index_run_patchsets("run-1")
                .await
                .expect("redb ai_index_run_patchset"),
            sq_st
                .list_ai_index_run_patchsets("run-1")
                .await
                .expect("sqlite ai_index_run_patchset"),
        );
        assert_eq!(
            st.list_ai_index_intent_context_frames("intent-1")
                .await
                .expect("redb ai_index_intent_context_frame"),
            sq_st
                .list_ai_index_intent_context_frames("intent-1")
                .await
                .expect("sqlite ai_index_intent_context_frame"),
        );
        assert_eq!(
            st.list_ai_validation_reports("thread-1")
                .await
                .expect("redb ai_validation_report"),
            sq_st
                .list_ai_validation_reports("thread-1")
                .await
                .expect("sqlite ai_validation_report"),
        );
        assert_eq!(
            st.list_ai_risk_score_breakdowns("thread-1")
                .await
                .expect("redb ai_risk_score_breakdown"),
            sq_st
                .list_ai_risk_score_breakdowns("thread-1")
                .await
                .expect("sqlite ai_risk_score_breakdown"),
        );
        assert_eq!(
            st.list_ai_decision_proposals("thread-1")
                .await
                .expect("redb ai_decision_proposal"),
            sq_st
                .list_ai_decision_proposals("thread-1")
                .await
                .expect("sqlite ai_decision_proposal"),
        );
        assert_eq!(
            st.list_ai_final_decisions("thread-1")
                .await
                .expect("redb ai_final_decision"),
            sq_st
                .list_ai_final_decisions("thread-1")
                .await
                .expect("sqlite ai_final_decision"),
        );
        assert_eq!(
            st.list_approved_permissions("proj-1")
                .await
                .expect("redb approved_permission"),
            sq_st
                .list_approved_permissions("proj-1")
                .await
                .expect("sqlite approved_permission"),
        );
        assert_eq!(
            st.list_automation_logs(None, None)
                .await
                .expect("redb automation_log"),
            sq_st
                .list_automation_logs(None, None)
                .await
                .expect("sqlite automation_log"),
        );
        assert_eq!(
            st.list_source_call_logs(Some("sess-1"), None)
                .await
                .expect("redb source_call_log"),
            sq_st
                .list_source_call_logs(Some("sess-1"), None)
                .await
                .expect("sqlite source_call_log"),
        );
        assert_eq!(
            st.get_memory_episode("ep-1")
                .await
                .expect("redb memory_episode"),
            sq_st
                .get_memory_episode("ep-1")
                .await
                .expect("sqlite memory_episode"),
        );
        assert_eq!(
            st.list_memory_episode_evidence("ep-1")
                .await
                .expect("redb memory_episode_evidence"),
            sq_st
                .list_memory_episode_evidence("ep-1")
                .await
                .expect("sqlite memory_episode_evidence"),
        );
        assert_eq!(
            st.get_memory_projection_state("repo-1", MemorySourceKind::Commit)
                .await
                .expect("redb memory_projection_state"),
            sq_st
                .get_memory_projection_state("repo-1", MemorySourceKind::Commit)
                .await
                .expect("sqlite memory_projection_state"),
        );
        assert_eq!(
            st.query_memory_paths_by_path(b"src/storage/migrate.rs", None, None, None)
                .await
                .expect("redb MEMORY_BY_PATH"),
            sq_st
                .query_memory_paths_by_path(b"src/storage/migrate.rs", None, None, None)
                .await
                .expect("sqlite MEMORY_BY_PATH"),
        );
        assert_eq!(
            st.get_memory_episode_search_doc("ep-1")
                .await
                .expect("redb memory_episode_search_doc"),
            sq_st
                .get_memory_episode_search_doc("ep-1")
                .await
                .expect("sqlite memory_episode_search_doc"),
        );
        assert_eq!(
            st.get_bridge_index_publication("op-bridge-idx-1")
                .await
                .expect("redb agent_bridge_index_publication"),
            sq_st
                .get_bridge_index_publication("op-bridge-idx-1")
                .await
                .expect("sqlite agent_bridge_index_publication"),
        );
    }

    async fn assert_all_capture_tables_in_redb(
        cap_source_db: Option<&Path>,
        cap_tmp_path: &Path,
        expect_copy_receipt: bool,
        expected_session_commits: usize,
        extra_post_copy_session: Option<&str>,
    ) {
        use redb::{ReadableDatabase as _, ReadableTable as _};

        use crate::storage::{
            RedbCaptureStore, SqliteCaptureStore,
            capture_store::{AgentBridgeStore, CaptureStore},
            opener::open_redb_read_only,
        };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let file_mode = fs::symlink_metadata(cap_tmp_path)
                .expect("stat capture tmp redb")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                file_mode, 0o600,
                "staged capture redb must have strict 0600 permissions"
            );
            let dir_mode = fs::symlink_metadata(cap_tmp_path.parent().unwrap())
                .expect("stat private dir")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                dir_mode, 0o700,
                ".libra/private dir must have strict 0700 permissions"
            );
        }

        let oid_a = "1111111111111111111111111111111111111111";
        let oid_b = "2222222222222222222222222222222222222222";
        let oid_c = "4444444444444444444444444444444444444444";
        let digest = "a".repeat(64);
        let digest_b = "c".repeat(64);
        let ns = "b".repeat(32);

        let cap = RedbCaptureStore::open_read_only(cap_tmp_path)
            .await
            .expect("open exported capture redb via RedbCaptureStore::open_read_only");

        // Full field-by-field verification of ALL 25 capture tables against exact source values
        let svs = cap
            .list_schema_versions()
            .await
            .expect("list capture schema_versions");
        assert!(!svs.is_empty());
        assert!(svs.iter().any(|r| r.version == CAPTURE_SCHEMA_VERSION));
        let receipt = cap.get_copy_receipt().await.expect("get copy receipt");
        assert_eq!(receipt.is_some(), expect_copy_receipt);

        assert_eq!(
            cap.get_audit_log("aud-1")
                .await
                .expect("get agent_audit_log"),
            Some(AgentAuditLogRecord {
                audit_id: "aud-1".to_string(),
                timestamp: "2026-11-21T00:05:00Z".to_string(),
                user_id: Some("u1".to_string()),
                user_name: Some("Ada".to_string()),
                action: "export".to_string(),
                checkpoint_id: "cp-1".to_string(),
                scope: "committed".to_string(),
                export_path: None,
                justification: Some("audit".to_string()),
                granted: true,
            })
        );
        assert_eq!(
            cap.get_bridge_session("bs-1")
                .await
                .expect("get agent_bridge_session"),
            Some(AgentBridgeSessionRecord {
                bridge_session_id: "bs-1".to_string(),
                source: "deepseek-harness".to_string(),
                repository_id: "repo-1".to_string(),
                worktree_id: Some("wt-1".to_string()),
                workspace_id: Some("ws-1".to_string()),
                parent_session_id: None,
                agent_id: Some("agent-1".to_string()),
                session_state: BridgeSessionState::Open,
                schema_version: 1,
                last_event_seq: 1,
                last_acked_seq: 1,
                created_at: 1_795_400_600,
                updated_at: 1_795_400_601,
            })
        );
        assert_eq!(
            cap.get_bridge_checkpoint("bcp-1")
                .await
                .expect("get agent_bridge_checkpoint"),
            Some(AgentBridgeCheckpointRecord {
                checkpoint_id: "bcp-1".to_string(),
                bridge_session_id: "bs-1".to_string(),
                agent_checkpoint_id: Some("cp-1".to_string()),
                target_oid: Some(oid_a.to_string()),
                created_at: 1_795_400_602,
            })
        );
        let expected_bridge_event = AgentBridgeEventRecord {
            event_id: 1,
            bridge_session_id: "bs-1".to_string(),
            event_seq: 1,
            event_type: "message".to_string(),
            payload_sha256: digest.clone(),
            payload: "{\"msg\":\"hi\"}".to_string(),
            operation_id: Some("op-bridge-1".to_string()),
            created_at: 1_795_400_603,
        };
        assert_eq!(
            cap.get_bridge_event_by_seq("bs-1", 1)
                .await
                .expect("get bridge_events_by_seq"),
            Some(expected_bridge_event.clone())
        );
        assert_eq!(
            cap.list_bridge_events_by_time()
                .await
                .expect("list events_by_time"),
            vec![expected_bridge_event]
        );
        assert_eq!(
            cap.get_bridge_operation("op-bridge-1")
                .await
                .expect("get agent_bridge_operation"),
            Some(AgentBridgeOperationRecord {
                operation_id: "op-bridge-1".to_string(),
                bridge_session_id: "bs-1".to_string(),
                method: "commit".to_string(),
                params_digest: digest.clone(),
                repository_id: "repo-1".to_string(),
                workspace_id: Some("ws-1".to_string()),
                status: BridgeOperationStatus::Applied,
                result_digest: Some(digest.clone()),
                created_at: 1_795_400_605,
                completed_at: Some(1_795_400_606),
            })
        );
        assert_eq!(
            cap.list_bridge_links("bs-1")
                .await
                .expect("list agent_bridge_link"),
            vec![AgentBridgeLinkRecord {
                link_id: 1,
                bridge_session_id: "bs-1".to_string(),
                source_type: BridgeLinkSourceType::Operation,
                source_id: "op-bridge-1".to_string(),
                target_type: "commit".to_string(),
                target_id: oid_a.to_string(),
                created_at: 1_795_400_604,
            }]
        );
        assert_eq!(
            cap.get_cloud_base("repo-1")
                .await
                .expect("get agent_capture_cloud_base"),
            Some(AgentCaptureCloudBaseRecord {
                repo_id: "repo-1".to_string(),
                remote_generation: 3,
                updated_at: 1_795_400_607,
            })
        );
        assert_eq!(
            cap.get_incarnation("codex", "prov-sess-1")
                .await
                .expect("get agent_capture_incarnation"),
            Some(AgentCaptureIncarnationRecord {
                agent_kind: "codex".to_string(),
                provider_session_id: "prov-sess-1".to_string(),
                next_session_sync_revision: 4,
                source_namespace: ns,
                updated_at: 1_795_400_608,
            })
        );
        let expected_sess_1 = AgentSessionRecord {
            session_id: "sess-1".to_string(),
            agent_kind: CaptureAgentKind::Codex,
            provider_session_id: "prov-sess-1".to_string(),
            thread_id: Some("thread-1".to_string()),
            state: CaptureSessionState::Active,
            working_dir: "/work".to_string(),
            worktree_id: Some("wt-1".to_string()),
            parent_commit: Some(oid_a.to_string()),
            parent_session_id: None,
            metadata_json: "{}".to_string(),
            redaction_report: "{}".to_string(),
            started_at: 1_795_400_500,
            last_event_at: 1_795_400_510,
            stopped_at: None,
            schema_version: 1,
            sync_revision: 2,
            repo_id: Some("repo-1".to_string()),
            workspace_id: Some("ws-1".to_string()),
            workspace_fence: Some(1),
            scope_state: CaptureScopeState::Scoped,
        };
        assert_eq!(
            cap.get_session("sess-1").await.expect("get agent_session"),
            Some(expected_sess_1.clone())
        );
        let sessions_desc = cap
            .list_sessions_by_started_desc()
            .await
            .expect("list sessions_by_time");
        if let Some(extra_sid) = extra_post_copy_session {
            let expected_extra = AgentSessionRecord {
                session_id: extra_sid.to_string(),
                agent_kind: CaptureAgentKind::Codex,
                provider_session_id: format!("prov-{extra_sid}"),
                thread_id: None,
                state: CaptureSessionState::Stopped,
                working_dir: "/work".to_string(),
                worktree_id: Some("wt-1".to_string()),
                parent_commit: Some(oid_b.to_string()),
                parent_session_id: None,
                metadata_json: "{}".to_string(),
                redaction_report: "{}".to_string(),
                started_at: 1_795_400_900,
                last_event_at: 1_795_400_910,
                stopped_at: Some(1_795_400_920),
                schema_version: 1,
                sync_revision: 1,
                repo_id: Some("repo-1".to_string()),
                workspace_id: Some("ws-1".to_string()),
                workspace_fence: Some(1),
                scope_state: CaptureScopeState::Scoped,
            };
            assert_eq!(sessions_desc, vec![expected_extra.clone(), expected_sess_1]);
            assert_eq!(
                cap.get_session(extra_sid)
                    .await
                    .expect("get extra post-copy session"),
                Some(expected_extra)
            );
        } else {
            assert_eq!(sessions_desc, vec![expected_sess_1]);
        }
        let expected_cp = AgentCheckpointRecord {
            checkpoint_id: "cp-1".to_string(),
            session_id: "sess-1".to_string(),
            parent_checkpoint_id: None,
            scope: CaptureCheckpointScope::Committed,
            parent_commit: Some(oid_a.to_string()),
            tree_oid: oid_b.to_string(),
            metadata_blob_oid: oid_c.to_string(),
            traces_commit: oid_b.to_string(),
            tool_use_id: None,
            subagent_session_id: None,
            description: Some("turn 1".to_string()),
            created_at: 1_795_400_505,
            sync_revision: 1,
        };
        assert_eq!(
            cap.get_checkpoint("cp-1")
                .await
                .expect("get agent_checkpoint"),
            Some(expected_cp.clone())
        );
        assert_eq!(
            cap.list_checkpoints_by_session("sess-1")
                .await
                .expect("list checkpoints_by_session"),
            vec![expected_cp]
        );
        assert_eq!(
            cap.get_checkpoint_prune_tombstone("cp-pruned-1")
                .await
                .expect("get agent_checkpoint_prune_tombstone"),
            Some(AgentCheckpointPruneTombstoneRecord {
                checkpoint_id: "cp-pruned-1".to_string(),
                session_id: "sess-1".to_string(),
                pruned_at: 1_795_400_609,
            })
        );
        assert_eq!(
            cap.get_coverage_claim("sess-1", "turn-1", 1)
                .await
                .expect("get agent_coverage_claim"),
            Some(AgentCoverageClaimRecord {
                session_id: "sess-1".to_string(),
                logical_turn_key: "turn-1".to_string(),
                coverage_schema_version: 1,
                coverage_digest: digest.clone(),
                completeness: CaptureCompleteness::Complete,
                revision: 1,
                state: CaptureCoverageClaimState::CatalogCommitted,
                attempt_checkpoint_id: None,
                owner: None,
                lease_expires_at: None,
                fence_token: Some(1),
                checkpoint_id: Some("cp-1".to_string()),
                traces_commit: Some(oid_b.to_string()),
                source_channel: CaptureSourceChannel::Live,
                created_at: 1_795_400_610,
                updated_at: 1_795_400_611,
            })
        );
        assert_eq!(
            cap.get_coverage_conflict("sess-1", "turn-1", 1)
                .await
                .expect("get agent_coverage_conflict"),
            Some(AgentCoverageConflictRecord {
                session_id: "sess-1".to_string(),
                logical_turn_key: "turn-1".to_string(),
                coverage_schema_version: 1,
                incumbent_revision: 1,
                incumbent_digest: digest.clone(),
                incumbent_checkpoint_id: Some("cp-1".to_string()),
                incoming_digest: digest_b,
                incoming_source_channel: CaptureSourceChannel::Import,
                incoming_observed_at: 1_795_400_612,
                incoming_canonical_json: "{}".to_string(),
                incoming_redaction_report_json: "{}".to_string(),
            })
        );
        assert_eq!(
            cap.list_coverage_revisions("sess-1", "turn-1", 1)
                .await
                .expect("list agent_coverage_revision"),
            vec![AgentCoverageRevisionRecord {
                session_id: "sess-1".to_string(),
                logical_turn_key: "turn-1".to_string(),
                coverage_schema_version: 1,
                revision: 1,
                checkpoint_id: "cp-1".to_string(),
                coverage_digest: digest.clone(),
                completeness: CaptureCompleteness::Complete,
                source_channel: CaptureSourceChannel::Live,
                created_at: 1_795_400_613,
            }]
        );
        assert_eq!(
            cap.get_export_job("job-1")
                .await
                .expect("get agent_export_job"),
            Some(AgentExportJobRecord {
                job_id: "job-1".to_string(),
                agent_kind: "codex".to_string(),
                provider_session_id: "prov-sess-1".to_string(),
                owner: Some("worker-1".to_string()),
                lease_expires_at: Some(1_895_400_000),
                fence_token: Some(1),
                observed_generation: 2,
                processed_generation: 1,
                state: CaptureExportJobState::Idle,
                last_error_code: None,
                created_at: 1_795_400_614,
                updated_at: 1_795_400_615,
                ttl_expires_at: 1_895_400_999,
                repo_id: Some("repo-1".to_string()),
                worktree_id: Some("wt-1".to_string()),
                workspace_id: Some("ws-1".to_string()),
                workspace_fence: Some(1),
                scope_state: CaptureScopeState::Scoped,
            })
        );
        assert_eq!(
            cap.get_import_identity("imp-1")
                .await
                .expect("get agent_import_identity"),
            Some(AgentImportIdentityRecord {
                identity_id: "imp-1".to_string(),
                agent_kind: "codex".to_string(),
                provider_session_id: "prov-sess-1".to_string(),
                source_kind: "jsonl".to_string(),
                source_id: "src-1".to_string(),
                schema_version: 1,
                observed_digest: Some(digest.clone()),
                committed_digest: Some(digest.clone()),
                attempt_id: None,
                attempt_checkpoint_id: None,
                next_ordinal: 2,
                state: CaptureImportIdentityState::Committed,
                owner: None,
                lease_expires_at: None,
                fence_token: None,
                last_error_code: None,
                created_at: 1_795_400_616,
                updated_at: 1_795_400_617,
                repo_id: Some("repo-1".to_string()),
                worktree_id: Some("wt-1".to_string()),
                workspace_id: Some("ws-1".to_string()),
                workspace_fence: Some(1),
                scope_state: CaptureScopeState::Scoped,
            })
        );
        let expected_tombstone = AgentImportTombstoneRecord {
            tombstone_id: "tomb-1".to_string(),
            agent_kind: "codex".to_string(),
            provider_session_id: "prov-erased-1".to_string(),
            erased_session_id: "sess-erased-1".to_string(),
            source_fingerprint: Some("fp-1".to_string()),
            erased_at: 1_795_400_618,
        };
        assert_eq!(
            cap.get_import_tombstone("tomb-1")
                .await
                .expect("get agent_import_tombstone"),
            Some(expected_tombstone.clone())
        );
        assert_eq!(
            cap.list_import_tombstones()
                .await
                .expect("list import tombstones"),
            vec![expected_tombstone]
        );
        let commits = cap
            .list_session_commits("sess-1")
            .await
            .expect("list agent_session_commit");
        assert_eq!(commits.len(), expected_session_commits);
        assert!(commits.contains(&AgentSessionCommitRecord {
            session_id: "sess-1".to_string(),
            hash_kind: CaptureHashKind::Sha1,
            commit_oid: oid_a.to_string(),
        }));
        assert_eq!(
            cap.get_subagent_content_claim("sess-1", "codex", "sub-key-1", 1)
                .await
                .expect("get agent_subagent_content_claim"),
            Some(AgentSubagentContentClaimRecord {
                parent_session_id: "sess-1".to_string(),
                provider_kind: "codex".to_string(),
                source_key: "sub-key-1".to_string(),
                content_schema_version: 1,
                current_revision: 1,
                current_checkpoint_id: Some("cp-1".to_string()),
                current_digest: Some(digest.clone()),
                state: CaptureSubagentClaimState::Idle,
                attempt_digest: None,
                attempt_checkpoint_id: None,
                owner: None,
                lease_expires_at: None,
                fence_token: 1,
                created_at: 1_795_400_619,
                updated_at: 1_795_400_620,
                revision_cursor: 1,
                sync_revision: 1,
            })
        );
        assert_eq!(
            cap.list_subagent_content_revisions("sess-1", "codex", "sub-key-1", 1)
                .await
                .expect("list agent_subagent_content_revision"),
            vec![AgentSubagentContentRevisionRecord {
                parent_session_id: "sess-1".to_string(),
                provider_kind: "codex".to_string(),
                source_key: "sub-key-1".to_string(),
                content_schema_version: 1,
                revision: 1,
                checkpoint_id: "cp-1".to_string(),
                content_digest: digest,
                source_channel: CaptureSubagentSourceChannel::Live,
                partial: false,
                created_at: 1_795_400_621,
            }]
        );
        assert_eq!(
            cap.get_subagent_link("cp-1")
                .await
                .expect("get agent_subagent_link"),
            Some(AgentSubagentLinkRecord {
                content_checkpoint_id: "cp-1".to_string(),
                parent_session_id: "sess-1".to_string(),
                link_state: CaptureSubagentLinkState::Resolved,
                boundary_checkpoint_id: Some("cp-1".to_string()),
                stable_subagent_id: Some("sub-stable-1".to_string()),
                created_at: 1_795_400_622,
                updated_at: 1_795_400_623,
                sync_revision: 1,
            })
        );
        let expected_usage = AgentUsageStatsRecord {
            id: "usage-1".to_string(),
            session_id: Some("sess-1".to_string()),
            thread_id: Some("thread-1".to_string()),
            agent_run_id: Some("run-1".to_string()),
            run_id: Some("run-1".to_string()),
            provider: "openai".to_string(),
            model: "gpt-5".to_string(),
            request_kind: "chat".to_string(),
            intent: Some("code".to_string()),
            prompt_tokens: 100,
            completion_tokens: 50,
            cached_tokens: 20,
            reasoning_tokens: 10,
            total_tokens: 150,
            tool_call_count: 2,
            wall_clock_ms: 1200,
            provider_latency_ms: Some(900),
            cost_estimate_micro_dollars: Some(3500),
            cost_usd: Some(0.0035),
            usage_estimated: false,
            started_at: Some("2026-11-21T00:05:00Z".to_string()),
            finished_at: Some("2026-11-21T00:05:01Z".to_string()),
            success: true,
            error_kind: None,
            schema_version: 1,
            created_at: "2026-11-21T00:05:01Z".to_string(),
            agent_name: Some("codex".to_string()),
            repo_id: Some("repo-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            event_id: Some("ev-usage-1".to_string()),
        };
        assert_eq!(
            cap.get_usage_stats("usage-1")
                .await
                .expect("get agent_usage_stats"),
            Some(expected_usage.clone())
        );
        assert_eq!(
            cap.list_usage_stats_by_session("sess-1")
                .await
                .expect("list usage_stats_by_session"),
            vec![expected_usage]
        );
        assert_eq!(
            cap.get_workspace_scope_audit("wsa-1")
                .await
                .expect("get agent_workspace_scope_audit"),
            Some(AgentWorkspaceScopeAuditRecord {
                audit_id: "wsa-1".to_string(),
                action: "adopt_legacy_capture_scope".to_string(),
                agent_kind: "codex".to_string(),
                provider_session_id: "prov-sess-1".to_string(),
                repo_id: "repo-1".to_string(),
                worktree_id: "wt-1".to_string(),
                workspace_id: Some("ws-1".to_string()),
                workspace_fence: Some(1),
                actor: Some("ada".to_string()),
                created_at: 1_795_400_624,
            })
        );

        // When authoritative `capture.db` exists (Layouts B & C), additionally
        // cross-check direct SqliteCaptureStore vs RedbCaptureStore parity.
        if let Some(cap_db) = cap_source_db {
            let sq_cap = SqliteCaptureStore::open_read_only(cap_db)
                .await
                .expect("open source capture sqlite via SqliteCaptureStore::open_read_only");
            assert_eq!(svs, sq_cap.list_schema_versions().await.expect("sq svs"));
            assert_eq!(
                receipt,
                sq_cap.get_copy_receipt().await.expect("sq receipt")
            );
            assert_eq!(
                commits,
                sq_cap
                    .list_session_commits("sess-1")
                    .await
                    .expect("sq session commits")
            );
            assert_eq!(
                cap.list_bridge_events("bs-1").await.expect("redb events"),
                sq_cap.list_bridge_events("bs-1").await.expect("sq events")
            );
            assert_eq!(
                cap.list_bridge_operations("bs-1")
                    .await
                    .expect("redb bridge ops"),
                sq_cap
                    .list_bridge_operations("bs-1")
                    .await
                    .expect("sq bridge ops")
            );
            assert_eq!(
                cap.list_workspace_scope_audits("codex", "prov-sess-1")
                    .await
                    .expect("redb wsa list"),
                sq_cap
                    .list_workspace_scope_audits("codex", "prov-sess-1")
                    .await
                    .expect("sq wsa list")
            );
        }
        drop(cap);

        // Explicitly verify ALL capture secondary index tables are populated!
        let ro = open_redb_read_only(cap_tmp_path, DatabaseRole::Capture)
            .expect("open capture tmp redb for secondary index verification");
        let rtx = ro.begin_read().expect("begin capture read tx");
        assert!(
            rtx.open_table(redb_cap::SESSIONS_BY_PROVIDER_TABLE)
                .expect("open sessions_by_provider")
                .get(("codex", "prov-sess-1"))
                .expect("get sessions_by_provider")
                .is_some(),
            "sessions_by_provider index must be populated"
        );
        assert!(
            rtx.open_table(redb_cap::BRIDGE_LINKS_BY_KEY_TABLE)
                .expect("open bridge_links_by_key")
                .get(("operation", "op-bridge-1", "commit", oid_a))
                .expect("get bridge_links_by_key")
                .is_some(),
            "bridge_links_by_key index must be populated"
        );
        assert!(
            rtx.open_table(redb_cap::USAGE_STATS_BY_SESSION_EVENT_TABLE)
                .expect("open usage_stats_by_session_event")
                .get(("sess-1", "ev-usage-1"))
                .expect("get usage_stats_by_session_event")
                .is_some(),
            "usage_stats_by_session_event index must be populated"
        );
        assert!(
            rtx.open_table(redb_cap::IMPORT_TOMBSTONES_BY_PROVIDER_TABLE)
                .expect("open import_tombstones_by_provider")
                .get(("codex", "prov-erased-1"))
                .expect("get import_tombstones_by_provider")
                .is_some(),
            "import_tombstones_by_provider index must be populated"
        );
        assert!(
            rtx.open_table(redb_cap::IMPORT_TOMBSTONES_BY_SESSION_TABLE)
                .expect("open import_tombstones_by_session")
                .get("sess-erased-1")
                .expect("get import_tombstones_by_session")
                .is_some(),
            "import_tombstones_by_session index must be populated"
        );
    }

    /// Advance `sqlite_sequence` high-water marks in the repository SQLite DB
    /// by inserting high-ID rows and deleting them (and optionally clearing all
    /// rows to test empty-after-delete sequence continuity).
    async fn advance_and_delete_repo_sequences(
        git_db: &Path,
        cfg_hw: i64,
        ckv_hw: i64,
        reflog_hw: i64,
        wt_hw: i64,
        clear_all_rows: bool,
    ) {
        let mut conn = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(git_db)
                .create_if_missing(false),
        )
        .await
        .expect("open git_db for sequence high-water setup");
        conn.execute("BEGIN IMMEDIATE").await.expect("begin tx");
        sea_orm::sqlx::query(
            "INSERT INTO config (id, configuration, name, key, value) VALUES (?, 'user', NULL, 'deleted_hw', 'v')",
        )
        .bind(cfg_hw)
        .execute(&mut conn)
        .await
        .expect("insert config hw");
        sea_orm::sqlx::query(
            "INSERT INTO config_kv (id, key, value, encrypted) VALUES (?, 'deleted.hw.key', 'v', 0)",
        )
        .bind(ckv_hw)
        .execute(&mut conn)
        .await
        .expect("insert config_kv hw");
        sea_orm::sqlx::query(
            "INSERT INTO reflog (id, ref_name, old_oid, new_oid, committer_name, committer_email, timestamp, action, message, worktree_id) VALUES (?, 'refs/heads/tmp', '0000000000000000000000000000000000000000', '1111111111111111111111111111111111111111', 'Ada', 'a@b', 1795400999, 'tmp', 'deleted', NULL)",
        )
        .bind(reflog_hw)
        .execute(&mut conn)
        .await
        .expect("insert reflog hw");
        sea_orm::sqlx::query(
            "INSERT INTO worktree_intent_journal (id, op, worktree_id, payload, stage, created_at) VALUES (?, 'add', 'wt-del', '{}', 'staged', 1795400999)",
        )
        .bind(wt_hw)
        .execute(&mut conn)
        .await
        .expect("insert worktree_intent_journal hw");

        if clear_all_rows {
            conn.execute("DELETE FROM config")
                .await
                .expect("clear config");
            conn.execute("DELETE FROM config_kv")
                .await
                .expect("clear config_kv");
            conn.execute("DELETE FROM reflog")
                .await
                .expect("clear reflog");
            conn.execute("DELETE FROM worktree_intent_journal")
                .await
                .expect("clear worktree_intent_journal");
        } else {
            sea_orm::sqlx::query("DELETE FROM config WHERE id = ?")
                .bind(cfg_hw)
                .execute(&mut conn)
                .await
                .expect("delete config hw");
            sea_orm::sqlx::query("DELETE FROM config_kv WHERE id = ?")
                .bind(ckv_hw)
                .execute(&mut conn)
                .await
                .expect("delete config_kv hw");
            sea_orm::sqlx::query("DELETE FROM reflog WHERE id = ?")
                .bind(reflog_hw)
                .execute(&mut conn)
                .await
                .expect("delete reflog hw");
            sea_orm::sqlx::query("DELETE FROM worktree_intent_journal WHERE id = ?")
                .bind(wt_hw)
                .execute(&mut conn)
                .await
                .expect("delete worktree_intent_journal hw");
        }
        conn.execute("COMMIT").await.expect("commit sequence setup");
        conn.close().await.expect("close sequence setup conn");
    }

    /// Advance `sqlite_sequence` high-water marks in the capture SQLite DB
    /// (`agent_bridge_event` & `agent_bridge_link`) by inserting high-ID rows
    /// and deleting them (or clearing all rows when `clear_all_rows` is true).
    async fn advance_and_delete_capture_sequences(
        db_path: &Path,
        ev_hw: i64,
        link_hw: i64,
        clear_all_rows: bool,
    ) {
        let mut conn = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(db_path)
                .create_if_missing(false),
        )
        .await
        .expect("open capture db for sequence high-water setup");
        let digest = "a".repeat(64);
        conn.execute("BEGIN IMMEDIATE").await.expect("begin tx");
        sea_orm::sqlx::query(
            "INSERT INTO agent_bridge_event (event_id, bridge_session_id, event_seq, event_type, payload_sha256, payload, operation_id, created_at) VALUES (?, 'bs-1', 999, 'deleted', ?, '{}', NULL, 1795400999)",
        )
        .bind(ev_hw)
        .bind(&digest)
        .execute(&mut conn)
        .await
        .expect("insert bridge_event hw");
        sea_orm::sqlx::query(
            "INSERT INTO agent_bridge_link (link_id, bridge_session_id, source_type, source_id, target_type, target_id, created_at) VALUES (?, 'bs-1', 'operation', 'op-del', 'commit', '1111111111111111111111111111111111111111', 1795400999)",
        )
        .bind(link_hw)
        .execute(&mut conn)
        .await
        .expect("insert bridge_link hw");

        if clear_all_rows {
            conn.execute("DELETE FROM agent_bridge_event")
                .await
                .expect("clear bridge_event");
            conn.execute("DELETE FROM agent_bridge_link")
                .await
                .expect("clear bridge_link");
        } else {
            sea_orm::sqlx::query("DELETE FROM agent_bridge_event WHERE event_id = ?")
                .bind(ev_hw)
                .execute(&mut conn)
                .await
                .expect("delete bridge_event hw");
            sea_orm::sqlx::query("DELETE FROM agent_bridge_link WHERE link_id = ?")
                .bind(link_hw)
                .execute(&mut conn)
                .await
                .expect("delete bridge_link hw");
        }
        conn.execute("COMMIT")
            .await
            .expect("commit capture sequence setup");
        conn.close().await.expect("close capture sequence conn");
    }

    /// Open writable copies of the exported `.tmp` redb stores and insert new
    /// rows into ALL 6 sequence-backed tables, proving every newly allocated ID
    /// is strictly greater than the deleted SQLite `sqlite_sequence` high-water
    /// mark (`config`, `config_kv`, `reflog`, `worktree_intent_journal`,
    /// `agent_bridge_event`, `agent_bridge_link`).
    #[allow(clippy::too_many_arguments)]
    async fn assert_post_export_sequence_continuity(
        git_tmp_path: &Path,
        cap_tmp_path: &Path,
        cfg_hw: u64,
        ckv_hw: u64,
        reflog_hw: u64,
        wt_hw: i64,
        ev_hw: i64,
        link_hw: i64,
    ) {
        use redb::{ReadableDatabase as _, ReadableTable as _};

        use crate::storage::{
            BridgeEventAppendInput, BridgeLinkWrite, RedbCaptureStore, RedbGitStateStore,
            RedbGitStore,
            capture_store::AgentBridgeStore,
            config_store::ConfigStore,
            git_state_store::{GitStateStore, WorktreeIntentWrite},
            git_store::{GitStore, ReflogWrite},
            opener::open_redb_read_only,
            redb::RedbConfigStore,
        };

        let scratch = tempdir().expect("create writable post-export scratch dir");
        let git_rw_path = scratch.path().join("git.rw.redb");
        let priv_dir = scratch.path().join("private");
        fs::create_dir_all(&priv_dir).expect("create private scratch dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&priv_dir, fs::Permissions::from_mode(0o700))
                .expect("chmod 0700 private scratch dir");
        }
        let cap_rw_path = priv_dir.join("capture.rw.redb");
        fs::copy(git_tmp_path, &git_rw_path).expect("copy git tmp for rw test");
        fs::copy(cap_tmp_path, &cap_rw_path).expect("copy cap tmp for rw test");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&cap_rw_path, fs::Permissions::from_mode(0o600))
                .expect("chmod 0600 capture scratch copy");
        }

        // 1. config & config_kv
        {
            let cfg = RedbConfigStore::open_read_write(&git_rw_path, DatabaseRole::Repository)
                .expect("open git rw config store");
            cfg.insert_legacy_entry(&LegacyConfigEntry {
                configuration: "user".to_string(),
                name: None,
                key: "post_export_seq".to_string(),
                value: "ok".to_string(),
            })
            .await
            .expect("insert post-export legacy config");
            cfg.set("post.export.seq.key", "ok", false)
                .await
                .expect("set post-export config_kv");
            drop(cfg);

            let ro = open_redb_read_only(&git_rw_path, DatabaseRole::Repository)
                .expect("reopen git rw ro");
            let rtx = ro.begin_read().expect("begin git ro tx");
            let max_cfg_id = rtx
                .open_table(redb_cfg::CONFIG_LEGACY_TABLE)
                .expect("open config")
                .iter()
                .expect("iter config")
                .map(|item| item.expect("cfg item").0.value())
                .max()
                .expect("non-empty config");
            assert_eq!(
                max_cfg_id,
                cfg_hw + 1,
                "config sequence must advance past deleted high-water mark {cfg_hw}"
            );
            let max_ckv_id = rtx
                .open_table(redb_cfg::CONFIG_KV_TABLE)
                .expect("open config_kv")
                .iter()
                .expect("iter config_kv")
                .map(|item| item.expect("ckv item").0.value())
                .max()
                .expect("non-empty config_kv");
            assert_eq!(
                max_ckv_id,
                ckv_hw + 1,
                "config_kv sequence must advance past deleted high-water mark {ckv_hw}"
            );
        }

        // 2. reflog
        {
            let git = RedbGitStore::open_read_write(&git_rw_path).expect("open git rw store");
            let oid = "5555555555555555555555555555555555555555";
            git.update_ref_cas(
                &RefKey::LocalBranch {
                    name: "seq-branch".to_string(),
                },
                None,
                Some(&RefTarget::Direct(oid.to_string())),
                &[ReflogWrite {
                    ref_name: "refs/heads/seq-branch".to_string(),
                    old_oid: "0".repeat(40),
                    new_oid: oid.to_string(),
                    timestamp: 1_795_401_000,
                    committer_name: "Ada".to_string(),
                    committer_email: "ada@example.com".to_string(),
                    action: "branch".to_string(),
                    message: "post-export seq".to_string(),
                    worktree_id: None,
                }],
            )
            .await
            .expect("insert post-export reflog");
            drop(git);

            let ro = open_redb_read_only(&git_rw_path, DatabaseRole::Repository)
                .expect("reopen git rw ro for reflog");
            let rtx = ro.begin_read().expect("begin git ro tx");
            let max_reflog_id = rtx
                .open_table(redb_git::REFLOG_TABLE)
                .expect("open reflog")
                .iter()
                .expect("iter reflog")
                .map(|item| item.expect("reflog item").0.value())
                .max()
                .expect("non-empty reflog");
            assert_eq!(
                max_reflog_id,
                reflog_hw + 1,
                "reflog sequence must advance past deleted high-water mark {reflog_hw}"
            );
        }

        // 3. worktree_intent_journal
        {
            let st =
                RedbGitStateStore::open_read_write(&git_rw_path).expect("open git state rw store");
            let rec = st
                .append_worktree_intent(&WorktreeIntentWrite {
                    op: WorktreeIntentOp::Add,
                    worktree_id: Some("wt-post-seq".to_string()),
                    payload: "{}".to_string(),
                    stage: Some("staged".to_string()),
                    created_at: 1_795_401_001,
                })
                .await
                .expect("append post-export worktree intent");
            assert_eq!(
                rec.id,
                wt_hw + 1,
                "worktree_intent_journal sequence must advance past deleted high-water mark {wt_hw}"
            );
        }

        // 4. agent_bridge_event & agent_bridge_link
        {
            let cap = RedbCaptureStore::open_read_write(
                &cap_rw_path,
                crate::storage::opener::RedbRepairContext::WriteCommand,
            )
            .await
            .expect("open capture rw store");
            cap.append_and_ack_events(
                "bs-1",
                &[BridgeEventAppendInput {
                    event_seq: 2,
                    event_type: "post_export".to_string(),
                    payload: "{\"seq\":2}".to_string(),
                    operation_id: None,
                }],
                1_795_401_002,
            )
            .await
            .expect("append post-export bridge event");
            let ev = cap
                .get_bridge_event_by_seq("bs-1", 2)
                .await
                .expect("get post-export bridge event")
                .expect("post-export bridge event must exist");
            assert_eq!(
                ev.event_id,
                ev_hw + 1,
                "agent_bridge_event sequence must advance past deleted high-water mark {ev_hw}"
            );

            let (link, inserted) = cap
                .insert_bridge_link(&BridgeLinkWrite {
                    bridge_session_id: "bs-1".to_string(),
                    source_type: BridgeLinkSourceType::Operation,
                    source_id: "op-post-seq".to_string(),
                    target_type: "commit".to_string(),
                    target_id: "5555555555555555555555555555555555555555".to_string(),
                    created_at: 1_795_401_003,
                })
                .await
                .expect("insert post-export bridge link");
            assert!(inserted);
            assert_eq!(
                link.link_id,
                link_hw + 1,
                "agent_bridge_link sequence must advance past deleted high-water mark {link_hw}"
            );
        }
    }

    #[tokio::test]
    async fn forward_phase1_offline_wal_checkpoint_flushes_to_standalone_main_db() {
        use crate::storage::{
            RedbCaptureStore, SqliteCaptureStore, capture_store::CaptureStore,
            config_store::ConfigStore, manifest::INITIAL_STORAGE_GENERATION, redb::RedbConfigStore,
            sqlite::SqliteConfigStore,
        };

        let dir = tempdir().expect("create offline WAL test dir");
        let storage = dir.path().join(".libra");
        let git_db = git_sqlite_db_path(&storage);
        let cap_db = capture_sqlite_db_path(&storage);

        // Create real full-schema Repository + detached Capture SQLite DBs.
        fs::create_dir_all(&storage).expect("create .libra dir");
        {
            let _cfg = SqliteConfigStore::create(&git_db, DatabaseRole::Repository)
                .await
                .expect("create git sqlite db");
            let _cap = SqliteCaptureStore::create(&cap_db)
                .await
                .expect("create capture sqlite db");
        }
        // Detach inline capture tables from `libra.db` so this fixture is a
        // clean Layout C dual-WAL repository.
        {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&git_db)
                    .create_if_missing(false),
            )
            .await
            .expect("open git_db to detach inline capture tables");
            conn.execute("PRAGMA foreign_keys = OFF;")
                .await
                .expect("disable fk");
            for tbl in LEGACY_INLINE_CAPTURE_TABLES {
                conn.execute(AssertSqlSafe(format!("DROP TABLE IF EXISTS \"{tbl}\"")))
                    .await
                    .expect("drop inline capture table");
            }
            conn.close().await.expect("close detach conn");
        }

        // Write committed rows EXCLUSIVELY into `-wal` (never checkpointed into
        // the main `.db` files) using `wal_autocheckpoint = 0` +
        // `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE = 1` on BOTH `libra.db` and `capture.db`.
        {
            let mut git_conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&git_db)
                    .create_if_missing(false)
                    .journal_mode(sea_orm::sqlx::sqlite::SqliteJournalMode::Wal)
                    .synchronous(SqliteSynchronous::Full),
            )
            .await
            .expect("open git_db WAL writer");
            git_conn
                .execute("PRAGMA wal_autocheckpoint = 0;")
                .await
                .expect("disable git wal_autocheckpoint");
            set_wal_checkpoint_on_close(&mut git_conn, DatabaseRole::Repository, true)
                .await
                .expect("disable git close checkpoint");
            for i in 0..16usize {
                sea_orm::sqlx::query(
                    "INSERT INTO config_kv (key, value, encrypted) VALUES (?, ?, 0)",
                )
                .bind(format!("wal.only.key.{i:02}"))
                .bind(format!("wal-value-{i:02}"))
                .execute(&mut git_conn)
                .await
                .expect("insert WAL-only config_kv row");
            }
            // Keep `-shm` around by leaving a read snapshot active briefly, then
            // closing the writer with NO_CKPT_ON_CLOSE=1.
            git_conn.close().await.expect("close git WAL writer");
        }
        {
            let mut cap_conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&cap_db)
                    .create_if_missing(false)
                    .journal_mode(sea_orm::sqlx::sqlite::SqliteJournalMode::Wal)
                    .synchronous(SqliteSynchronous::Full),
            )
            .await
            .expect("open cap_db WAL writer");
            cap_conn
                .execute("PRAGMA wal_autocheckpoint = 0;")
                .await
                .expect("disable cap wal_autocheckpoint");
            set_wal_checkpoint_on_close(&mut cap_conn, DatabaseRole::Capture, true)
                .await
                .expect("disable cap close checkpoint");
            for i in 0..8usize {
                sea_orm::sqlx::query(
                    "INSERT INTO agent_audit_log (audit_id, timestamp, user_id, user_name, action, checkpoint_id, scope, export_path, justification, granted) VALUES (?, '2026-11-21T01:00:00Z', 'u1', 'Ada', 'export', 'cp-wal', 'turn', NULL, 'wal-only', 1)",
                )
                .bind(format!("wal-audit-{i:02}"))
                .execute(&mut cap_conn)
                .await
                .expect("insert WAL-only capture row");
            }
            cap_conn.close().await.expect("close cap WAL writer");
        }

        let git_wal = sqlite_sidecar_path(&git_db, "-wal");
        let git_shm = sqlite_sidecar_path(&git_db, "-shm");
        let cap_wal = sqlite_sidecar_path(&cap_db, "-wal");
        let cap_shm = sqlite_sidecar_path(&cap_db, "-shm");
        assert!(
            fs::metadata(&git_wal)
                .expect("git -wal must exist before quiescence")
                .len()
                > 0
        );
        assert!(
            fs::metadata(&cap_wal)
                .expect("capture -wal must exist before quiescence")
                .len()
                > 0
        );

        // Prove AC1 precondition: copying ONLY the raw main `.db` files (without
        // `-wal`) yields ZERO of the newly committed WAL-only rows!
        {
            let probe_dir = tempdir().expect("create pre-quiesce probe dir");
            let git_only = probe_dir.path().join("git-only.db");
            let cap_only = probe_dir.path().join("cap-only.db");
            fs::copy(&git_db, &git_only).expect("copy raw git_db");
            fs::copy(&cap_db, &cap_only).expect("copy raw cap_db");

            let mut c = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&git_only)
                    .read_only(true),
            )
            .await
            .expect("open raw git_db snapshot");
            let count: i64 = sea_orm::sqlx::query_scalar(
                "SELECT COUNT(*) FROM config_kv WHERE key LIKE 'wal.only.key.%'",
            )
            .fetch_one(&mut c)
            .await
            .expect("count WAL-only git rows in main db");
            assert_eq!(
                count, 0,
                "precondition: WAL-only rows must NOT yet be flushed into main libra.db"
            );
            c.close().await.expect("close raw git probe");

            let mut cc = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&cap_only)
                    .read_only(true),
            )
            .await
            .expect("open raw cap_db snapshot");
            let cap_count: i64 = sea_orm::sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_audit_log WHERE audit_id LIKE 'wal-audit-%'",
            )
            .fetch_one(&mut cc)
            .await
            .expect("count WAL-only capture rows in main db");
            assert_eq!(
                cap_count, 0,
                "precondition: WAL-only rows must NOT yet be flushed into main capture.db"
            );
            cc.close().await.expect("close raw cap probe");
        }

        // Execute Phase 1 quiescence (direct `PRAGMA journal_mode = DELETE;`
        // under `locking_mode = EXCLUSIVE`) + Phase 1 `.tmp` export!
        let mut guard = quiesce_forward_repository_phase1(&storage)
            .await
            .expect("offline WAL Phase 1 quiescence must succeed");
        assert_eq!(guard.sqlite.git.journal_mode(), "delete");
        assert_eq!(
            guard
                .sqlite
                .capture
                .as_ref()
                .expect("capture source present")
                .journal_mode(),
            "delete"
        );
        // AC1: Both `-wal` and `-shm` companion files are completely eliminated.
        assert!(!git_wal.exists(), "libra.db-wal must be eliminated");
        assert!(!git_shm.exists(), "libra.db-shm must be eliminated");
        assert!(!cap_wal.exists(), "capture.db-wal must be eliminated");
        assert!(!cap_shm.exists(), "capture.db-shm must be eliminated");

        let report =
            export_forward_phase1_to_redb_tmp(&mut guard, &storage, INITIAL_STORAGE_GENERATION)
                .await
                .expect("export after offline WAL checkpoint must succeed");
        assert_eq!(report.layout, LegacyRepositoryLayout::LayoutC);
        guard.close().await.expect("close quiesce guard");

        // AC2: Copy ONLY the standalone main `.db` files to an isolated dir and
        // prove 100% of original WAL-only committed rows are independently
        // readable and pass `PRAGMA integrity_check == "ok"`.
        {
            let standalone_dir = tempdir().expect("create standalone probe dir");
            let standalone_git = standalone_dir.path().join("libra.db");
            let standalone_cap = standalone_dir.path().join("capture.db");
            fs::copy(&git_db, &standalone_git).expect("copy standalone libra.db");
            fs::copy(&cap_db, &standalone_cap).expect("copy standalone capture.db");

            let mut c = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&standalone_git)
                    .read_only(true),
            )
            .await
            .expect("open standalone libra.db");
            let integrity: String = sea_orm::sqlx::query_scalar("PRAGMA integrity_check;")
                .fetch_one(&mut c)
                .await
                .expect("check standalone libra.db integrity");
            assert_eq!(integrity, "ok");
            let mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode;")
                .fetch_one(&mut c)
                .await
                .expect("check standalone libra.db journal_mode");
            assert_eq!(mode.to_ascii_lowercase(), "delete");
            let count: i64 = sea_orm::sqlx::query_scalar(
                "SELECT COUNT(*) FROM config_kv WHERE key LIKE 'wal.only.key.%'",
            )
            .fetch_one(&mut c)
            .await
            .expect("count flushed git rows");
            assert_eq!(
                count, 16,
                "100% of WAL-only git rows must live in standalone libra.db"
            );
            c.close().await.expect("close standalone libra.db");

            let mut cc = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&standalone_cap)
                    .read_only(true),
            )
            .await
            .expect("open standalone capture.db");
            let cap_integrity: String = sea_orm::sqlx::query_scalar("PRAGMA integrity_check;")
                .fetch_one(&mut cc)
                .await
                .expect("check standalone capture.db integrity");
            assert_eq!(cap_integrity, "ok");
            let cap_mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode;")
                .fetch_one(&mut cc)
                .await
                .expect("check standalone capture.db journal_mode");
            assert_eq!(cap_mode.to_ascii_lowercase(), "delete");
            let cap_count: i64 = sea_orm::sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_audit_log WHERE audit_id LIKE 'wal-audit-%'",
            )
            .fetch_one(&mut cc)
            .await
            .expect("count flushed capture rows");
            assert_eq!(
                cap_count, 8,
                "100% of WAL-only capture rows must live in standalone capture.db"
            );
            cc.close().await.expect("close standalone capture.db");
        }

        // Also prove 100% of WAL-only rows made it into the staged `.tmp` redb files.
        let redb_cfg =
            RedbConfigStore::open_read_only(&report.git_tmp_path, DatabaseRole::Repository)
                .expect("open exported git redb tmp");
        for i in 0..16usize {
            let entry = redb_cfg
                .get(&format!("wal.only.key.{i:02}"))
                .await
                .expect("read exported wal key")
                .expect("exported wal key must exist");
            assert_eq!(entry.value, format!("wal-value-{i:02}"));
        }
        let redb_cap = RedbCaptureStore::open_read_only(&report.capture_tmp_path)
            .await
            .expect("open exported capture redb tmp");
        for i in 0..8usize {
            assert!(
                redb_cap
                    .get_audit_log(&format!("wal-audit-{i:02}"))
                    .await
                    .expect("read exported wal audit")
                    .is_some()
            );
        }
    }

    #[tokio::test]
    async fn forward_phase1_all_three_legacy_layouts() {
        use redb::{ReadableDatabase as _, TableHandle as _};

        use crate::storage::{
            SqliteCaptureStore, manifest::INITIAL_STORAGE_GENERATION, opener::open_redb_read_only,
        };

        // --------------------------------------------------------------------
        // Layout A (AC3): Unsplit legacy repo (only `libra.db`, 61 + 23 tables)
        // --------------------------------------------------------------------
        {
            let dir = tempdir().expect("create Layout A repo dir");
            let storage = dir.path().join(".libra");
            let git_db = git_sqlite_db_path(&storage);

            populate_all_61_repository_tables(&git_db).await;
            populate_capture_rows_via_sql(&git_db, 0, None).await;
            advance_and_delete_repo_sequences(&git_db, 40, 600, 50, 60, false).await;
            advance_and_delete_capture_sequences(&git_db, 70, 80, false).await;
            switch_fixture_db_to_delete_mode(&git_db).await;

            let git_bytes_before = fs::read(&git_db).expect("snapshot Layout A libra.db");
            assert!(!capture_sqlite_db_path(&storage).exists());

            let mut guard = quiesce_forward_repository_phase1(&storage)
                .await
                .expect("Layout A quiesce must succeed");
            let report =
                export_forward_phase1_to_redb_tmp(&mut guard, &storage, INITIAL_STORAGE_GENERATION)
                    .await
                    .expect("Layout A export must succeed");
            guard.close().await.expect("close Layout A guard");

            assert_eq!(report.layout, LegacyRepositoryLayout::LayoutA);
            assert_eq!(report.git_tmp_path, git_redb_tmp_path(&storage, 1));
            assert_eq!(report.capture_tmp_path, capture_redb_tmp_path(&storage, 1));
            assert_eq!(
                report.git_table_counts.len(),
                REPOSITORY_ACTIVE_SOURCE_TABLES_COUNT
            );
            assert_eq!(
                report.capture_table_counts.len(),
                CAPTURE_SOURCE_TABLES_COUNT
            );
            assert_eq!(
                report.max_batch_rows, FORWARD_MIGRATION_STREAM_BATCH_ROWS,
                "525 config_kv rows must stream with exact 500-row max batch cap"
            );
            for (tbl, cnt) in &report.git_table_counts {
                assert!(*cnt >= 1, "Layout A repo table `{tbl}` exported 0 rows");
            }
            for tbl in LEGACY_INLINE_CAPTURE_TABLES {
                assert!(
                    report.capture_table_counts.get(tbl).copied().unwrap_or(0) >= 1,
                    "Layout A inline capture table `{tbl}` exported 0 rows"
                );
            }
            assert_eq!(report.capture_table_counts["schema_versions"], 1);
            assert_eq!(
                report.capture_table_counts["capture_catalog_copy_receipt"],
                0
            );

            assert_all_61_repo_tables_in_redb(&git_db, &report.git_tmp_path).await;
            assert_all_capture_tables_in_redb(None, &report.capture_tmp_path, false, 1, None).await;
            assert_post_export_sequence_continuity(
                &report.git_tmp_path,
                &report.capture_tmp_path,
                40,
                600,
                50,
                60,
                70,
                80,
            )
            .await;

            assert_eq!(
                fs::read(&git_db).expect("re-read Layout A libra.db"),
                git_bytes_before,
                "Layout A export must leave libra.db byte-for-byte unchanged"
            );
        }

        // --------------------------------------------------------------------
        // Layout B (AC4): Copied-not-detached dual DBs with complete receipt +
        // 100% PK coverage (including >500 keys to test multi-page PK probe)
        // + post-copy row present only in authoritative `capture.db`
        // --------------------------------------------------------------------
        {
            let dir = tempdir().expect("create Layout B repo dir");
            let storage = dir.path().join(".libra");
            let git_db = git_sqlite_db_path(&storage);
            let cap_db = capture_sqlite_db_path(&storage);

            populate_all_61_repository_tables(&git_db).await;
            populate_capture_rows_via_sql(&git_db, 520, None).await;
            advance_and_delete_repo_sequences(&git_db, 41, 601, 51, 61, false).await;
            // Advance inline `libra.db` capture sequences HIGHER than `capture.db`
            // (170/180 vs 71/81) to verify Layout B takes the max across both held
            // connections and never moves capture sequences backwards.
            advance_and_delete_capture_sequences(&git_db, 170, 180, false).await;

            {
                let cap_store = SqliteCaptureStore::create(&cap_db)
                    .await
                    .expect("create Layout B capture.db");
                use crate::storage::capture_store::CaptureStore;
                cap_store
                    .put_copy_receipt(&CaptureCatalogCopyReceiptRecord {
                        id: 1,
                        copied_at: "2026-11-21T02:00:00Z".to_string(),
                    })
                    .await
                    .expect("write Layout B copy receipt");
            }
            populate_capture_rows_via_sql(&cap_db, 520, Some("sess-post-copy")).await;
            advance_and_delete_capture_sequences(&cap_db, 71, 81, false).await;
            switch_fixture_db_to_delete_mode(&git_db).await;
            switch_fixture_db_to_delete_mode(&cap_db).await;

            let git_bytes_before = fs::read(&git_db).expect("snapshot Layout B libra.db");
            let cap_bytes_before = fs::read(&cap_db).expect("snapshot Layout B capture.db");

            let mut guard = quiesce_forward_repository_phase1(&storage)
                .await
                .expect("Layout B quiesce must succeed");
            let report =
                export_forward_phase1_to_redb_tmp(&mut guard, &storage, INITIAL_STORAGE_GENERATION)
                    .await
                    .expect("Layout B export with valid receipt & full PK coverage must succeed");
            guard.close().await.expect("close Layout B guard");

            assert_eq!(report.layout, LegacyRepositoryLayout::LayoutB);
            assert_eq!(
                report.git_table_counts.len(),
                REPOSITORY_ACTIVE_SOURCE_TABLES_COUNT
            );
            assert_eq!(
                report.capture_table_counts.len(),
                CAPTURE_SOURCE_TABLES_COUNT
            );
            assert!(report.max_batch_rows <= FORWARD_MIGRATION_STREAM_BATCH_ROWS);
            assert_eq!(
                report.capture_table_counts["capture_catalog_copy_receipt"],
                1
            );
            assert_eq!(report.capture_table_counts["agent_session"], 2);
            assert_eq!(report.capture_table_counts["agent_session_commit"], 521);

            assert_all_61_repo_tables_in_redb(&git_db, &report.git_tmp_path).await;
            assert_all_capture_tables_in_redb(
                Some(&cap_db),
                &report.capture_tmp_path,
                true,
                521,
                Some("sess-post-copy"),
            )
            .await;
            assert_post_export_sequence_continuity(
                &report.git_tmp_path,
                &report.capture_tmp_path,
                41,
                601,
                51,
                61,
                170,
                180,
            )
            .await;

            assert_eq!(
                fs::read(&git_db).expect("re-read Layout B libra.db"),
                git_bytes_before,
                "Layout B export must leave libra.db byte-for-byte unchanged"
            );
            assert_eq!(
                fs::read(&cap_db).expect("re-read Layout B capture.db"),
                cap_bytes_before,
                "Layout B export must leave capture.db byte-for-byte unchanged"
            );
        }

        // --------------------------------------------------------------------
        // Layout C (AC6): Detached dual DBs (61 active repo tables + retired
        // table ignored, zero inline capture tables + 25 capture.db tables)
        // --------------------------------------------------------------------
        {
            let dir = tempdir().expect("create Layout C repo dir");
            let storage = dir.path().join(".libra");
            let git_db = git_sqlite_db_path(&storage);
            let cap_db = capture_sqlite_db_path(&storage);

            populate_all_61_repository_tables(&git_db).await;
            advance_and_delete_repo_sequences(&git_db, 42, 602, 52, 62, false).await;
            // Drop all 23 inline capture tables and add a retired table row to
            // verify retired tables are cleanly ignored.
            {
                let mut conn = SqliteConnection::connect_with(
                    &SqliteConnectOptions::new()
                        .filename(&git_db)
                        .create_if_missing(false),
                )
                .await
                .expect("open Layout C git_db");
                conn.execute("PRAGMA foreign_keys = OFF;")
                    .await
                    .expect("disable fk");
                for tbl in LEGACY_INLINE_CAPTURE_TABLES {
                    conn.execute(AssertSqlSafe(format!("DROP TABLE IF EXISTS \"{tbl}\"")))
                        .await
                        .expect("drop inline capture table");
                }
                conn.execute(
                    "CREATE TABLE IF NOT EXISTS legacy_operation (op_id TEXT PRIMARY KEY)",
                )
                .await
                .expect("create retired legacy_operation");
                conn.execute("INSERT OR IGNORE INTO legacy_operation (op_id) VALUES ('retired-1')")
                    .await
                    .expect("insert retired row");
                conn.close().await.expect("close Layout C prep conn");
            }
            {
                let cap_store = SqliteCaptureStore::create(&cap_db)
                    .await
                    .expect("create Layout C capture.db");
                use crate::storage::capture_store::CaptureStore;
                cap_store
                    .put_copy_receipt(&CaptureCatalogCopyReceiptRecord {
                        id: 1,
                        copied_at: "2026-11-21T03:00:00Z".to_string(),
                    })
                    .await
                    .expect("write Layout C copy receipt");
            }
            populate_capture_rows_via_sql(&cap_db, 0, None).await;
            advance_and_delete_capture_sequences(&cap_db, 72, 82, false).await;
            switch_fixture_db_to_delete_mode(&git_db).await;
            switch_fixture_db_to_delete_mode(&cap_db).await;

            let git_bytes_before = fs::read(&git_db).expect("snapshot Layout C libra.db");
            let cap_bytes_before = fs::read(&cap_db).expect("snapshot Layout C capture.db");

            let mut guard = quiesce_forward_repository_phase1(&storage)
                .await
                .expect("Layout C quiesce must succeed");
            let report =
                export_forward_phase1_to_redb_tmp(&mut guard, &storage, INITIAL_STORAGE_GENERATION)
                    .await
                    .expect("Layout C export must succeed");
            guard.close().await.expect("close Layout C guard");

            assert_eq!(report.layout, LegacyRepositoryLayout::LayoutC);
            assert_eq!(
                report.git_table_counts.len(),
                REPOSITORY_ACTIVE_SOURCE_TABLES_COUNT
            );
            assert_eq!(
                report.capture_table_counts.len(),
                CAPTURE_SOURCE_TABLES_COUNT
            );
            assert!(report.max_batch_rows <= FORWARD_MIGRATION_STREAM_BATCH_ROWS);
            for (tbl, cnt) in &report.git_table_counts {
                assert!(*cnt >= 1, "Layout C repo table `{tbl}` exported 0 rows");
            }
            for (tbl, cnt) in &report.capture_table_counts {
                assert!(*cnt >= 1, "Layout C capture table `{tbl}` exported 0 rows");
            }

            // Confirm retired table was NOT exported into `git.gen-00000001.redb.tmp`.
            {
                let ro = open_redb_read_only(&report.git_tmp_path, DatabaseRole::Repository)
                    .expect("open git tmp redb for table inventory");
                let rtx = ro.begin_read().expect("begin read tx");
                let names: BTreeSet<String> = rtx
                    .list_tables()
                    .expect("list redb tables")
                    .map(|h| h.name().to_string())
                    .collect();
                assert!(
                    !names.contains("legacy_operation"),
                    "retired table `legacy_operation` must not be exported"
                );
            }

            assert_all_61_repo_tables_in_redb(&git_db, &report.git_tmp_path).await;
            assert_all_capture_tables_in_redb(
                Some(&cap_db),
                &report.capture_tmp_path,
                true,
                1,
                None,
            )
            .await;
            assert_post_export_sequence_continuity(
                &report.git_tmp_path,
                &report.capture_tmp_path,
                42,
                602,
                52,
                62,
                72,
                82,
            )
            .await;

            assert_eq!(
                fs::read(&git_db).expect("re-read Layout C libra.db"),
                git_bytes_before,
                "Layout C export must leave libra.db byte-for-byte unchanged"
            );
            assert_eq!(
                fs::read(&cap_db).expect("re-read Layout C capture.db"),
                cap_bytes_before,
                "Layout C export must leave capture.db byte-for-byte unchanged"
            );
        }

        // --------------------------------------------------------------------
        // Empty-after-delete AUTOINCREMENT sequence continuity + legitimately
        // absent on-demand bridge publication tables
        // --------------------------------------------------------------------
        {
            let dir = tempdir().expect("create empty-seq Layout C dir");
            let storage = dir.path().join(".libra");
            let git_db = git_sqlite_db_path(&storage);
            let cap_db = capture_sqlite_db_path(&storage);

            populate_all_61_repository_tables(&git_db).await;
            advance_and_delete_repo_sequences(&git_db, 91, 700, 93, 94, true).await;
            {
                let mut conn = SqliteConnection::connect_with(
                    &SqliteConnectOptions::new()
                        .filename(&git_db)
                        .create_if_missing(false),
                )
                .await
                .expect("open git_db for empty-seq prep");
                conn.execute("PRAGMA foreign_keys = OFF;")
                    .await
                    .expect("disable fk");
                for tbl in LEGACY_INLINE_CAPTURE_TABLES {
                    conn.execute(AssertSqlSafe(format!("DROP TABLE IF EXISTS \"{tbl}\"")))
                        .await
                        .expect("drop inline capture");
                }
                // Drop the 2 explicitly on-demand bridge publication tables to
                // confirm legitimately absent on-demand tables export as 0 rows.
                for tbl in ON_DEMAND_REPOSITORY_SOURCE_TABLES {
                    conn.execute(AssertSqlSafe(format!("DROP TABLE IF EXISTS \"{tbl}\"")))
                        .await
                        .expect("drop on-demand bridge table");
                }
                conn.close().await.expect("close prep conn");
            }
            {
                let _cap_store = SqliteCaptureStore::create(&cap_db)
                    .await
                    .expect("create capture.db");
            }
            populate_capture_rows_via_sql(&cap_db, 0, None).await;
            advance_and_delete_capture_sequences(&cap_db, 95, 96, true).await;
            switch_fixture_db_to_delete_mode(&git_db).await;
            switch_fixture_db_to_delete_mode(&cap_db).await;

            let mut guard = quiesce_forward_repository_phase1(&storage)
                .await
                .expect("quiesce empty-seq repo");
            let report =
                export_forward_phase1_to_redb_tmp(&mut guard, &storage, INITIAL_STORAGE_GENERATION)
                    .await
                    .expect("export empty-seq repo with absent on-demand tables must succeed");
            guard.close().await.expect("close empty-seq guard");

            assert_eq!(report.git_table_counts["config"], 0);
            assert_eq!(report.git_table_counts["config_kv"], 0);
            assert_eq!(report.git_table_counts["reflog"], 0);
            assert_eq!(report.git_table_counts["worktree_intent_journal"], 0);
            assert_eq!(report.git_table_counts["agent_bridge_publication"], 0);
            assert_eq!(report.git_table_counts["agent_bridge_index_publication"], 0);
            assert_eq!(report.capture_table_counts["agent_bridge_event"], 0);
            assert_eq!(report.capture_table_counts["agent_bridge_link"], 0);
            assert_post_export_sequence_continuity(
                &report.git_tmp_path,
                &report.capture_tmp_path,
                91,
                700,
                93,
                94,
                95,
                96,
            )
            .await;
        }

        // --------------------------------------------------------------------
        // Fail-closed required source-table inventory checks (Layout A & C)
        // --------------------------------------------------------------------
        for (case_label, is_layout_c, target_is_capture, drop_table) in [
            (
                "layout_a_missing_schema_versions",
                false,
                false,
                "schema_versions",
            ),
            (
                "layout_a_missing_inline_capture",
                false,
                false,
                "agent_audit_log",
            ),
            ("layout_c_missing_repo_reference", true, false, "reference"),
            (
                "layout_c_missing_capture_session",
                true,
                true,
                "agent_session",
            ),
        ] {
            let dir = tempdir().expect("create missing-table tempdir");
            let storage = dir.path().join(".libra");
            let git_db = git_sqlite_db_path(&storage);
            let cap_db = capture_sqlite_db_path(&storage);
            populate_all_61_repository_tables(&git_db).await;
            if is_layout_c {
                let mut conn = SqliteConnection::connect_with(
                    &SqliteConnectOptions::new()
                        .filename(&git_db)
                        .create_if_missing(false),
                )
                .await
                .expect("open git_db");
                conn.execute("PRAGMA foreign_keys = OFF;")
                    .await
                    .expect("fk off");
                for tbl in LEGACY_INLINE_CAPTURE_TABLES {
                    conn.execute(AssertSqlSafe(format!("DROP TABLE IF EXISTS \"{tbl}\"")))
                        .await
                        .expect("drop inline");
                }
                conn.close().await.expect("close git_db");
                let _cap = SqliteCaptureStore::create(&cap_db)
                    .await
                    .expect("create cap_db");
            }
            let drop_target = if target_is_capture { &cap_db } else { &git_db };
            {
                let mut conn = SqliteConnection::connect_with(
                    &SqliteConnectOptions::new()
                        .filename(drop_target)
                        .create_if_missing(false),
                )
                .await
                .expect("open drop target");
                conn.execute("PRAGMA foreign_keys = OFF;")
                    .await
                    .expect("fk off");
                conn.execute(AssertSqlSafe(format!("DROP TABLE \"{drop_table}\"")))
                    .await
                    .expect("drop required table");
                conn.close().await.expect("close drop target");
            }
            switch_fixture_db_to_delete_mode(&git_db).await;
            if is_layout_c {
                switch_fixture_db_to_delete_mode(&cap_db).await;
            }
            let git_before = fs::read(&git_db).expect("snapshot git_db");
            let cap_before = if is_layout_c {
                Some(fs::read(&cap_db).expect("snapshot cap_db"))
            } else {
                None
            };

            let mut guard = quiesce_forward_repository_phase1(&storage)
                .await
                .expect("quiesce missing-table fixture");
            let err =
                export_forward_phase1_to_redb_tmp(&mut guard, &storage, INITIAL_STORAGE_GENERATION)
                    .await
                    .expect_err(&format!(
                        "missing required table case `{case_label}` must fail closed"
                    ));
            guard.close().await.expect("close missing-table guard");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert!(!git_redb_tmp_path(&storage, 1).exists());
            assert!(!capture_redb_tmp_path(&storage, 1).exists());
            assert_eq!(fs::read(&git_db).expect("re-read git_db"), git_before);
            if let Some(cap_before) = cap_before {
                assert_eq!(fs::read(&cap_db).expect("re-read cap_db"), cap_before);
            }
        }

        // --------------------------------------------------------------------
        // Unix staging directory permissions & pathname replacement safety
        // --------------------------------------------------------------------
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            // Group-writable and world-writable `redb/` dir, `private/` dir,
            // and `.libra` root must fail closed with zero `.tmp` artifacts.
            for (case_label, sub_opt, bad_mode) in [
                ("group_writable_redb", Some(REDB_SUBDIR), 0o775),
                ("world_writable_redb", Some(REDB_SUBDIR), 0o777),
                ("group_writable_private", Some(PRIVATE_SUBDIR), 0o770),
                ("world_writable_private", Some(PRIVATE_SUBDIR), 0o777),
                ("group_writable_storage_root", None, 0o775),
            ] {
                let dir = tempdir().expect("create perm test tempdir");
                let storage = dir.path().join(".libra");
                let git_db = git_sqlite_db_path(&storage);
                populate_all_61_repository_tables(&git_db).await;
                switch_fixture_db_to_delete_mode(&git_db).await;
                let target_dir = match sub_opt {
                    Some(sub) => {
                        let d = storage.join(sub);
                        fs::create_dir_all(&d).expect("create staging subdir");
                        d
                    }
                    None => storage.clone(),
                };
                fs::set_permissions(&target_dir, fs::Permissions::from_mode(bad_mode))
                    .expect("chmod bad mode");

                let mut guard = quiesce_forward_repository_phase1(&storage)
                    .await
                    .expect("quiesce perm fixture");
                let err = export_forward_phase1_to_redb_tmp(
                    &mut guard,
                    &storage,
                    INITIAL_STORAGE_GENERATION,
                )
                .await
                .expect_err(&format!(
                    "unsafe directory permission case `{case_label}` must fail closed"
                ));
                guard.close().await.expect("close perm guard");
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                assert!(!git_redb_tmp_path(&storage, 1).exists());
                assert!(!capture_redb_tmp_path(&storage, 1).exists());
                // Restore sane permissions for clean tempdir cleanup.
                let _ = fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o700));
            }

            // External pathname replacement must be rejected by
            // `verify_still_bound` and `Drop` must NEVER unlink the foreign inode.
            {
                let dir = tempdir().expect("create replacement test tempdir");
                let storage = dir.path().join(".libra");
                fs::create_dir_all(&storage).expect("create .libra");
                fs::set_permissions(&storage, fs::Permissions::from_mode(0o755))
                    .expect("chmod .libra 0755");
                let tmp_name = "git.gen-00000001.redb.tmp";
                let (db, guard) = create_redb_tmp_database(
                    &storage,
                    REDB_SUBDIR,
                    tmp_name,
                    0o755,
                    0o644,
                    false,
                    DatabaseRole::Repository,
                )
                .expect("create staged tmp");
                drop(db);

                let staged_path = storage.join(REDB_SUBDIR).join(tmp_name);
                let foreign_tmp = storage.join(REDB_SUBDIR).join("foreign.replacement");
                fs::write(&foreign_tmp, b"FOREIGN_PAYLOAD").expect("write foreign file");
                fs::rename(&foreign_tmp, &staged_path).expect("replace staged pathname");

                let err = guard
                    .verify_still_bound(&staged_path)
                    .expect_err("replaced pathname must fail verify_still_bound");
                assert_eq!(err.stable_code_str(), "LBR-IO-001");

                // Drop guard while still armed — foreign inode MUST remain intact!
                drop(guard);
                assert_eq!(
                    fs::read(&staged_path).expect("foreign file must NOT be unlinked by guard"),
                    b"FOREIGN_PAYLOAD"
                );
            }
        }

        #[cfg(windows)]
        {
            // External pathname replacement on Windows must be rejected by
            // `verify_still_bound` (by volume+file index) and `Drop` must NEVER
            // delete the foreign replacement file.
            let dir = tempdir().expect("create windows replacement test tempdir");
            let storage = dir.path().join(".libra");
            fs::create_dir_all(&storage).expect("create .libra");
            let tmp_name = "git.gen-00000001.redb.tmp";
            let (db, guard) = create_redb_tmp_database(
                &storage,
                REDB_SUBDIR,
                tmp_name,
                0o755,
                0o644,
                false,
                DatabaseRole::Repository,
            )
            .expect("create staged tmp");
            drop(db);

            let staged_path = storage.join(REDB_SUBDIR).join(tmp_name);
            fs::remove_file(&staged_path).expect("remove original staged tmp");
            fs::write(&staged_path, b"FOREIGN_PAYLOAD_WIN").expect("write foreign replacement");

            let err = guard
                .verify_still_bound(&staged_path)
                .expect_err("replaced pathname must fail verify_still_bound on Windows");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");

            drop(guard);
            assert_eq!(
                fs::read(&staged_path).expect("foreign file must NOT be deleted by Windows guard"),
                b"FOREIGN_PAYLOAD_WIN"
            );
        }
    }

    #[tokio::test]
    async fn forward_phase1_layout_b_incomplete_receipt_reject() {
        use crate::storage::{
            SqliteCaptureStore, capture_store::CaptureStore, manifest::INITIAL_STORAGE_GENERATION,
        };

        async fn build_base_layout_b(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
            let storage = root.join(".libra");
            let git_db = git_sqlite_db_path(&storage);
            let cap_db = capture_sqlite_db_path(&storage);
            populate_all_61_repository_tables(&git_db).await;
            populate_capture_rows_via_sql(&git_db, 0, None).await;
            {
                let cap_store = SqliteCaptureStore::create(&cap_db)
                    .await
                    .expect("create Layout B reject fixture capture.db");
                cap_store
                    .put_copy_receipt(&CaptureCatalogCopyReceiptRecord {
                        id: 1,
                        copied_at: "2026-11-21T04:00:00Z".to_string(),
                    })
                    .await
                    .expect("seed valid copy receipt");
            }
            populate_capture_rows_via_sql(&cap_db, 0, None).await;
            (storage, git_db, cap_db)
        }

        async fn assert_rejected_cleanly(
            storage: &Path,
            git_db: &Path,
            cap_db: &Path,
            label: &str,
        ) {
            switch_fixture_db_to_delete_mode(git_db).await;
            switch_fixture_db_to_delete_mode(cap_db).await;
            let git_before = fs::read(git_db).expect("snapshot git_db");
            let cap_before = fs::read(cap_db).expect("snapshot cap_db");

            let mut guard = quiesce_forward_repository_phase1(storage)
                .await
                .expect("quiesce Layout B reject fixture");
            let err =
                export_forward_phase1_to_redb_tmp(&mut guard, storage, INITIAL_STORAGE_GENERATION)
                    .await
                    .expect_err(&format!("Layout B case `{label}` must fail closed"));
            guard.close().await.expect("close reject guard");

            assert_eq!(
                err.stable_code_str(),
                "LBR-IO-001",
                "Layout B case `{label}` must report LBR-IO-001, got {err:?}"
            );
            let err_text = err.to_string();
            assert!(
                !err_text.contains(storage.to_string_lossy().as_ref()),
                "Layout B case `{label}` leaked raw storage path: {err_text}"
            );
            assert!(
                !git_redb_tmp_path(storage, 1).exists(),
                "Layout B case `{label}` must leave zero git.tmp artifact"
            );
            assert!(
                !capture_redb_tmp_path(storage, 1).exists(),
                "Layout B case `{label}` must leave zero capture.tmp artifact"
            );
            assert_eq!(
                fs::read(git_db).expect("re-read git_db"),
                git_before,
                "Layout B case `{label}` must not mutate libra.db"
            );
            assert_eq!(
                fs::read(cap_db).expect("re-read cap_db"),
                cap_before,
                "Layout B case `{label}` must not mutate capture.db"
            );
        }

        // Case 1: `capture_catalog_copy_receipt` table dropped
        {
            let dir = tempdir().expect("case 1 tempdir");
            let (storage, git_db, cap_db) = build_base_layout_b(dir.path()).await;
            let mut conn =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&cap_db))
                    .await
                    .expect("open cap_db");
            conn.execute("DROP TABLE capture_catalog_copy_receipt")
                .await
                .expect("drop receipt table");
            conn.close().await.expect("close cap_db");
            assert_rejected_cleanly(&storage, &git_db, &cap_db, "missing_receipt_table").await;
        }

        // Case 2: `capture_catalog_copy_receipt` table empty (0 rows)
        {
            let dir = tempdir().expect("case 2 tempdir");
            let (storage, git_db, cap_db) = build_base_layout_b(dir.path()).await;
            let mut conn =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&cap_db))
                    .await
                    .expect("open cap_db");
            conn.execute("DELETE FROM capture_catalog_copy_receipt")
                .await
                .expect("delete receipt row");
            conn.close().await.expect("close cap_db");
            assert_rejected_cleanly(&storage, &git_db, &cap_db, "empty_receipt_table").await;
        }

        // Case 3: `capture_catalog_copy_receipt` row id=1 has whitespace/empty `copied_at`
        {
            let dir = tempdir().expect("case 3 tempdir");
            let (storage, git_db, cap_db) = build_base_layout_b(dir.path()).await;
            let mut conn =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&cap_db))
                    .await
                    .expect("open cap_db");
            conn.execute("UPDATE capture_catalog_copy_receipt SET copied_at = '   ' WHERE id = 1")
                .await
                .expect("blank copied_at");
            conn.close().await.expect("close cap_db");
            assert_rejected_cleanly(&storage, &git_db, &cap_db, "blank_copied_at").await;
        }

        // Case 4: `capture_catalog_copy_receipt` only has wrong id (id = 2)
        {
            let dir = tempdir().expect("case 4 tempdir");
            let (storage, git_db, cap_db) = build_base_layout_b(dir.path()).await;
            let mut conn =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&cap_db))
                    .await
                    .expect("open cap_db");
            conn.execute("DROP TABLE capture_catalog_copy_receipt")
                .await
                .expect("drop receipt");
            conn.execute(
                "CREATE TABLE capture_catalog_copy_receipt (id INTEGER PRIMARY KEY, copied_at TEXT NOT NULL)",
            )
            .await
            .expect("recreate receipt without CHECK");
            conn.execute(
                "INSERT INTO capture_catalog_copy_receipt (id, copied_at) VALUES (2, '2026-11-21T04:00:00Z')",
            )
            .await
            .expect("insert wrong id=2");
            conn.close().await.expect("close cap_db");
            assert_rejected_cleanly(&storage, &git_db, &cap_db, "wrong_receipt_id").await;
        }

        // Case 5: Valid receipt, but an inline table (`agent_audit_log`) missing from `capture.db`
        {
            let dir = tempdir().expect("case 5 tempdir");
            let (storage, git_db, cap_db) = build_base_layout_b(dir.path()).await;
            let mut conn =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&cap_db))
                    .await
                    .expect("open cap_db");
            conn.execute("DROP TABLE agent_audit_log")
                .await
                .expect("drop agent_audit_log in capture.db");
            conn.close().await.expect("close cap_db");
            assert_rejected_cleanly(&storage, &git_db, &cap_db, "missing_capture_table").await;
        }

        // Case 6: Valid receipt & tables, but missing primary key in standard PK table (`agent_audit_log`)
        {
            let dir = tempdir().expect("case 6 tempdir");
            let (storage, git_db, cap_db) = build_base_layout_b(dir.path()).await;
            let mut conn =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&git_db))
                    .await
                    .expect("open git_db");
            conn.execute(
                "INSERT INTO agent_audit_log (audit_id, timestamp, user_id, user_name, action, checkpoint_id, scope, export_path, justification, granted) VALUES ('aud-uncopied', '2026-11-21T04:10:00Z', 'u1', 'Ada', 'export', 'cp-1', 'committed', NULL, 'missing-in-cap', 1)",
            )
            .await
            .expect("insert uncopied row into libra.db");
            conn.close().await.expect("close git_db");
            assert_rejected_cleanly(&storage, &git_db, &cap_db, "missing_primary_key").await;
        }

        // Case 7: Valid receipt & tables, but missing unique key in PK-less table (`agent_coverage_claim`)
        {
            let dir = tempdir().expect("case 7 tempdir");
            let (storage, git_db, cap_db) = build_base_layout_b(dir.path()).await;
            let mut conn =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&cap_db))
                    .await
                    .expect("open cap_db");
            conn.execute(
                "DELETE FROM agent_coverage_claim WHERE session_id = 'sess-1' AND logical_turn_key = 'turn-1'",
            )
            .await
            .expect("delete coverage claim row from capture.db");
            conn.close().await.expect("close cap_db");
            assert_rejected_cleanly(
                &storage,
                &git_db,
                &cap_db,
                "missing_unique_key_coverage_claim",
            )
            .await;
        }
    }
}
