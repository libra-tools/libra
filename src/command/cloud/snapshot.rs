//! Frozen cloud-sync snapshot spool (plan-20260911 PI-09).
//!
//! Before its first remote write, `cloud sync` freezes the object identities,
//! the refs projection and the project name it is about to upload into a
//! private, per-run spool under `.libra/cloud-sync/snapshots/<run>`. Every
//! read is its own short statement on one dedicated read connection, and an
//! unchanged `PRAGMA data_version` around them proves they saw one database
//! state (otherwise the freeze is retried), so the spool is one consistent
//! snapshot without holding the database lock for the whole scan. Upload, metadata and the `is_synced` update read only that
//! spool. Pages bound memory without capping how many objects a repository
//! may hold.
//!
//! The run directory is created exclusively with owner-only modes and holds
//! an owner marker from the start, so a later cleanup can tell whose run a
//! leftover was. The `cloud-sync` / `snapshots` parents must be real
//! directories inside `.libra`, owned by the current user and not writable
//! by group or others (a symlink is never followed), so no other user can
//! swap a path under the spool. The spool is removed when the
//! [`SnapshotSpool`] is released or dropped, so success and failure both
//! clean up.

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use sea_orm::{QueryOrder, QuerySelect};
use serde::Deserialize;

use super::{
    metadata::{
        DigestWriter, ReferenceProjectionKey, ReferenceProjectionWriter, copy_reference_page,
        metadata_hash_of_file, prepare_reference_copy, reference_projection_page,
    },
    *,
};

/// Directory under `.libra` that holds cloud-sync working state.
pub(super) const SPOOL_ROOT_DIR: &str = "cloud-sync";
/// Directory under [`SPOOL_ROOT_DIR`] holding one directory per sync run.
pub(super) const SPOOL_SNAPSHOTS_DIR: &str = "snapshots";
const OBJECTS_FILE: &str = "objects.jsonl";
const REFS_FILE: &str = "refs.json";
const REPOSITORY_FILE: &str = "repository.json";
/// The agent-capture catalog frozen with the snapshot (PI-26).
const CATALOG_FILE: &str = "catalog.json";
/// Owner marker written into every run directory when it is created.
pub(super) const OWNER_MARKER_FILE: &str = "owner.json";
/// The files a run directory can hold, each with what it contains; notes
/// about a kept run directory name the ones actually present (PI-26).
const RUN_FILES: [(&str, &str); 5] = [
    (OWNER_MARKER_FILE, "owner marker: pid, host, boot id"),
    (
        OBJECTS_FILE,
        "object identities: id, type, size, repository id, timestamp",
    ),
    (REFS_FILE, "refs"),
    (REPOSITORY_FILE, "project name"),
    (
        CATALOG_FILE,
        "agent-capture catalog: sessions, checkpoints, prune and import tombstones, the traces head, subagent claims, revisions and links, legacy Claude mirror rows",
    ),
];
/// `tool` value of [`OwnerMarker`].
pub(super) const OWNER_MARKER_TOOL: &str = "libra-cloud-sync";
/// Rows read from SQLite, and identities read back from the spool, per page.
pub(super) const DEFAULT_SPOOL_PAGE_SIZE: usize = 1024;

/// Bytes of the frozen refs JSON hashed or uploaded at a time (PI-27); also
/// the multipart part size, which S3-compatible stores require to be at
/// least 5 MiB.
pub(super) const DEFAULT_METADATA_CHUNK_SIZE: usize = 8 * 1024 * 1024;

/// Per-run knobs. Production uses `Default`; tests shrink the page size and
/// pause between the freeze and the first remote write.
#[derive(Default)]
pub(super) struct SnapshotTuning {
    pub(super) page_size: Option<usize>,
    #[cfg(test)]
    pub(super) after_freeze: Option<AfterFreezeHook>,
    /// Fail the first spool write as if the disk were full.
    #[cfg(test)]
    pub(super) fail_spool_write: bool,
    /// Fail every local `is_synced` update.
    #[cfg(test)]
    pub(super) fail_status_update: bool,
    /// Fail the removal of this run's spool.
    #[cfg(test)]
    pub(super) fail_spool_release: bool,
    /// Fail removing a leftover run directory whose owner exited.
    #[cfg(test)]
    pub(super) fail_residue_removal: bool,
    /// Fail listing the leftover run directories part-way.
    #[cfg(test)]
    pub(super) fail_residue_listing: bool,
    /// Fail writing the owner marker.
    #[cfg(test)]
    pub(super) fail_marker_write: bool,
    /// Fail reading `cloud.name` inside the freeze.
    #[cfg(test)]
    pub(super) fail_config_read: bool,
    /// Use this run directory name instead of a random one.
    #[cfg(test)]
    pub(super) run_name: Option<String>,
    /// Run after each object page of the freeze.
    #[cfg(test)]
    pub(super) between_pages: Option<BetweenPagesHook>,
    /// Run after each ref page the freeze copies from the repository.
    #[cfg(test)]
    pub(super) between_ref_pages: Option<BetweenPagesHook>,
    /// Run after each refs projection page the freeze writes to the spool.
    #[cfg(test)]
    pub(super) between_projection_pages: Option<BetweenPagesHook>,
    /// Make the first freeze attempt end on what looks like a replaced
    /// connection.
    #[cfg(test)]
    pub(super) simulate_reconnect: bool,
    /// Make the first freeze attempt lose its TEMP objects after the first
    /// copied ref page, as a replaced connection would.
    #[cfg(test)]
    pub(super) simulate_reconnect_during_refs: bool,
    /// Make the first freeze attempt lose its TEMP objects while it creates
    /// the ref copy, as a replaced connection would.
    #[cfg(test)]
    pub(super) simulate_reconnect_during_ref_setup: bool,
    /// Run after each agent-capture catalog page the freeze reads.
    #[cfg(test)]
    pub(super) between_catalog_pages: Option<BetweenPagesHook>,
    /// Make the first freeze attempt lose its TEMP marker while marking the
    /// connection, as a replaced connection would.
    #[cfg(test)]
    pub(super) simulate_reconnect_during_mark: bool,
    /// Fail writing the connection marker with an I/O error.
    #[cfg(test)]
    pub(super) fail_mark_write: bool,
    /// Fail recording `cloud.metadata_hash` after the metadata upload.
    #[cfg(test)]
    pub(super) fail_metadata_hash_record: bool,
    /// Hash and upload the refs JSON this many bytes at a time.
    #[cfg(test)]
    pub(super) metadata_chunk_size: Option<usize>,
    /// Run between hashing the frozen refs and uploading them.
    #[cfg(test)]
    pub(super) before_metadata_upload: Option<AfterFreezeHook>,
}

/// Called with the freeze attempt number (1-based) after each page.
#[cfg(test)]
pub(super) type BetweenPagesHook =
    std::sync::Arc<dyn Fn(usize) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

#[cfg(test)]
pub(super) type AfterFreezeHook =
    std::sync::Arc<dyn Fn(PathBuf) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

impl SnapshotTuning {
    pub(super) fn page_size(&self) -> usize {
        self.page_size.unwrap_or(DEFAULT_SPOOL_PAGE_SIZE).max(1)
    }

    pub(super) fn metadata_chunk_size(&self) -> usize {
        #[cfg(test)]
        if let Some(size) = self.metadata_chunk_size {
            return size.max(1);
        }
        DEFAULT_METADATA_CHUNK_SIZE
    }
}

/// Freeze attempts before a database that keeps changing stops the run.
pub(super) const MAX_FREEZE_ATTEMPTS: usize = 3;

/// `PRAGMA data_version`: changes on this connection exactly when another
/// connection commits to the database.
async fn data_version(conn: &sea_orm::DatabaseConnection) -> Result<i64, sea_orm::DbErr> {
    let row = conn
        .query_one_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA data_version",
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("PRAGMA data_version returned no row".into()))?;
    row.try_get::<i64>("", "data_version")
}

/// `total_changes()`: rows changed through this connection itself since it
/// opened, which `PRAGMA data_version` does not reflect.
async fn total_changes(conn: &sea_orm::DatabaseConnection) -> Result<i64, sea_orm::DbErr> {
    let row = conn
        .query_one_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "SELECT total_changes() AS n",
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("total_changes() returned no row".into()))?;
    row.try_get::<i64>("", "n")
}

/// Run `read` on `conn` in one unchanged-database window (PI-26): the TEMP
/// connection marker, `PRAGMA data_version` (commits through other
/// connections) and `total_changes()` (writes through this shared connection,
/// such as another task of this process) are checked around it, and the read
/// is taken again, up to [`MAX_FREEZE_ATTEMPTS`] times, when anything changed
/// meanwhile or the connection was replaced. `None` when every attempt saw a
/// change. No lock is held between the statements `read` issues.
pub(super) async fn read_in_one_window<T, F, Fut>(
    conn: &sea_orm::DatabaseConnection,
    mut read: F,
) -> CloudResult<Option<T>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = CloudResult<T>>,
{
    let window_failure = |error: sea_orm::DbErr| {
        super::agent_capture::local_agent_capture_failure(
            "check the local catalog read window",
            error,
        )
    };
    for attempt in 1..=MAX_FREEZE_ATTEMPTS {
        mark_freeze_connection(conn, attempt)
            .await
            .map_err(window_failure)?;
        let start = (
            data_version(conn).await.map_err(window_failure)?,
            total_changes(conn).await.map_err(window_failure)?,
        );
        let value = read().await?;
        let end = (
            data_version(conn).await.map_err(window_failure)?,
            total_changes(conn).await.map_err(window_failure)?,
        );
        if end == start
            && same_freeze_connection(conn, attempt)
                .await
                .map_err(window_failure)?
        {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// Record `attempt` in a TEMP table private to this physical connection.
async fn mark_freeze_connection(
    conn: &sea_orm::DatabaseConnection,
    attempt: usize,
) -> Result<(), sea_orm::DbErr> {
    conn.execute_unprepared(
        "CREATE TEMP TABLE IF NOT EXISTS libra_cloud_sync_freeze (attempt INTEGER NOT NULL)",
    )
    .await?;
    conn.execute_unprepared("DELETE FROM temp.libra_cloud_sync_freeze")
        .await?;
    conn.execute_unprepared(&format!(
        "INSERT INTO temp.libra_cloud_sync_freeze (attempt) VALUES ({attempt})"
    ))
    .await?;
    Ok(())
}

/// A statement on the connection's TEMP objects (the marker or the ref copy)
/// failed. Only a statement that found its TEMP table missing on a
/// connection without this attempt's marker is a replaced connection: a
/// change like any other, so the attempt is retried (`Ok`). Any other failure
/// (for example temporary storage that is full) stops the freeze with the
/// original error.
async fn replaced_connection(
    conn: &sea_orm::DatabaseConnection,
    attempt: usize,
    error: sea_orm::DbErr,
) -> Result<(), sea_orm::DbErr> {
    if !error.to_string().contains("no such table") {
        return Err(error);
    }
    match same_freeze_connection(conn, attempt).await {
        Ok(false) => Ok(()),
        _ => Err(error),
    }
}

/// Whether the connection now answering is the one [`mark_freeze_connection`]
/// marked for `attempt` (a replacement has no such TEMP table).
async fn same_freeze_connection(
    conn: &sea_orm::DatabaseConnection,
    attempt: usize,
) -> Result<bool, sea_orm::DbErr> {
    let row = match conn
        .query_one_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "SELECT attempt FROM temp.libra_cloud_sync_freeze",
        ))
        .await
    {
        Ok(row) => row,
        Err(error) if error.to_string().contains("no such table") => return Ok(false),
        Err(error) => return Err(error),
    };
    match row {
        Some(row) => Ok(row.try_get::<i64>("", "attempt")? == attempt as i64),
        None => Ok(false),
    }
}

/// Replace a spool file left by an earlier freeze attempt of this run.
fn recreate_private_file(path: &Path) -> std::io::Result<File> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    create_private_file(path)
}

/// One frozen `object_index` identity, as written to the spool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FrozenObject {
    pub(super) id: i64,
    pub(super) o_id: String,
    pub(super) o_type: String,
    pub(super) o_size: i64,
    pub(super) repo_id: String,
    pub(super) created_at: i64,
}

impl From<object_index::Model> for FrozenObject {
    fn from(model: object_index::Model) -> Self {
        Self {
            id: model.id,
            o_id: model.o_id,
            o_type: model.o_type,
            o_size: model.o_size,
            repo_id: model.repo_id,
            created_at: model.created_at,
        }
    }
}

/// Who created a run directory. Written once, when the directory is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct OwnerMarker {
    pub(super) tool: String,
    pub(super) pid: u32,
    /// The PID namespace `pid` belongs to (`/proc/self/ns/pid` on Linux;
    /// `None` where the platform has no PID namespaces).
    pub(super) pid_namespace: Option<String>,
    /// Identity of the current boot of this host (Linux
    /// `/proc/sys/kernel/random/boot_id`, macOS `kern.bootsessionuuid`);
    /// `None` when it cannot be read.
    pub(super) boot_id: Option<String>,
    pub(super) host: String,
    pub(super) started_at_unix_ms: i64,
    pub(super) libra_version: String,
}

impl OwnerMarker {
    fn for_this_process() -> Self {
        Self {
            tool: OWNER_MARKER_TOOL.to_string(),
            pid: std::process::id(),
            pid_namespace: pid_namespace(),
            boot_id: boot_id(),
            host: host_name(),
            started_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            libra_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// This process's PID namespace identity, e.g. `pid:[4026531836]`.
pub(super) fn pid_namespace() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        fs::read_link("/proc/self/ns/pid")
            .ok()
            .map(|link| link.to_string_lossy().into_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// This host's current boot identity.
pub(super) fn boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
    }
    #[cfg(target_os = "macos")]
    {
        let mut buffer = [0u8; 64];
        let mut len = buffer.len();
        // SAFETY: the name is NUL-terminated; `buffer` is valid for `len`
        // bytes and sysctlbyname writes at most that many.
        let status = unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                buffer.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return None;
        }
        let bytes = &buffer[..len.min(buffer.len())];
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        let id = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
        (!id.is_empty()).then_some(id)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(unix)]
pub(super) fn host_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for `buffer.len()` bytes; gethostname
    // writes at most that many bytes.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return String::new();
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// The computer's NetBIOS name from the operating system, never from the
/// `COMPUTERNAME` environment variable (PI-25).
#[cfg(windows)]
pub(super) fn host_name() -> String {
    use windows_sys::Win32::System::SystemInformation::{ComputerNameNetBIOS, GetComputerNameExW};

    let mut size = 0u32;
    // SAFETY: a null buffer with size 0 only asks for the size required,
    // which the call writes to `size`.
    unsafe { GetComputerNameExW(ComputerNameNetBIOS, std::ptr::null_mut(), &mut size) };
    if size == 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; size as usize];
    // SAFETY: `buffer` holds `size` UTF-16 units; on success the call writes
    // the name and stores its length (without the terminator) in `size`.
    let written =
        unsafe { GetComputerNameExW(ComputerNameNetBIOS, buffer.as_mut_ptr(), &mut size) };
    if written == 0 {
        return String::new();
    }
    buffer.truncate(size as usize);
    String::from_utf16_lossy(&buffer)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn host_name() -> String {
    String::new()
}

/// Repository metadata frozen with the snapshot: the project name to
/// register, i.e. the configured `cloud.name` or, when unset, the working
/// directory's name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FrozenRepository {
    pub(super) project_name: String,
}

/// What a freeze page holds; test builds count each kind separately.
#[derive(Debug, Clone, Copy)]
pub(super) enum PageKind {
    Objects = 0,
    Refs = 1,
}

/// One page of rows read for the freeze: the only rows of its kind it holds
/// in memory at a time. Test builds count the pages alive at once.
struct FreezePage<T> {
    rows: Vec<T>,
    #[cfg(test)]
    kind: PageKind,
}

#[cfg(test)]
static LIVE_PAGES: [std::sync::atomic::AtomicUsize; 2] = [
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
];
#[cfg(test)]
static PEAK_PAGES: [std::sync::atomic::AtomicUsize; 2] = [
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
];

impl<T> FreezePage<T> {
    fn new(rows: Vec<T>, kind: PageKind) -> Self {
        #[cfg(test)]
        {
            use std::sync::atomic::Ordering;
            let live = LIVE_PAGES[kind as usize].fetch_add(1, Ordering::SeqCst) + 1;
            PEAK_PAGES[kind as usize].fetch_max(live, Ordering::SeqCst);
            Self { rows, kind }
        }
        #[cfg(not(test))]
        {
            let _ = kind;
            Self { rows }
        }
    }
}

#[cfg(test)]
impl<T> Drop for FreezePage<T> {
    fn drop(&mut self) {
        LIVE_PAGES[self.kind as usize].fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The most `kind` pages alive at once since the last call; resets the count.
#[cfg(test)]
pub(super) fn take_peak_pages(kind: PageKind) -> usize {
    PEAK_PAGES[kind as usize].swap(0, std::sync::atomic::Ordering::SeqCst)
}

/// The frozen refs projection: the spool file uploaded as repository
/// metadata, its `cloud.metadata_hash` and how many refs it holds.
pub(super) struct FrozenRefs {
    pub(super) references: usize,
    pub(super) path: PathBuf,
    pub(super) len: u64,
    pub(super) hash: u64,
    pub(super) upload_chunk_size: usize,
    /// Where a background cleanup failure of the upload is reported.
    pub(super) report: crate::utils::storage::remote::AbandonedUploadReport,
    #[cfg(test)]
    pub(super) before_upload: Option<AfterFreezeHook>,
    #[cfg(test)]
    pub(super) fail_hash_record: bool,
}

/// Where a spool dropped without [`SnapshotSpool::finish`] reports a
/// directory it could not remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpoolWarningSink {
    /// `warning:` text on stderr in every mode, like
    /// `emit_post_envelope_warning`: a panicking or cancelled run renders no
    /// envelope that could carry it.
    Stderr,
    /// One `cloud_sync.spool.warning` NDJSON line on the `--progress=json`
    /// stream.
    NdjsonProgress,
}

/// A private per-run spool. Dropping it removes the run directory.
pub(super) struct SnapshotSpool {
    dir: PathBuf,
    warning_sink: SpoolWarningSink,
    objects: u64,
    references: usize,
    refs_bytes: u64,
    refs_digest: u64,
    catalog_bytes: u64,
    catalog_digest: u64,
    released: bool,
    #[cfg(test)]
    fail_release: bool,
}

fn spool_failure(action: &str, path: &Path, error: impl fmt::Display) -> CloudError {
    CloudError::Generic(format!(
        "cloud sync could not {action} its local snapshot spool at {}: {error}; nothing was written to D1 or R2. Check free disk space and that the directory is a plain directory inside .libra, then retry",
        path.display()
    ))
}

/// The repository storage directory (`.libra`) anchors the spool: it must
/// be a real directory (a symlink is not followed) owned by the current user
/// and not writable by group or others (Windows: a protected DACL granting
/// only the current user and SYSTEM), so no other user can swap the
/// `cloud-sync` subtree between the checks below and the spool writes.
fn ensure_trusted_root(storage: &Path) -> CloudResult<()> {
    let meta =
        fs::symlink_metadata(storage).map_err(|error| spool_failure("inspect", storage, error))?;
    if meta.file_type().is_symlink() {
        return Err(spool_failure(
            "use",
            storage,
            "the repository storage directory is a symbolic link and is not followed",
        ));
    }
    if !meta.is_dir() {
        return Err(spool_failure(
            "use",
            storage,
            "the repository storage path is not a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err(spool_failure(
                "use",
                storage,
                format!(
                    "the repository storage directory is owned by uid {} but cloud sync runs as uid {euid}",
                    meta.uid()
                ),
            ));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(spool_failure(
                "use",
                storage,
                "the repository storage directory is writable by group or others; run `chmod go-w` on it",
            ));
        }
    }
    #[cfg(windows)]
    {
        // Windows: `.libra` must already carry a protected DACL granting only
        // the current user and SYSTEM (no reparse point); otherwise refuse.
        crate::utils::windows_fs::open_private_directory(storage).map_err(|error| {
            spool_failure(
                "use",
                storage,
                format!(
                    "{error}; cloud sync on Windows needs a private .libra: run {}, then retry",
                    icacls_private_commands(storage)
                ),
            )
        })?;
    }
    Ok(())
}

/// A spool read after the freeze failed. Remote writes may already have
/// happened, so this never claims otherwise.
fn spool_read_failure(path: &Path, error: impl fmt::Display) -> CloudError {
    CloudError::Generic(format!(
        "cloud sync could not read its local snapshot spool at {}: {error}; part of this run may already have been sent to D1 or R2. Re-run `libra cloud sync`: objects not marked synced locally are sent again",
        path.display()
    ))
}

/// Refuse a `.libra` entry that is itself a symbolic link. Repository
/// discovery resolves links, so the raw entry in the working directory is
/// checked separately.
fn ensure_raw_root_not_linked(raw_root: &Path) -> CloudResult<()> {
    match fs::symlink_metadata(raw_root) {
        Ok(meta) if meta.file_type().is_symlink() => Err(spool_failure(
            "use",
            raw_root,
            "the repository storage directory is a symbolic link and is not followed",
        )),
        _ => Ok(()),
    }
}

/// The `icacls` commands, each in backticks, that make `dir` private:
/// inheritance removed with full control for the current user and SYSTEM
/// only, then the current user as owner. The check also requires that
/// owner, and an elevated administrator's new files are owned by the
/// Administrators group instead. The account is the process token's user
/// SID (`*S-1-5-...`), never the `USERNAME` environment variable (PI-25).
#[cfg(windows)]
pub(super) fn icacls_private_commands(dir: &Path) -> String {
    let user = crate::utils::windows_fs::current_user_sid_string()
        .map(|sid| format!("*{sid}"))
        .unwrap_or_else(|_| "<your account>".to_string());
    format!(
        "`icacls \"{path}\" /inheritance:r /grant:r \"{user}:(OI)(CI)F\" \"SYSTEM:(OI)(CI)F\"` and then `icacls \"{path}\" /setowner \"{user}\"` (if `icacls \"{path}\"` still lists another account, remove it with `icacls \"{path}\" /remove \"<account>\"`)",
        path = dir.display()
    )
}

/// Accept an existing parent only when it is a real directory (never a
/// symlink) inside `.libra`, owned by the current user and not writable by
/// group or others; create it owner-only when absent.
fn ensure_spool_parent(storage: &Path, dir: &Path) -> CloudResult<()> {
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_parent_or_reuse(dir).map_err(|error| spool_failure("create", dir, error))?;
            fs::symlink_metadata(dir).map_err(|error| spool_failure("inspect", dir, error))?
        }
        Err(error) => return Err(spool_failure("inspect", dir, error)),
    };
    if meta.file_type().is_symlink() {
        return Err(spool_failure(
            "use",
            dir,
            "the path is a symbolic link and is not followed",
        ));
    }
    if !meta.is_dir() {
        return Err(spool_failure("use", dir, "the path is not a directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err(spool_failure(
                "use",
                dir,
                format!(
                    "the directory is owned by uid {} but cloud sync runs as uid {euid}",
                    meta.uid()
                ),
            ));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(spool_failure(
                "use",
                dir,
                "the directory is writable by group or others; run `chmod go-w` on it",
            ));
        }
    }
    #[cfg(windows)]
    {
        // Opened without following reparse points; the DACL must be protected
        // and grant only the current user and SYSTEM.
        crate::utils::windows_fs::open_private_directory(dir).map_err(|error| {
            spool_failure(
                "use",
                dir,
                format!(
                    "{error}; make it private with {}, then retry",
                    icacls_private_commands(dir)
                ),
            )
        })?;
    }
    let root = storage
        .canonicalize()
        .map_err(|error| spool_failure("resolve", storage, error))?;
    let resolved = dir
        .canonicalize()
        .map_err(|error| spool_failure("resolve", dir, error))?;
    if !resolved.starts_with(&root) {
        return Err(spool_failure(
            "use",
            dir,
            "the directory resolves outside .libra",
        ));
    }
    Ok(())
}

/// Create a spool parent; a concurrent sync that created it first is not an
/// error, because the caller validates whatever is there next.
pub(super) fn create_parent_or_reuse(dir: &Path) -> std::io::Result<()> {
    match create_private_dir(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}

/// Create one directory (not its parents), failing if it already exists.
/// Unix: mode 0700. Windows: a protected DACL granting only the current user
/// and SYSTEM, inherited by the files created inside.
pub(super) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        crate::utils::windows_fs::create_private_directory(dir)
    }
    #[cfg(not(windows))]
    {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)
    }
}

/// Create a new file that must not exist yet, owner-only (Unix mode 0600;
/// on Windows it inherits the private run directory's DACL).
pub(super) fn create_private_file(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        // A file created by path takes the token's default owner, which for
        // an elevated administrator is the Administrators group. Create it
        // relative to the private directory with the current user as owner,
        // so the owner-only check on a marker holds on Windows too (PI-25).
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("a spool file needs a parent directory"))?;
        let name = path
            .file_name()
            .ok_or_else(|| std::io::Error::other("a spool file needs a name"))?;
        let dir = crate::utils::windows_fs::open_private_directory(parent)?;
        crate::utils::windows_fs::create_private_file_at(&dir, name)
    }
    #[cfg(not(windows))]
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)
    }
}

impl SnapshotSpool {
    /// Create this run's spool directory under `<storage>/cloud-sync/snapshots`.
    pub(super) fn create(
        storage: &Path,
        raw_root: &Path,
        tuning: &SnapshotTuning,
        warning_sink: SpoolWarningSink,
    ) -> CloudResult<Self> {
        ensure_raw_root_not_linked(raw_root)?;
        ensure_trusted_root(storage)?;
        let root = storage.join(SPOOL_ROOT_DIR);
        ensure_spool_parent(storage, &root)?;
        let snapshots = root.join(SPOOL_SNAPSHOTS_DIR);
        ensure_spool_parent(storage, &snapshots)?;
        #[cfg(test)]
        let name = tuning
            .run_name
            .clone()
            .unwrap_or_else(|| format!("run-{}", Uuid::new_v4()));
        #[cfg(not(test))]
        let name = format!("run-{}", Uuid::new_v4());
        let dir = snapshots.join(name);
        create_private_dir(&dir).map_err(|error| spool_failure("create", &dir, error))?;
        // From here on, dropping the spool removes the directory.
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut spool = Self {
            dir,
            warning_sink,
            objects: 0,
            references: 0,
            refs_bytes: 0,
            refs_digest: 0,
            catalog_bytes: 0,
            catalog_digest: 0,
            released: false,
            #[cfg(test)]
            fail_release: tuning.fail_spool_release,
        };
        #[cfg(not(test))]
        let _ = tuning;
        match spool.write_marker(tuning) {
            Ok(()) => Ok(spool),
            // Remove the half-made run now so a removal failure is reported
            // with the error instead of only being logged by `Drop`.
            Err(error) => Err(match spool.finish() {
                Some(note) => error.with_note(&note),
                None => error,
            }),
        }
    }

    fn write_marker(&mut self, tuning: &SnapshotTuning) -> CloudResult<()> {
        let marker_path = self.dir.join(OWNER_MARKER_FILE);
        #[cfg(test)]
        if tuning.fail_marker_write {
            return Err(spool_failure(
                "write",
                &marker_path,
                std::io::Error::other("injected marker write failure"),
            ));
        }
        #[cfg(not(test))]
        let _ = tuning;
        let marker = serde_json::to_vec(&OwnerMarker::for_this_process())
            .map_err(|error| spool_failure("write", &marker_path, error))?;
        let mut marker_file = create_private_file(&marker_path)
            .map_err(|error| spool_failure("create", &marker_path, error))?;
        marker_file
            .write_all(&marker)
            .and_then(|()| marker_file.sync_all())
            .map_err(|error| spool_failure("write", &marker_path, error))
    }

    #[cfg(test)]
    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(super) fn object_count(&self) -> u64 {
        self.objects
    }

    /// Freeze what this run uploads: the object identities (unsynced ones, or
    /// all of the repository's with `force`), the refs projection and the
    /// project name.
    ///
    /// Every read runs as its own short statement on one dedicated read
    /// connection, so no database lock is held across the scan and other
    /// processes' writers wait at most one statement. SQLite's
    /// `PRAGMA data_version` on that connection changes exactly when any
    /// other connection commits, so an unchanged value before the first and
    /// after the last read proves all of them saw one database state. A change
    /// retries the freeze; after [`MAX_FREEZE_ATTEMPTS`] the run stops, still
    /// before any remote write.
    pub(super) async fn freeze(
        &mut self,
        db_path: &Path,
        repo_id: &str,
        force: bool,
        tuning: &SnapshotTuning,
    ) -> CloudResult<()> {
        let db_path_text = db_path.to_str().ok_or_else(|| {
            CloudError::Generic(format!(
                "the repository database path {} is not valid UTF-8",
                db_path.display()
            ))
        })?;
        let conn = crate::internal::db::open_connection_without_schema_management(
            db_path_text,
            crate::internal::db::DEFAULT_CONNECTION_TIMEOUT,
        )
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "open the repository database for the snapshot: {error}; nothing was written to D1 or R2"
            ))
        })?;
        // TEMP objects (the connection marker and the refs copy) live in a
        // temporary file rather than memory, so a large ref set never has to
        // fit in memory.
        if let Err(error) = conn.execute_unprepared("PRAGMA temp_store = FILE").await {
            let _ = conn.close().await;
            return Err(CloudError::Generic(format!(
                "prepare the repository database for the snapshot: {error}; nothing was written to D1 or R2"
            )));
        }
        // The capture catalog is a separate file once it moved out of
        // `libra.db`; its reads get their own change detection below.
        let catalog_conn = match super::agent_capture::local_capture_catalog(&conn).await {
            Ok(catalog_conn) => catalog_conn,
            Err(error) => {
                let _ = conn.close().await;
                return Err(error);
            }
        };
        let split_catalog = match crate::internal::db::connection_has_table(&conn, "agent_session")
            .await
        {
            Ok(combined) => !combined,
            Err(error) => {
                let _ = conn.close().await;
                return Err(CloudError::Generic(format!(
                    "inspect the repository database for the snapshot: {error}; nothing was written to D1 or R2"
                )));
            }
        };
        let catalog = split_catalog.then_some(&catalog_conn);
        let mut outcome = Err(CloudError::Generic(String::new()));
        for attempt in 1..=MAX_FREEZE_ATTEMPTS {
            match self
                .freeze_once(&conn, catalog, repo_id, force, tuning, attempt)
                .await
            {
                Ok(true) => {
                    outcome = Ok(());
                    break;
                }
                Ok(false) => {
                    outcome = Err(CloudError::Generic(format!(
                        "the local repository database kept changing while cloud sync took its snapshot ({MAX_FREEZE_ATTEMPTS} attempts); nothing was written to D1 or R2. Re-run `libra cloud sync` when other Libra commands in this repository have finished"
                    )));
                }
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
        }
        let _ = conn.close().await;
        outcome
    }

    /// One freeze attempt. Returns `false` when another connection committed
    /// during the attempt, so its reads may mix two database states.
    async fn freeze_once(
        &mut self,
        conn: &sea_orm::DatabaseConnection,
        catalog_conn: Option<&sea_orm::DatabaseConnection>,
        repo_id: &str,
        force: bool,
        tuning: &SnapshotTuning,
        attempt: usize,
    ) -> CloudResult<bool> {
        let page_size = tuning.page_size();
        let objects_path = self.dir.join(OBJECTS_FILE);
        let file = recreate_private_file(&objects_path)
            .map_err(|error| spool_failure("create", &objects_path, error))?;
        let mut writer = BufWriter::new(file);
        #[cfg(test)]
        if tuning.fail_spool_write {
            return Err(spool_failure(
                "write",
                &objects_path,
                std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    "no space left on device (injected)",
                ),
            ));
        }
        #[cfg(not(test))]
        let _ = attempt;

        let read_failure = |error: sea_orm::DbErr| {
            CloudError::Generic(format!(
                "read the repository for the snapshot: {error}; nothing was written to D1 or R2"
            ))
        };
        // `data_version` values compare only on one physical connection, and
        // the pool behind `conn` could replace it between statements. A row
        // in a connection-private TEMP table proves the connection at the end
        // is the one from the start; TEMP writes never touch the repository
        // database or its lock.
        let marked = mark_freeze_connection(conn, attempt).await;
        #[cfg(test)]
        let marked = if tuning.fail_mark_write {
            Err(sea_orm::DbErr::Custom(
                "disk I/O error writing the TEMP marker (injected)".to_string(),
            ))
        } else {
            marked
        };
        #[cfg(test)]
        let marked = if tuning.simulate_reconnect_during_mark && attempt == 1 {
            // What a replacement in the middle of marking looks like: the
            // marker table made on the old connection is gone.
            conn.execute_unprepared("DROP TABLE temp.libra_cloud_sync_freeze")
                .await
                .map_err(read_failure)?;
            conn.execute_unprepared("INSERT INTO temp.libra_cloud_sync_freeze (attempt) VALUES (1)")
                .await
                .map(|_| ())
        } else {
            marked
        };
        if let Err(error) = marked {
            replaced_connection(conn, attempt, error)
                .await
                .map_err(read_failure)?;
            return Ok(false);
        }
        let start = data_version(conn).await.map_err(read_failure)?;
        // A split catalog proves its own connection and change counter the
        // same way; a commit to either file retries the attempt.
        let catalog_start = match catalog_conn {
            Some(catalog_conn) => {
                if let Err(error) = mark_freeze_connection(catalog_conn, attempt).await {
                    replaced_connection(catalog_conn, attempt, error)
                        .await
                        .map_err(read_failure)?;
                    return Ok(false);
                }
                Some(data_version(catalog_conn).await.map_err(read_failure)?)
            }
            None => None,
        };
        let boundary = conn
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT MAX(id) AS max_id FROM object_index WHERE repo_id = ?",
                [repo_id.into()],
            ))
            .await
            .map_err(read_failure)?
            .map(|row| row.try_get::<Option<i64>>("", "max_id"))
            .transpose()
            .map_err(read_failure)?
            .flatten();
        // The refs (PI-27): first copied up to their own boundary into the
        // connection's TEMP copy a page per short statement in `id` order, so
        // a large ref set never holds the read lock for long and refs inserted
        // meanwhile cannot keep the copy going; then streamed into the spool
        // in upload order a page at a time through the copy's order index, so
        // at most one page of refs is in memory and no page sorts the whole
        // set. The `data_version` check covers changes during both steps, and
        // a retry rewrites the file.
        let refs_path = self.dir.join(REFS_FILE);
        let refs_file = recreate_private_file(&refs_path)
            .map_err(|error| spool_failure("create", &refs_path, error))?;
        let mut refs_writer =
            ReferenceProjectionWriter::new(DigestWriter::new(BufWriter::new(refs_file)))
                .map_err(|error| spool_failure("write", &refs_path, error))?;
        let ref_boundary = conn
            .query_one_raw(sea_orm::Statement::from_string(
                conn.get_database_backend(),
                "SELECT MAX(id) AS max_id FROM reference",
            ))
            .await
            .map_err(read_failure)?
            .map(|row| row.try_get::<Option<i64>>("", "max_id"))
            .transpose()
            .map_err(read_failure)?
            .flatten();
        let prepared = prepare_reference_copy(conn).await;
        #[cfg(test)]
        let prepared = if tuning.simulate_reconnect_during_ref_setup && attempt == 1 {
            // What a replacement between creating the TEMP table and its
            // index looks like: the marker and the table are gone, and the
            // index creation fails on the missing table.
            for table in ["libra_cloud_sync_refs", "libra_cloud_sync_freeze"] {
                conn.execute_unprepared(&format!("DROP TABLE temp.{table}"))
                    .await
                    .map_err(read_failure)?;
            }
            conn.execute_unprepared(
                "CREATE INDEX temp.libra_cloud_sync_refs_order ON libra_cloud_sync_refs (id)",
            )
            .await
            .map(|_| ())
        } else {
            prepared
        };
        if let Err(error) = prepared {
            replaced_connection(conn, attempt, error)
                .await
                .map_err(read_failure)?;
            return Ok(false);
        }
        if let Some(ref_boundary) = ref_boundary {
            let mut last_id = i64::MIN;
            loop {
                let (copied, last) =
                    match copy_reference_page(conn, last_id, ref_boundary, page_size).await {
                        Ok(copied) => copied,
                        Err(error) => {
                            replaced_connection(conn, attempt, error)
                                .await
                                .map_err(read_failure)?;
                            return Ok(false);
                        }
                    };
                #[cfg(test)]
                if let Some(between_ref_pages) = &tuning.between_ref_pages {
                    between_ref_pages(attempt).await;
                }
                #[cfg(test)]
                if tuning.simulate_reconnect_during_refs && attempt == 1 {
                    // What a fresh connection looks like mid-scan: neither
                    // the TEMP ref copy nor the marker.
                    for table in ["libra_cloud_sync_refs", "libra_cloud_sync_freeze"] {
                        conn.execute_unprepared(&format!("DROP TABLE temp.{table}"))
                            .await
                            .map_err(read_failure)?;
                    }
                }
                match last {
                    Some(last) if copied > 0 => last_id = last,
                    _ => break,
                }
                if copied < page_size as u64 {
                    break;
                }
            }
        }
        let mut after_ref: Option<ReferenceProjectionKey> = None;
        loop {
            let rows = match reference_projection_page(conn, after_ref.as_ref(), page_size).await {
                Ok(rows) => rows,
                Err(error) => {
                    replaced_connection(conn, attempt, error)
                        .await
                        .map_err(read_failure)?;
                    return Ok(false);
                }
            };
            let page = FreezePage::new(rows, PageKind::Refs);
            let Some(last) = page.rows.last() else {
                break;
            };
            after_ref = Some(ReferenceProjectionKey::of(last));
            for model in &page.rows {
                refs_writer
                    .push(model)
                    .map_err(|error| spool_failure("write", &refs_path, error))?;
            }
            refs_writer
                .flush()
                .map_err(|error| spool_failure("write", &refs_path, error))?;
            let page_len = page.rows.len();
            drop(page);
            #[cfg(test)]
            if let Some(between_projection_pages) = &tuning.between_projection_pages {
                between_projection_pages(attempt).await;
            }
            if page_len < page_size {
                break;
            }
        }
        #[cfg(test)]
        if tuning.fail_config_read {
            return Err(CloudError::Generic(
                "read cloud.name for the snapshot: injected config read failure; nothing was written to D1 or R2".to_string(),
            ));
        }
        let cloud_name = ConfigKv::get_with_conn(conn, "cloud.name")
            .await
            .map_err(|error| {
                CloudError::Generic(format!(
                    "read cloud.name for the snapshot: {error}; nothing was written to D1 or R2"
                ))
            })?
            .map(|entry| entry.value);
        let repository = FrozenRepository {
            project_name: cloud_name.unwrap_or_else(|| {
                util::working_dir()
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown-project".to_string())
            }),
        };

        // The object identities up to the boundary (ids only grow), a page
        // per statement.
        let mut objects = 0u64;
        if let Some(boundary) = boundary {
            let mut last_id = i64::MIN;
            loop {
                let mut query = object_index::Entity::find()
                    .filter(object_index::Column::RepoId.eq(repo_id))
                    .filter(object_index::Column::Id.gt(last_id))
                    .filter(object_index::Column::Id.lte(boundary));
                if !force {
                    query = query.filter(object_index::Column::IsSynced.eq(0));
                }
                let mut page = FreezePage::new(
                    query
                        .order_by_asc(object_index::Column::Id)
                        .limit(page_size as u64)
                        .all(conn)
                        .await
                        .map_err(read_failure)?,
                    PageKind::Objects,
                );
                let Some(last) = page.rows.last() else {
                    break;
                };
                last_id = last.id;
                let page_len = page.rows.len();
                // Each page is written out before the next is read: the spool
                // grows a page at a time and only this page is in memory.
                for model in std::mem::take(&mut page.rows) {
                    serde_json::to_writer(&mut writer, &FrozenObject::from(model))
                        .map_err(|error| spool_failure("write", &objects_path, error))?;
                    writer
                        .write_all(b"\n")
                        .map_err(|error| spool_failure("write", &objects_path, error))?;
                    objects += 1;
                }
                writer
                    .flush()
                    .map_err(|error| spool_failure("write", &objects_path, error))?;
                drop(page);
                #[cfg(test)]
                if let Some(between_pages) = &tuning.between_pages {
                    between_pages(attempt).await;
                }
                if page_len < page_size {
                    break;
                }
            }
        }
        // The agent-capture catalog (PI-26), in the same window: each page
        // its own short statement, the rows written private next to the rest
        // of the spool. A read failure is kept for the mirror phase.
        #[cfg(test)]
        let catalog_hook: Option<super::agent_capture::CatalogPageHook> =
            tuning.between_catalog_pages.clone().map(|hook| {
                let hook: super::agent_capture::CatalogPageHook =
                    std::sync::Arc::new(move || hook(attempt));
                hook
            });
        #[cfg(not(test))]
        let catalog_hook: Option<super::agent_capture::CatalogPageHook> = None;
        let catalog = super::agent_capture::freeze_agent_capture(
            catalog_conn.unwrap_or(conn),
            catalog_hook.as_ref(),
        )
        .await;
        let catalog_path = self.dir.join(CATALOG_FILE);
        let catalog_file = recreate_private_file(&catalog_path)
            .map_err(|error| spool_failure("create", &catalog_path, error))?;
        let mut catalog_out = DigestWriter::new(BufWriter::new(catalog_file));
        serde_json::to_writer(&mut catalog_out, &catalog)
            .map_err(|error| spool_failure("write", &catalog_path, error))?;
        catalog_out
            .flush()
            .map_err(|error| spool_failure("write", &catalog_path, error))?;
        let catalog_digest = catalog_out.digest();
        let catalog_file = catalog_out
            .into_inner()
            .into_inner()
            .map_err(|error| spool_failure("write", &catalog_path, error.into_error()))?;
        catalog_file
            .sync_all()
            .map_err(|error| spool_failure("write", &catalog_path, error))?;
        let catalog_bytes = catalog_file
            .metadata()
            .map_err(|error| spool_failure("write", &catalog_path, error))?
            .len();
        drop(catalog);

        #[cfg(test)]
        if tuning.simulate_reconnect && attempt == 1 {
            // What a fresh connection looks like: no TEMP marker.
            conn.execute_unprepared("DROP TABLE temp.libra_cloud_sync_freeze")
                .await
                .map_err(read_failure)?;
        }
        if data_version(conn).await.map_err(read_failure)? != start
            || !same_freeze_connection(conn, attempt)
                .await
                .map_err(read_failure)?
        {
            return Ok(false);
        }
        if let (Some(catalog_conn), Some(catalog_start)) = (catalog_conn, catalog_start)
            && (data_version(catalog_conn).await.map_err(read_failure)? != catalog_start
                || !same_freeze_connection(catalog_conn, attempt)
                    .await
                    .map_err(read_failure)?)
        {
            return Ok(false);
        }

        writer
            .flush()
            .map_err(|error| spool_failure("write", &objects_path, error))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| spool_failure("write", &objects_path, error))?;

        let (refs_out, references) = refs_writer
            .finish()
            .map_err(|error| spool_failure("write", &refs_path, error))?;
        let refs_digest = refs_out.digest();
        let refs_file = refs_out
            .into_inner()
            .into_inner()
            .map_err(|error| spool_failure("write", &refs_path, error.into_error()))?;
        refs_file
            .sync_all()
            .map_err(|error| spool_failure("write", &refs_path, error))?;
        let refs_bytes = refs_file
            .metadata()
            .map_err(|error| spool_failure("write", &refs_path, error))?
            .len();

        let repository_path = self.dir.join(REPOSITORY_FILE);
        let repository_json = serde_json::to_vec(&repository)
            .map_err(|error| spool_failure("write", &repository_path, error))?;
        let mut repository_file = recreate_private_file(&repository_path)
            .map_err(|error| spool_failure("create", &repository_path, error))?;
        repository_file
            .write_all(&repository_json)
            .and_then(|()| repository_file.sync_all())
            .map_err(|error| spool_failure("write", &repository_path, error))?;

        self.objects = objects;
        self.references = references;
        self.refs_bytes = refs_bytes;
        self.refs_digest = refs_digest;
        self.catalog_bytes = catalog_bytes;
        self.catalog_digest = catalog_digest;
        Ok(true)
    }

    /// The repository metadata frozen with this snapshot.
    pub(super) fn repository(&self) -> CloudResult<FrozenRepository> {
        let path = self.dir.join(REPOSITORY_FILE);
        let bytes = fs::read(&path).map_err(|error| spool_read_failure(&path, error))?;
        serde_json::from_slice(&bytes).map_err(|error| spool_read_failure(&path, error))
    }

    /// Read the frozen identities back, `page_size` at a time.
    pub(super) fn object_pages(&self, page_size: usize) -> CloudResult<FrozenObjectPages> {
        let path = self.dir.join(OBJECTS_FILE);
        let file = File::open(&path).map_err(|error| spool_read_failure(&path, error))?;
        Ok(FrozenObjectPages {
            reader: BufReader::new(file),
            path,
            page_size: page_size.max(1),
            remaining: self.objects,
        })
    }

    /// The agent-capture catalog frozen with this snapshot (PI-26); a file
    /// that is not byte for byte what the freeze wrote is a read failure.
    pub(super) fn agent_capture(&self) -> CloudResult<super::agent_capture::FrozenAgentCapture> {
        use std::{
            hash::{DefaultHasher, Hasher},
            io::Read,
        };

        // One read: the bytes checked against the freeze's length and
        // digest are the bytes deserialized.
        let path = self.dir.join(CATALOG_FILE);
        let mut bytes = Vec::new();
        File::open(&path)
            .and_then(|file| {
                file.take(self.catalog_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
            })
            .map_err(|error| spool_read_failure(&path, error))?;
        let mut digest = DefaultHasher::new();
        digest.write(&bytes);
        if bytes.len() as u64 != self.catalog_bytes || digest.finish() != self.catalog_digest {
            return Err(spool_read_failure(
                &path,
                "the spool no longer holds the agent-capture catalog the freeze wrote",
            ));
        }
        serde_json::from_slice(&bytes).map_err(|error| spool_read_failure(&path, error))
    }

    /// The frozen refs projection, hashed from the spool file
    /// `chunk_size` bytes at a time; a file that is not byte for byte what
    /// the freeze wrote (its length and digest) is a read failure.
    pub(super) fn refs(&self, tuning: &SnapshotTuning) -> CloudResult<FrozenRefs> {
        let chunk_size = tuning.metadata_chunk_size();
        let path = self.dir.join(REFS_FILE);
        let (hash, digest, len) = metadata_hash_of_file(&path, chunk_size)
            .map_err(|error| spool_read_failure(&path, error))?;
        if len != self.refs_bytes {
            return Err(spool_read_failure(
                &path,
                format!(
                    "the frozen refs are {} bytes, the spool holds {len}",
                    self.refs_bytes
                ),
            ));
        }
        if digest != self.refs_digest {
            return Err(spool_read_failure(
                &path,
                "the spool no longer holds the refs the freeze wrote",
            ));
        }
        Ok(FrozenRefs {
            references: self.references,
            path,
            len,
            hash,
            upload_chunk_size: chunk_size,
            report: background_warning_report(self.warning_sink),
            #[cfg(test)]
            before_upload: tuning.before_metadata_upload.clone(),
            #[cfg(test)]
            fail_hash_record: tuning.fail_metadata_hash_record,
        })
    }

    /// Remove the run directory now. Returns a user-facing note naming the
    /// directory when it could not be removed; the caller reports it.
    pub(super) fn finish(mut self) -> Option<String> {
        self.remove()
    }

    fn remove(&mut self) -> Option<String> {
        if self.released {
            return None;
        }
        self.released = true;
        #[cfg(test)]
        let result = if self.fail_release {
            Err(std::io::Error::other("injected removal failure"))
        } else {
            fs::remove_dir_all(&self.dir)
        };
        #[cfg(not(test))]
        let result = fs::remove_dir_all(&self.dir);
        let error = result.err()?;
        tracing::warn!(
            spool = %self.dir.display(),
            error = %error,
            "could not remove the cloud sync snapshot spool"
        );
        Some(format!(
            "cloud sync could not remove its snapshot spool {}: {error}. It holds {}; it is never reused, so delete that directory manually",
            self.dir.display(),
            describe_run_contents(&self.dir)
        ))
    }
}

/// Name prefix of the run directories under the `snapshots` directory.
const RUN_DIR_PREFIX: &str = "run-";
/// The most bytes of an owner marker cleanup reads.
const MAX_OWNER_MARKER_BYTES: u64 = 64 * 1024;

/// Remove the leftover run directories of earlier syncs whose owner provably
/// exited, and describe every leftover kept (PI-25). Runs at the start of
/// `cloud sync`, before this run creates its own directory, and returns one
/// note per directory kept, not inspected, or not removable, for the caller
/// to report.
///
/// Fail-closed: a directory is removed only when its owner marker is a
/// private regular file naming this tool, this host, this boot and (on Linux)
/// this PID namespace, and signal 0 to its pid reports no such process.
/// Nothing is followed through a link, nothing outside `snapshots` is
/// touched, and only the marker of a run is ever read.
pub(super) fn clean_residues(
    storage: &Path,
    raw_root: &Path,
    tuning: &SnapshotTuning,
) -> Vec<String> {
    #[cfg(not(test))]
    let _ = tuning;
    let snapshots = storage.join(SPOOL_ROOT_DIR).join(SPOOL_SNAPSHOTS_DIR);
    let mut notes = Vec::new();
    // `.libra` itself goes through PI-09's checks — first the raw entry in
    // the working directory (repository discovery resolves a link), then its
    // ownership; the spool parents may simply not exist yet.
    if let Err(error) =
        ensure_raw_root_not_linked(raw_root).and_then(|()| ensure_trusted_root(storage))
    {
        notes.push(format!(
            "cloud sync did not look for leftover snapshot directories: {error}"
        ));
        return notes;
    }
    for dir in [storage.join(SPOOL_ROOT_DIR), snapshots.clone()] {
        match check_private_dir(&dir, PARENT_PERMISSION_BITS) {
            Ok(true) => {}
            Ok(false) => return notes,
            Err(reason) => {
                notes.push(format!(
                    "cloud sync did not look for leftover snapshot directories under {}: {reason}; nothing there was read or deleted",
                    dir.display()
                ));
                return notes;
            }
        }
    }
    let entries = match fs::read_dir(&snapshots) {
        Ok(entries) => entries,
        Err(error) => {
            notes.push(format!(
                "cloud sync could not list leftover snapshot directories in {}: {error}",
                snapshots.display()
            ));
            return notes;
        }
    };
    let mut candidates = Vec::new();
    let mut listing_error = None;
    for entry in entries {
        match entry {
            Ok(entry)
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(RUN_DIR_PREFIX) =>
            {
                candidates.push(entry.path());
            }
            Ok(_) => {}
            Err(error) => listing_error = Some(error),
        }
    }
    #[cfg(test)]
    if tuning.fail_residue_listing {
        listing_error = Some(std::io::Error::other("injected listing failure"));
    }
    if let Some(error) = listing_error {
        notes.push(format!(
            "cloud sync could not list every leftover snapshot directory in {}: {error}; the ones it could not list were not inspected",
            snapshots.display()
        ));
    }
    candidates.sort();
    let me = ProcessIdentity::current();
    for run in candidates {
        if let Some(note) = clean_residue(&run, &me, tuning) {
            notes.push(note);
        }
    }
    notes
}

/// This process's identity, which an owner marker must match before its run
/// can be proven gone (PI-25).
#[derive(Debug, Clone)]
pub(super) struct ProcessIdentity {
    pub(super) host: String,
    pub(super) boot_id: Option<String>,
    // Read only where an exited owner can be proven (Linux and macOS).
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    pub(super) pid_namespace: Option<String>,
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    pub(super) pid: u32,
}

impl ProcessIdentity {
    pub(super) fn current() -> Self {
        Self {
            host: host_name(),
            boot_id: boot_id(),
            pid_namespace: pid_namespace(),
            pid: std::process::id(),
        }
    }
}

/// Signal 0 to a process: `Ok(())` when it exists and may be signalled,
/// otherwise the OS error (`ESRCH`: no such process).
pub(super) type SignalProbe = fn(i32) -> std::io::Result<()>;

/// [`SignalProbe`] through `kill(pid, 0)`.
pub(super) fn signal_zero(pid: i32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that `pid` exists and may be
        // signalled; callers pass a positive pid, which names exactly one
        // process.
        if unsafe { libc::kill(pid, 0) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

/// Group/other permission bits a spool parent must not have (it may be
/// readable, never writable by others).
const PARENT_PERMISSION_BITS: u32 = 0o022;
/// Group/other permission bits a run directory or owner marker must not
/// have: PI-09 creates them owner-only.
const PRIVATE_PERMISSION_BITS: u32 = 0o077;

/// Judge one leftover run directory; `None` when it was removed.
fn clean_residue(run: &Path, me: &ProcessIdentity, tuning: &SnapshotTuning) -> Option<String> {
    #[cfg(not(test))]
    let _ = tuning;
    let uninspected = |reason: String| {
        Some(format!(
            "cloud sync did not inspect the leftover snapshot directory {}: {reason}; nothing in it was read or deleted. Check who created it before deleting it by hand",
            run.display()
        ))
    };
    if let Err(reason) = check_private_dir(run, PRIVATE_PERMISSION_BITS).and_then(|present| {
        present
            .then_some(())
            .ok_or_else(|| "it disappeared while cloud sync looked at it".to_string())
    }) {
        return uninspected(reason);
    }
    let marker = match read_private_marker(run) {
        Ok(marker) => marker,
        Err(reason) => return uninspected(reason),
    };
    let kept = |reason: String| {
        Some(format!(
            "cloud sync kept the leftover snapshot directory {} of an earlier run: {reason}. It holds {}; delete it by hand once you have confirmed that run has stopped (no `libra cloud sync` for this repository is still running on that host)",
            run.display(),
            describe_run_contents(run)
        ))
    };
    let marker = match marker {
        None => {
            return kept(
                "its owner marker is missing, so its owner cannot be identified".to_string(),
            );
        }
        Some(bytes) => match serde_json::from_slice::<OwnerMarker>(&bytes) {
            Ok(marker) => marker,
            Err(error) => return kept(format!("its owner marker is not valid ({error})")),
        },
    };
    if let Err(reason) = owner_exited(&marker, me, signal_zero) {
        return kept(reason);
    }
    #[cfg(test)]
    let removed = if tuning.fail_residue_removal {
        Err(std::io::Error::other("injected removal failure"))
    } else {
        fs::remove_dir_all(run)
    };
    #[cfg(not(test))]
    let removed = fs::remove_dir_all(run);
    removed.err().map(|error| {
        format!(
            "cloud sync could not remove the leftover snapshot directory {} of a run that has exited: {error}. It holds {}; delete that directory by hand",
            run.display(),
            describe_run_contents(run)
        )
    })
}

/// The snapshot files present in the run directory `run` and what each
/// holds, for a note about a run directory that stays on disk. Each file is
/// only looked at, never followed through a link or read.
pub(super) fn describe_run_contents(run: &Path) -> String {
    let present: Vec<String> = RUN_FILES
        .iter()
        .filter(|(name, _)| fs::symlink_metadata(run.join(name)).is_ok_and(|meta| meta.is_file()))
        .map(|(name, holds)| format!("{name} ({holds})"))
        .collect();
    if present.is_empty() {
        "none of the snapshot files".to_string()
    } else {
        present.join(", ")
    }
}

/// Whether `dir` is a real directory (never followed through a link) owned
/// by the current user without any of `forbidden` permission bits (on
/// Windows: a protected DACL granting only the current user and SYSTEM).
/// `Ok(false)` when it does not exist.
fn check_private_dir(dir: &Path, forbidden: u32) -> Result<bool, String> {
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("it cannot be inspected ({error})")),
    };
    if meta.file_type().is_symlink() {
        return Err("it is a symbolic link, which cloud sync does not follow".to_string());
    }
    if !meta.is_dir() {
        return Err("it is not a directory".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid only reads the process credential.
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err("it belongs to another user".to_string());
        }
        if meta.mode() & forbidden != 0 {
            return Err(format!(
                "it is accessible to group or others (mode {:o})",
                meta.mode() & 0o777
            ));
        }
    }
    #[cfg(windows)]
    {
        let _ = forbidden;
        crate::utils::windows_fs::open_private_directory(dir).map_err(|error| {
            format!(
                "{error}; make it private with {}",
                icacls_private_commands(dir)
            )
        })?;
    }
    #[cfg(not(any(unix, windows)))]
    let _ = forbidden;
    Ok(true)
}

/// Read the owner marker of `run` without following a link: `None` when
/// there is none; an error when it is not a private regular file of the
/// current user (another user could have forged it).
fn read_private_marker(run: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;

    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let path = run.join(OWNER_MARKER_FILE);
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Err(format!(
                    "its owner marker {} is a symbolic link, which cloud sync does not follow",
                    path.display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "its owner marker {} cannot be opened ({error})",
                    path.display()
                ));
            }
        };
        let meta = file
            .metadata()
            .map_err(|error| format!("its owner marker cannot be inspected ({error})"))?;
        // SAFETY: geteuid only reads the process credential.
        let euid = unsafe { libc::geteuid() };
        if !meta.is_file() {
            return Err("its owner marker is not a regular file".to_string());
        }
        if meta.uid() != euid {
            return Err("its owner marker belongs to another user".to_string());
        }
        if meta.mode() & PRIVATE_PERMISSION_BITS != 0 {
            return Err(format!(
                "its owner marker is accessible to group or others (mode {:o}), so it could have been written by someone else",
                meta.mode() & 0o777
            ));
        }
        file
    };
    #[cfg(windows)]
    let file = {
        let dir = crate::utils::windows_fs::open_private_directory(run)
            .map_err(|error| format!("it cannot be opened privately ({error})"))?;
        let file = match crate::utils::windows_fs::open_at(
            &dir,
            std::ffi::OsStr::new(OWNER_MARKER_FILE),
            false,
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "its owner marker cannot be opened without following a link ({error})"
                ));
            }
        };
        crate::utils::windows_fs::validate_private(&file).map_err(|error| {
            format!("its owner marker is not private ({error}), so it could have been written by someone else")
        })?;
        file
    };
    #[cfg(not(any(unix, windows)))]
    let file: File = return Err("this platform cannot check an owner marker privately".to_string());
    let mut bytes = Vec::new();
    file.take(MAX_OWNER_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("its owner marker cannot be read ({error})"))?;
    if bytes.len() as u64 > MAX_OWNER_MARKER_BYTES {
        return Err("its owner marker is larger than any marker cloud sync writes".to_string());
    }
    Ok(Some(bytes))
}

/// `Ok` only when `marker` proves its run's process has exited: written by
/// this tool, on this host, in this boot and (on Linux) this PID namespace —
/// all as recorded in `me` — for a pid that is not this process and that
/// `probe` (signal 0) reports as gone. Otherwise the reason it cannot be
/// proven.
pub(super) fn owner_exited(
    marker: &OwnerMarker,
    me: &ProcessIdentity,
    probe: SignalProbe,
) -> Result<(), String> {
    if marker.tool != OWNER_MARKER_TOOL {
        return Err(format!(
            "it was written by another tool ({:?})",
            marker.tool
        ));
    }
    if me.host.is_empty() || marker.host != me.host {
        return Err(format!(
            "it was written on another host ({:?}), where its run may still be going",
            marker.host
        ));
    }
    match (marker.boot_id.as_deref(), me.boot_id.as_deref()) {
        (Some(theirs), Some(ours)) if !theirs.is_empty() && theirs == ours => {}
        (Some(theirs), Some(ours)) if !theirs.is_empty() && !ours.is_empty() => {
            return Err(
                "it was written in another boot of this host, or by another machine with the same name sharing this volume"
                    .to_string(),
            );
        }
        _ => {
            return Err("the boot of its run or of this host cannot be identified".to_string());
        }
    }
    #[cfg(target_os = "linux")]
    match (marker.pid_namespace.as_deref(), me.pid_namespace.as_deref()) {
        (Some(theirs), Some(ours)) if !theirs.is_empty() && theirs == ours => {}
        (Some(theirs), Some(ours)) if !theirs.is_empty() && !ours.is_empty() => {
            return Err(
                "it was written in another PID namespace (another container sharing this volume)"
                    .to_string(),
            );
        }
        _ => {
            return Err(
                "the PID namespace of its run or of this process cannot be identified".to_string(),
            );
        }
    }
    #[cfg(target_os = "macos")]
    if marker.pid_namespace.is_some() || me.pid_namespace.is_some() {
        return Err("it records a PID namespace, which this platform does not have".to_string());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = probe;
        return Err("this platform cannot prove that the run's owner has exited".to_string());
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if marker.pid == me.pid {
            return Err("it names this very process".to_string());
        }
        let pid = match i32::try_from(marker.pid) {
            Ok(pid) if pid > 0 => pid,
            _ => return Err(format!("its pid {} names no single process", marker.pid)),
        };
        match probe(pid) {
            Ok(()) => Err(format!(
                "its process {pid} is still running (or the pid has been reused)"
            )),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => Err(format!(
                "its process {pid} exists but cannot be checked (permission denied)"
            )),
            Err(error) => Err(format!("its process {pid} could not be checked ({error})")),
        }
    }
}

/// Test-only capture of the plain-text drop warning, in place of stderr.
#[cfg(test)]
pub(super) static DROP_WARNING_CAPTURE: std::sync::Mutex<Option<Vec<String>>> =
    std::sync::Mutex::new(None);

/// Write one line to stderr without panicking (this runs during unwinding).
fn write_drop_warning(line: &str) {
    #[cfg(test)]
    if let Ok(mut capture) = DROP_WARNING_CAPTURE.lock()
        && let Some(lines) = capture.as_mut()
    {
        lines.push(line.to_string());
        return;
    }
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Report a warning from work that outlives its caller (a background abort
/// of a cancelled metadata upload) through the run's warning sink: one
/// `cloud_sync.metadata.warning` NDJSON line under `--progress=json`,
/// otherwise a `warning:` line on stderr. Never panics.
pub(super) fn background_warning_report(
    sink: SpoolWarningSink,
) -> crate::utils::storage::remote::AbandonedUploadReport {
    std::sync::Arc::new(move |message: String| {
        crate::utils::output::record_warning_message(message.clone());
        match sink {
            SpoolWarningSink::NdjsonProgress => {
                super::JsonCloudSyncProgress::emit(super::metadata_warning_event(&message));
            }
            SpoolWarningSink::Stderr => write_drop_warning(&format!("warning: {message}")),
        }
    })
}

impl Drop for SnapshotSpool {
    /// Only reached without [`SnapshotSpool::finish`] (a panic or a cancelled
    /// future). A removal failure is still reported to the user, through
    /// calls that cannot panic again.
    fn drop(&mut self) {
        if let Some(note) = self.remove() {
            crate::utils::output::record_warning_message(note.clone());
            match self.warning_sink {
                SpoolWarningSink::NdjsonProgress => {
                    super::JsonCloudSyncProgress::emit(super::spool_warning_event(&note));
                }
                SpoolWarningSink::Stderr => write_drop_warning(&format!("warning: {note}")),
            }
        }
    }
}

/// Paged reader over the frozen identities.
/// Paged reader over the frozen identities. It yields exactly the number of
/// identities the freeze recorded: a shorter, longer or unterminated spool is
/// a read failure, never a silently smaller upload.
pub(super) struct FrozenObjectPages {
    reader: BufReader<File>,
    path: PathBuf,
    page_size: usize,
    remaining: u64,
}

impl FrozenObjectPages {
    pub(super) fn next_page(&mut self) -> CloudResult<Option<Vec<FrozenObject>>> {
        let mut page = Vec::with_capacity(self.page_size);
        let mut line = String::new();
        while page.len() < self.page_size && self.remaining > 0 {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|error| spool_read_failure(&self.path, error))?;
            if read == 0 {
                return Err(spool_read_failure(
                    &self.path,
                    format!(
                        "the spool ended {} identities short of the frozen count",
                        self.remaining
                    ),
                ));
            }
            let Some(record) = line.strip_suffix('\n') else {
                return Err(spool_read_failure(
                    &self.path,
                    "the last spool record is not newline-terminated",
                ));
            };
            let object: FrozenObject = serde_json::from_str(record)
                .map_err(|error| spool_read_failure(&self.path, error))?;
            page.push(object);
            self.remaining -= 1;
        }
        if self.remaining == 0 && page.is_empty() {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|error| spool_read_failure(&self.path, error))?;
            if read != 0 {
                return Err(spool_read_failure(
                    &self.path,
                    "the spool holds more identities than the freeze recorded",
                ));
            }
        }
        Ok((!page.is_empty()).then_some(page))
    }
}
