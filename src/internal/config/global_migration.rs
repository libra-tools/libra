//! One-time migration of the legacy global configuration database.
//!
//! plan-20260919 GCX-02 (ADR-GCX-02 / ADR-GCX-03): when the XDG-based
//! `<config dir>/libra/config.db` is missing while the legacy
//! `<home>/.libra/config.db` exists, the first global-configuration access
//! copies the legacy file into the new layout exactly once.
//!
//! Invariants the rest of the code base relies on:
//! - **GC-GCX-02** — the legacy file is opened read-only and is never renamed,
//!   truncated, deleted or written to, on the success path or on any failure
//!   path. The copy is produced by `VACUUM INTO`, which SQLite documents as
//!   read-only with respect to the source database.
//! - The new path is published by a single same-directory `rename`, so an
//!   interrupted migration can only leave a `config.db.migrate.*.tmp` behind —
//!   never a half-written `config.db`.
//! - The whole sequence is idempotent and serialized across processes by an
//!   advisory lock inside the target directory; a waiter that observes the
//!   published file simply adopts it instead of migrating a second time.
//! - No schema migration happens here (GC-13): the snapshot carries the legacy
//!   receipts verbatim, and no Repository-owned table is created or touched.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

use crate::internal::db::{DatabaseRole, schema, sqlite_schema_contains};

const ROLE: DatabaseRole = DatabaseRole::GlobalConfig;

/// Advisory lock serializing concurrent first-use migrations.
const LOCK_FILE_NAME: &str = ".config.db.migrate.lock";
/// Prefix of the staging snapshot. The suffix carries the pid plus a clock
/// nonce so two processes can never stage into the same file.
const TEMP_PREFIX: &str = "config.db.migrate.";
const TEMP_SUFFIX: &str = ".tmp";
/// `VACUUM INTO` was added in SQLite 3.27.0 (ADR-GCX-03 step 2).
const MIN_SQLITE_VERSION: (u32, u32, u32) = (3, 27, 0);
/// How long a non-holder waits for the lock holder before giving up. The
/// migration itself is a single file copy, so this is generous.
const LOCK_WAIT_BUDGET: Duration = Duration::from_secs(10);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);
/// Busy timeout for the two short-lived inspection connections.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// ADR-RB-02 classification of a global configuration directory (`config.redb`
/// vs unmigrated SQLite `config.db`), preserving `GC-13` isolation (never
/// creates or touches any Repository-owned table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GlobalConfigBackendState {
    /// `config.redb` is present and authoritative for this global config dir.
    RedbActive { redb_path: PathBuf },
    /// Only an unmigrated SQLite `config.db` exists (neither sentinel nor
    /// fenced tombstone).
    UnmigratedSqlite { sqlite_path: PathBuf },
    /// Neither an active `config.redb` nor an active SQLite `config.db` exists.
    FreshEmpty,
}

/// Classify a global configuration directory per `ADR-RB-02` without opening
/// or mutating any database.
pub(crate) fn classify_global_config_dir(dir: &Path) -> Result<GlobalConfigBackendState, String> {
    let redb_path = dir.join(crate::storage::config_store::CONFIG_REDB_FILE_NAME);
    if redb_path.exists() {
        return Ok(GlobalConfigBackendState::RedbActive { redb_path });
    }
    let sqlite_path = dir.join("config.db");
    if sqlite_path.exists() {
        let is_tombstone =
            crate::storage::config_store::is_config_sqlite_tombstone_sentinel(&sqlite_path)
                .map_err(|err| {
                    format!(
                        "cannot inspect '{}' for tombstone sentinel: {err}",
                        sqlite_path.display()
                    )
                })?
                || crate::storage::config_store::is_config_sqlite_fenced_tombstone(&sqlite_path)
                    .map_err(|err| {
                        format!(
                            "cannot inspect '{}' for fenced tombstone: {err}",
                            sqlite_path.display()
                        )
                    })?;
        if !is_tombstone {
            return Ok(GlobalConfigBackendState::UnmigratedSqlite { sqlite_path });
        }
    }
    Ok(GlobalConfigBackendState::FreshEmpty)
}

fn is_tombstone_config_db(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    Ok(
        crate::storage::config_store::is_config_sqlite_tombstone_sentinel(path).map_err(|err| {
            format!(
                "cannot inspect '{}' for tombstone sentinel: {err}",
                path.display()
            )
        })? || crate::storage::config_store::is_config_sqlite_fenced_tombstone(path).map_err(
            |err| {
                format!(
                    "cannot inspect '{}' for fenced tombstone: {err}",
                    path.display()
                )
            },
        )?,
    )
}

/// What the one-time migration did for this call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GlobalConfigMigration {
    /// Nothing was pending (no legacy file, or the new path already exists).
    NotNeeded,
    /// This call published `<config dir>/libra/config.db`.
    Migrated,
    /// Another process published the new path while this one waited.
    AlreadyPresent,
    /// The migration could not run. The legacy database stays authoritative;
    /// the caller decides whether that is tolerable (reads) or not (writes).
    Failed(String),
}

/// Copy `legacy` to `target` exactly once.
///
/// Never returns an error type: a migration failure is a *state*, because a
/// read may legitimately continue against the untouched legacy database.
pub(crate) async fn migrate_legacy_global_config(
    legacy: &Path,
    target: &Path,
) -> GlobalConfigMigration {
    match migrate(legacy, target).await {
        Ok(outcome) => outcome,
        Err(reason) => GlobalConfigMigration::Failed(reason),
    }
}

async fn migrate(legacy: &Path, target: &Path) -> Result<GlobalConfigMigration, String> {
    let Some(dir) = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Err(format!(
            "'{}' has no parent directory to migrate into",
            target.display()
        ));
    };

    // ADR-RB-02: if `config.redb` is already active in `dir`, never overwrite
    // or run SQLite legacy copy alongside it (GC-13 isolation).
    if matches!(
        classify_global_config_dir(dir)?,
        GlobalConfigBackendState::RedbActive { .. }
    ) {
        return Ok(GlobalConfigMigration::AlreadyPresent);
    }
    // Never migrate a SQLite tombstone sentinel or fenced tombstone.
    if is_tombstone_config_db(legacy)? {
        return Ok(GlobalConfigMigration::NotNeeded);
    }

    create_config_dir(dir)?;

    let lock = match acquire_migration_lock(dir, target).await? {
        Some(lock) => lock,
        None if target.exists() => return Ok(GlobalConfigMigration::AlreadyPresent),
        None => {
            return Err(format!(
                "another process is still migrating into '{}'",
                dir.display()
            ));
        }
    };

    // Re-probe under the lock: a holder that finished while we waited has
    // already published the file, and re-copying would overwrite it.
    if matches!(
        classify_global_config_dir(dir)?,
        GlobalConfigBackendState::RedbActive { .. }
    ) || target.exists()
    {
        drop(lock);
        return Ok(GlobalConfigMigration::AlreadyPresent);
    }
    if !legacy.exists() || is_tombstone_config_db(legacy)? {
        drop(lock);
        return Ok(GlobalConfigMigration::NotNeeded);
    }

    let backend = crate::storage::StorageBackendKind::Sqlite;
    match backend {
        crate::storage::StorageBackendKind::Redb => Ok(GlobalConfigMigration::AlreadyPresent),
        crate::storage::StorageBackendKind::Sqlite => {
            let staging = Staging::create(dir)?;
            let source = snapshot_legacy(legacy, staging.path()).await?;
            validate_snapshot(staging.path(), &source).await?;
            publish(staging.path(), target).map_err(|error| {
                format!(
                    "cannot publish the migrated database at '{}': {error}",
                    target.display()
                )
            })?;
            staging.published();
            drop(lock);
            Ok(GlobalConfigMigration::Migrated)
        }
    }
}

/// Create the configuration directory, restricting it to the owner on Unix.
fn create_config_dir(dir: &Path) -> Result<(), String> {
    let existed = dir.is_dir();
    fs::create_dir_all(dir).map_err(|error| {
        format!(
            "cannot create the configuration directory '{}': {error}",
            dir.display()
        )
    })?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        // Best effort: an inherited directory keeps whatever mode the user
        // chose, and a failure here must not abort a migration that is
        // otherwise sound.
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = existed;
    Ok(())
}

// ---------------------------------------------------------------------------
// Snapshot, validation and publication
// ---------------------------------------------------------------------------

/// The properties of a configuration database that the copy must reproduce.
///
/// `None` means the table (or view) is absent, which is itself part of the
/// fingerprint: a pre-ledger legacy file must stay pre-ledger after the copy.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    ledger: Option<Vec<(i64, String)>>,
    kv_rows: Option<i64>,
}

impl Fingerprint {
    /// A file with neither the configuration ledger nor `config_kv` is not a
    /// Libra configuration database and must not be adopted as one.
    fn is_configuration_database(&self) -> bool {
        self.ledger.is_some() || self.kv_rows.is_some()
    }
}

fn detect_config_path_backend(path: &Path) -> Result<crate::storage::StorageBackendKind, String> {
    use crate::storage::{
        StorageBackendKind,
        config_store::{
            CONFIG_REDB_FILE_NAME, ConfigScopeAuthority, inspect_config_scope_authority,
        },
    };
    if path
        .file_name()
        .is_some_and(|name| name == CONFIG_REDB_FILE_NAME)
    {
        return Ok(StorageBackendKind::Redb);
    }
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        match inspect_config_scope_authority(dir, ROLE) {
            Ok(ConfigScopeAuthority::Manifest { manifest, .. }) => return Ok(manifest.backend),
            Ok(ConfigScopeAuthority::UnmanifestedRedb { .. }) => {
                return Ok(StorageBackendKind::Redb);
            }
            Ok(ConfigScopeAuthority::LegacySqlite { .. } | ConfigScopeAuthority::Empty)
            | Err(_) => {}
        }
    }
    Ok(StorageBackendKind::Sqlite)
}

/// `VACUUM INTO` the read-only legacy database, returning its fingerprint.
async fn snapshot_legacy(legacy: &Path, staging: &Path) -> Result<Fingerprint, String> {
    match detect_config_path_backend(legacy)? {
        crate::storage::StorageBackendKind::Redb => Err(format!(
            "'{}' is backed by redb storage and does not use legacy SQLite global migration",
            legacy.display()
        )),
        crate::storage::StorageBackendKind::Sqlite => {
            let conn = schema::open_readonly_connection_for_role(legacy, BUSY_TIMEOUT, ROLE)
                .await
                .map_err(|error| {
                    format!(
                        "cannot read the legacy global configuration database '{}': {error}",
                        legacy.display()
                    )
                })?;
            let result = snapshot_legacy_with_conn(&conn, legacy, staging).await;
            close_connection(conn).await;
            result
        }
    }
}

async fn snapshot_legacy_with_conn(
    conn: &DatabaseConnection,
    legacy: &Path,
    staging: &Path,
) -> Result<Fingerprint, String> {
    let version = sqlite_version(conn).await?;
    if version < MIN_SQLITE_VERSION {
        return Err(format!(
            "SQLite {}.{}.{} cannot copy a database consistently; Libra needs {}.{}.{} or newer",
            version.0,
            version.1,
            version.2,
            MIN_SQLITE_VERSION.0,
            MIN_SQLITE_VERSION.1,
            MIN_SQLITE_VERSION.2
        ));
    }

    let fingerprint = fingerprint(conn).await?;
    if !fingerprint.is_configuration_database() {
        return Err(format!(
            "'{}' is not a Libra configuration database",
            legacy.display()
        ));
    }

    let literal = sql_string_literal(staging)?;
    conn.execute_unprepared(&format!("VACUUM INTO {literal}"))
        .await
        .map_err(|error| {
            format!(
                "cannot snapshot '{}' into '{}': {error}",
                legacy.display(),
                staging.display()
            )
        })?;
    Ok(fingerprint)
}

/// Prove the staged copy is intact and carries exactly the source's contents.
async fn validate_snapshot(staging: &Path, source: &Fingerprint) -> Result<(), String> {
    match detect_config_path_backend(staging)? {
        crate::storage::StorageBackendKind::Redb => Err(format!(
            "'{}' is in a redb-backed configuration scope and cannot be validated as a legacy SQLite snapshot",
            staging.display()
        )),
        crate::storage::StorageBackendKind::Sqlite => {
            let conn = schema::open_readonly_connection_for_role(staging, BUSY_TIMEOUT, ROLE)
                .await
                .map_err(|error| format!("cannot verify the migrated snapshot: {error}"))?;
            let result = validate_snapshot_with_conn(&conn, source).await;
            // The handle MUST be gone before the rename: Windows refuses to move a
            // file that still has an open handle.
            close_connection(conn).await;
            result
        }
    }
}

async fn validate_snapshot_with_conn(
    conn: &DatabaseConnection,
    source: &Fingerprint,
) -> Result<(), String> {
    let report = integrity_check(conn).await?;
    if report != "ok" {
        return Err(format!(
            "the migrated snapshot failed SQLite's integrity check ({report})"
        ));
    }
    let copy = fingerprint(conn).await?;
    if copy.ledger != source.ledger {
        return Err(
            "the migrated snapshot does not carry the legacy migration receipts verbatim"
                .to_string(),
        );
    }
    if copy.kv_rows != source.kv_rows {
        return Err(format!(
            "the migrated snapshot holds {} configuration entries instead of {}",
            describe_rows(copy.kv_rows),
            describe_rows(source.kv_rows)
        ));
    }
    Ok(())
}

fn describe_rows(rows: Option<i64>) -> String {
    rows.map_or_else(|| "no config_kv table".to_string(), |rows| rows.to_string())
}

/// Atomically adopt the staged copy, then tighten its mode.
fn publish(staging: &Path, target: &Path) -> io::Result<()> {
    fs::rename(staging, target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(target, fs::Permissions::from_mode(0o600))?;
        // Durability of the directory entry itself: without this the rename
        // can be lost by a crash even though the file contents survived.
        if let Some(dir) = target.parent()
            && let Ok(handle) = fs::File::open(dir)
        {
            let _ = handle.sync_all();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SQLite helpers
// ---------------------------------------------------------------------------

async fn fingerprint(conn: &DatabaseConnection) -> Result<Fingerprint, String> {
    Ok(Fingerprint {
        ledger: if relation_exists(conn, "configuration_schema_versions").await? {
            Some(ledger_rows(conn).await?)
        } else {
            None
        },
        kv_rows: if relation_exists(conn, "config_kv").await? {
            Some(count_rows(conn, "config_kv").await?)
        } else {
            None
        },
    })
}

/// `config_kv` is a table in current databases and a view in the pre-ledger
/// compatibility layout, so both spellings count as present.
async fn relation_exists(conn: &DatabaseConnection, name: &str) -> Result<bool, String> {
    for kind in ["table", "view"] {
        if sqlite_schema_contains(conn, kind, name)
            .await
            .map_err(|error| format!("cannot inspect the '{name}' schema entry: {error}"))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn ledger_rows(conn: &DatabaseConnection) -> Result<Vec<(i64, String)>, String> {
    let rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT version, name FROM configuration_schema_versions ORDER BY version",
        ))
        .await
        .map_err(|error| format!("cannot read the configuration migration receipts: {error}"))?;
    rows.into_iter()
        .map(|row| {
            let version = row
                .try_get::<i64>("", "version")
                .map_err(|error| format!("cannot read a migration receipt version: {error}"))?;
            let name = row
                .try_get::<String>("", "name")
                .map_err(|error| format!("cannot read a migration receipt name: {error}"))?;
            Ok((version, name))
        })
        .collect()
}

async fn count_rows(conn: &DatabaseConnection, relation: &str) -> Result<i64, String> {
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("SELECT COUNT(*) AS row_count FROM {relation}"),
        ))
        .await
        .map_err(|error| format!("cannot count the '{relation}' rows: {error}"))?
        .ok_or_else(|| format!("counting the '{relation}' rows returned no row"))?;
    row.try_get::<i64>("", "row_count")
        .map_err(|error| format!("cannot read the '{relation}' row count: {error}"))
}

async fn integrity_check(conn: &DatabaseConnection) -> Result<String, String> {
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA integrity_check",
        ))
        .await
        .map_err(|error| format!("cannot run the snapshot integrity check: {error}"))?
        .ok_or_else(|| "the snapshot integrity check returned no row".to_string())?;
    row.try_get::<String>("", "integrity_check")
        .map_err(|error| format!("cannot read the snapshot integrity check result: {error}"))
}

async fn sqlite_version(conn: &DatabaseConnection) -> Result<(u32, u32, u32), String> {
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT sqlite_version() AS version",
        ))
        .await
        .map_err(|error| format!("cannot read the SQLite version: {error}"))?
        .ok_or_else(|| "reading the SQLite version returned no row".to_string())?;
    let raw = row
        .try_get::<String>("", "version")
        .map_err(|error| format!("cannot read the SQLite version: {error}"))?;
    parse_sqlite_version(&raw).ok_or_else(|| format!("unrecognized SQLite version '{raw}'"))
}

fn parse_sqlite_version(raw: &str) -> Option<(u32, u32, u32)> {
    let mut parts = raw.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let patch = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
    Some((major, minor, patch))
}

/// Quote a path as a SQL string literal. `VACUUM INTO` takes an expression, so
/// the destination cannot be bound as a parameter.
fn sql_string_literal(path: &Path) -> Result<String, String> {
    let absolute = std::path::absolute(path)
        .map_err(|error| format!("cannot resolve '{}': {error}", path.display()))?;
    let text = absolute.to_str().ok_or_else(|| {
        format!(
            "the configuration directory path '{}' is not valid UTF-8; use a UTF-8 path",
            absolute.display()
        )
    })?;
    Ok(format!("'{}'", text.replace('\'', "''")))
}

/// Close a pool explicitly so its file handles are gone before the rename.
async fn close_connection(conn: DatabaseConnection) {
    if let Err(error) = conn.close().await {
        tracing::debug!(%error, "failed to close a global config migration handle");
    }
}

// ---------------------------------------------------------------------------
// Staging file
// ---------------------------------------------------------------------------

/// Deletes its file on drop unless the migration published it, so no failure
/// path can leave a partial snapshot behind.
struct Staging {
    path: PathBuf,
    published: bool,
}

impl Staging {
    fn create(dir: &Path) -> Result<Self, String> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or_default();
        let path = dir.join(format!(
            "{TEMP_PREFIX}{}.{nonce:08x}{TEMP_SUFFIX}",
            std::process::id()
        ));
        // `VACUUM INTO` refuses to overwrite an existing file, so a leftover
        // from a crashed run is removed first.
        if path.exists() {
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "cannot remove the stale snapshot '{}': {error}",
                    path.display()
                )
            })?;
        }
        Ok(Self {
            path,
            published: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn published(mut self) {
        self.published = true;
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-process lock
// ---------------------------------------------------------------------------

/// Held for the duration of one migration. Dropping it releases the lock.
struct MigrationLock {
    #[allow(dead_code)]
    file: fs::File,
    #[cfg(all(not(unix), not(windows)))]
    path: PathBuf,
}

/// Wait for the migration lock, adopting a concurrently published file instead.
///
/// `Ok(None)` means this process must not migrate: either the target appeared
/// while waiting, or the holder outlived the wait budget. The caller
/// distinguishes the two by probing the target once more.
async fn acquire_migration_lock(
    dir: &Path,
    target: &Path,
) -> Result<Option<MigrationLock>, String> {
    let lock_path = dir.join(LOCK_FILE_NAME);
    let deadline = Instant::now() + LOCK_WAIT_BUDGET;
    loop {
        if target.exists() {
            return Ok(None);
        }
        if let Some(lock) = try_acquire_migration_lock(&lock_path)? {
            return Ok(Some(lock));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(LOCK_RETRY_INTERVAL).await;
    }
}

#[cfg(unix)]
fn try_acquire_migration_lock(path: &Path) -> Result<Option<MigrationLock>, String> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};

    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| lock_open_error(path, error))?;
    // SAFETY: `flock` operates on a descriptor owned by `file`, which outlives
    // the call. The kernel releases the lock when the descriptor closes, so a
    // crashed holder cannot wedge the migration.
    let acquired = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if acquired == 0 {
        return Ok(Some(MigrationLock { file }));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
        _ => Err(format!(
            "cannot lock '{}' for the configuration migration: {error}",
            path.display()
        )),
    }
}

#[cfg(windows)]
fn try_acquire_migration_lock(path: &Path) -> Result<Option<MigrationLock>, String> {
    use std::os::windows::fs::OpenOptionsExt;

    match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        // A zero share mode is released by the kernel on process death and is
        // Windows' equivalent of the advisory lock used on Unix.
        .share_mode(0)
        .open(path)
    {
        Ok(file) => Ok(Some(MigrationLock { file })),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(None),
        Err(error) => Err(lock_open_error(path, error)),
    }
}

/// Platforms without a kernel-released lock fall back to an exclusive create
/// plus a stale-age reclaim, which is weaker but still serializes the common
/// case (ADR-GCX-03 step 1).
#[cfg(all(not(unix), not(windows)))]
fn try_acquire_migration_lock(path: &Path) -> Result<Option<MigrationLock>, String> {
    const STALE_LOCK_AGE: Duration = Duration::from_secs(300);

    match fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
    {
        Ok(file) => Ok(Some(MigrationLock {
            file,
            path: path.to_path_buf(),
        })),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let stale = fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .map(|modified| {
                    modified
                        .elapsed()
                        .is_ok_and(|elapsed| elapsed > STALE_LOCK_AGE)
                })
                .unwrap_or(false);
            if stale {
                let _ = fs::remove_file(path);
            }
            Ok(None)
        }
        Err(error) => Err(lock_open_error(path, error)),
    }
}

#[cfg(all(not(unix), not(windows)))]
impl Drop for MigrationLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_open_error(path: &Path, error: io::Error) -> String {
    format!(
        "cannot open '{}' for the configuration migration lock: {error}",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a real global-configuration database so the migration exercises
    /// the production schema, receipts included, rather than a hand-rolled file.
    async fn seed_configuration_database(path: &Path, entries: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture directory");
        }
        let conn = schema::create_configuration_database(path, ROLE)
            .await
            .expect("create fixture configuration database");
        for index in 0..entries {
            conn.execute_unprepared(&format!(
                "INSERT INTO config_kv (`key`, `value`, `encrypted`) \
                 VALUES ('fixture.key{index}', 'value{index}', 0)"
            ))
            .await
            .expect("seed fixture entry");
        }
        conn.close().await.expect("close fixture connection");
    }

    async fn read_fingerprint(path: &Path) -> Fingerprint {
        let conn = schema::open_readonly_connection_for_role(path, BUSY_TIMEOUT, ROLE)
            .await
            .expect("open fixture read-only");
        let observed = fingerprint(&conn).await.expect("fingerprint fixture");
        close_connection(conn).await;
        observed
    }

    fn file_digest(path: &Path) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(fs::read(path).expect("read file"));
        hasher.finalize().to_vec()
    }

    /// The happy path: the copy carries the legacy receipts and entries, and
    /// the legacy file is byte-for-byte untouched (GC-GCX-02).
    #[tokio::test]
    async fn migration_copies_the_legacy_database_and_never_touches_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let legacy = root.path().join(".libra").join("config.db");
        let target = root.path().join(".config").join("libra").join("config.db");
        seed_configuration_database(&legacy, 3).await;
        let before = file_digest(&legacy);
        let source = read_fingerprint(&legacy).await;

        let outcome = migrate_legacy_global_config(&legacy, &target).await;

        assert_eq!(outcome, GlobalConfigMigration::Migrated);
        assert!(target.exists(), "the migrated database must be published");
        assert_eq!(read_fingerprint(&target).await, source);
        assert_eq!(source.kv_rows, Some(3));
        assert_eq!(file_digest(&legacy), before, "legacy file must not change");
    }

    /// A second call adopts the published file instead of copying again, and no
    /// staging file survives either run.
    #[tokio::test]
    async fn migration_is_idempotent_and_leaves_no_staging_file() {
        let root = tempfile::tempdir().expect("tempdir");
        let legacy = root.path().join(".libra").join("config.db");
        let dir = root.path().join(".config").join("libra");
        let target = dir.join("config.db");
        seed_configuration_database(&legacy, 1).await;

        assert_eq!(
            migrate_legacy_global_config(&legacy, &target).await,
            GlobalConfigMigration::Migrated
        );
        let published = file_digest(&target);
        assert_eq!(
            migrate_legacy_global_config(&legacy, &target).await,
            GlobalConfigMigration::AlreadyPresent
        );
        assert_eq!(file_digest(&target), published);

        let staging: Vec<_> = fs::read_dir(&dir)
            .expect("read config dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(TEMP_PREFIX))
            .collect();
        assert!(staging.is_empty(), "staging files left behind: {staging:?}");
    }

    /// A file that is not a configuration database is never adopted as one.
    #[tokio::test]
    async fn migration_refuses_a_file_that_is_not_a_configuration_database() {
        let root = tempfile::tempdir().expect("tempdir");
        let legacy = root.path().join(".libra").join("config.db");
        let target = root.path().join(".config").join("libra").join("config.db");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("create legacy dir");
        fs::write(&legacy, b"this is not a sqlite database").expect("write legacy");

        let outcome = migrate_legacy_global_config(&legacy, &target).await;

        assert!(
            matches!(outcome, GlobalConfigMigration::Failed(_)),
            "{outcome:?}"
        );
        assert!(
            !target.exists(),
            "a rejected source must not publish a target"
        );
        assert_eq!(
            fs::read(&legacy).expect("read legacy"),
            b"this is not a sqlite database"
        );
    }

    /// A SQLite database that carries neither the configuration ledger nor
    /// `config_kv` is a foreign file, not an empty configuration.
    #[tokio::test]
    async fn migration_refuses_a_foreign_sqlite_database() {
        let root = tempfile::tempdir().expect("tempdir");
        let legacy = root.path().join(".libra").join("config.db");
        let target = root.path().join(".config").join("libra").join("config.db");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("create legacy dir");
        // An empty file is a valid (zero-page) SQLite database; the connector
        // deliberately refuses to create one itself.
        fs::write(&legacy, b"").expect("create empty database file");
        let conn = crate::internal::db::open_connection_without_schema_management(
            legacy.to_str().expect("utf-8 fixture path"),
            BUSY_TIMEOUT,
        )
        .await
        .expect("create foreign database");
        conn.execute_unprepared("CREATE TABLE unrelated (id INTEGER PRIMARY KEY)")
            .await
            .expect("create foreign table");
        close_connection(conn).await;

        let outcome = migrate_legacy_global_config(&legacy, &target).await;

        assert!(
            matches!(outcome, GlobalConfigMigration::Failed(_)),
            "{outcome:?}"
        );
        assert!(!target.exists());
    }

    /// Failure injection: an unwritable configuration directory reports a
    /// failure instead of panicking, and leaves the legacy file in place.
    #[cfg(unix)]
    #[tokio::test]
    async fn migration_reports_an_unwritable_configuration_directory() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("tempdir");
        let legacy = root.path().join(".libra").join("config.db");
        let config_home = root.path().join(".config");
        let target = config_home.join("libra").join("config.db");
        seed_configuration_database(&legacy, 1).await;
        fs::create_dir_all(&config_home).expect("create config home");
        let before = file_digest(&legacy);
        fs::set_permissions(&config_home, fs::Permissions::from_mode(0o500))
            .expect("make the config home read-only");

        let outcome = migrate_legacy_global_config(&legacy, &target).await;

        // Restore first so the temp dir can always be cleaned up.
        fs::set_permissions(&config_home, fs::Permissions::from_mode(0o700))
            .expect("restore the config home");
        let GlobalConfigMigration::Failed(reason) = outcome else {
            panic!("expected a failure, got {outcome:?}");
        };
        assert!(
            reason.contains("cannot create the configuration directory"),
            "unexpected reason: {reason}"
        );
        assert!(!target.exists());
        assert_eq!(file_digest(&legacy), before);
    }

    /// The staging guard is what keeps a failed migration from leaving a
    /// partial snapshot next to the real database.
    #[tokio::test]
    async fn staging_is_removed_unless_the_migration_published_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("libra");
        fs::create_dir_all(&dir).expect("create dir");

        let abandoned = Staging::create(&dir).expect("create staging");
        fs::write(abandoned.path(), b"partial").expect("write partial snapshot");
        let abandoned_path = abandoned.path().to_path_buf();
        assert!(
            abandoned_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(TEMP_PREFIX) && name.ends_with(TEMP_SUFFIX))
        );
        drop(abandoned);
        assert!(
            !abandoned_path.exists(),
            "an unpublished snapshot is removed"
        );

        let kept = Staging::create(&dir).expect("create staging");
        fs::write(kept.path(), b"complete").expect("write snapshot");
        let kept_path = kept.path().to_path_buf();
        kept.published();
        assert!(
            kept_path.exists(),
            "a published snapshot survives the guard"
        );
    }

    #[test]
    fn parse_sqlite_version_accepts_partial_and_rejects_garbage() {
        assert_eq!(parse_sqlite_version("3.27.0"), Some((3, 27, 0)));
        assert_eq!(parse_sqlite_version("3.53.4"), Some((3, 53, 4)));
        assert_eq!(parse_sqlite_version("3.45"), Some((3, 45, 0)));
        assert_eq!(parse_sqlite_version("3"), Some((3, 0, 0)));
        assert_eq!(parse_sqlite_version("not-a-version"), None);
        assert!((3, 26, 9) < MIN_SQLITE_VERSION);
        assert!((3, 27, 0) >= MIN_SQLITE_VERSION);
    }

    #[test]
    fn sql_string_literal_quotes_and_escapes_the_destination() {
        let root = tempfile::tempdir().expect("tempdir");
        let quoted = root.path().join("o'brien").join("config.db.tmp");
        let literal = sql_string_literal(&quoted).expect("literal");
        assert!(literal.starts_with('\'') && literal.ends_with('\''));
        assert!(literal.contains("o''brien"), "unescaped quote in {literal}");
    }
}
