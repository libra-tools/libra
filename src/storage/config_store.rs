//! Object-safe [`ConfigStore`] and [`MetadataStore`] storage traits and shared
//! domain records (`ADR-RB-04`, `RB-03`).
//!
//! Covers the 4 repository configuration/metadata tables (`schema_versions`,
//! `config`, `config_kv`, `metadata_kv`) and the 3 configuration-database
//! source tables (`config`, `config_kv`, `configuration_schema_versions`).

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    internal::db::schema::DatabaseRole,
    storage::{
        backend::StorageBackendKind,
        error::{StorageBackendError, StorageResult},
        manifest::{Phase2DbState, fsync_dir},
    },
};

/// Environment variable overriding the global configuration database path (`GC-13`).
pub const LIBRA_CONFIG_GLOBAL_DB_ENV: &str = "LIBRA_CONFIG_GLOBAL_DB";

/// Environment variable overriding the system configuration database path (`GC-13`).
pub const LIBRA_CONFIG_SYSTEM_DB_ENV: &str = "LIBRA_CONFIG_SYSTEM_DB";

/// Canonical `redb` configuration filename inside a `GlobalConfig` or `SystemConfig` directory.
pub const CONFIG_REDB_FILE_NAME: &str = "config.redb";

/// Canonical SQLite configuration filename inside a `GlobalConfig` or `SystemConfig` directory.
pub const CONFIG_SQLITE_FILE_NAME: &str = "config.db";

/// Canonical two-phase scope format manifest filename (`ADR-RB-02` #2).
pub const CONFIG_FORMAT_MANIFEST_FILE: &str = "config-format.json";

/// Current JSON schema version for [`ConfigFormatManifest`].
pub const CONFIG_FORMAT_MANIFEST_VERSION: u32 = 1;

/// Canonical cross-runtime coordination lock filename inside a config scope directory (`ADR-RB-02` #2).
pub const CONFIG_MIGRATION_LOCK_FILE: &str = "config-migration.lock";

/// Default bounded wait deadline for runtime [`ConfigScopeLock`] acquisition (`2s`, `ADR-RB-02` #2).
pub const DEFAULT_CONFIG_SCOPE_LOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// Fixed 31-byte non-SQLite sentinel written into `config.db` when `redb` owns a
/// `GlobalConfig` or `SystemConfig` scope (`ADR-RB-02` #2 Step F1).
pub const CONFIG_SQLITE_TOMBSTONE_SENTINEL: &[u8] = b"LIBRA_CONFIG_AUTHORITY_IS_REDB\n";

/// SQLite `sqlite_master` table name installed by forward migration rollback-journal
/// fencing (`ADR-RB-07` #1).
pub const SQLITE_MIGRATED_FENCE_TABLE: &str = "__libra_migrated_fence__";

/// Malformed SQL string written to `sqlite_master.sql` for [`SQLITE_MIGRATED_FENCE_TABLE`]
/// so any legacy SQLite connection fails closed with `malformed database schema`.
pub const SQLITE_MIGRATED_FENCE_SQL: &str = "LIBRA_MIGRATED_TO_REDB_FENCE_SYNTAX_ERROR";

/// Required Unix permission mode for `GlobalConfig` `config.db` tombstone (`0400`).
#[cfg(unix)]
pub const GLOBAL_CONFIG_TOMBSTONE_MODE: u32 = 0o400;

/// Required Unix permission mode for `SystemConfig` `config.db` tombstone (`0444`).
#[cfg(unix)]
pub const SYSTEM_CONFIG_TOMBSTONE_MODE: u32 = 0o444;

/// Return the exact Unix POSIX mode bits required for a `config.db` tombstone
/// file belonging to `role` (`0400` Global, `0444` System).
#[cfg(unix)]
#[must_use]
pub const fn expected_config_tombstone_mode(role: DatabaseRole) -> Option<u32> {
    match role {
        DatabaseRole::GlobalConfig => Some(GLOBAL_CONFIG_TOMBSTONE_MODE),
        DatabaseRole::SystemConfig => Some(SYSTEM_CONFIG_TOMBSTONE_MODE),
        DatabaseRole::Repository | DatabaseRole::Capture | DatabaseRole::Derived => None,
    }
}

/// Defensive upper bound on `config-format.json` size to prevent unbounded allocation.
const MAX_CONFIG_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Maximum retries for unique atomic temp filename creation during manifest publish.
const MAX_CONFIG_MANIFEST_TEMP_ATTEMPTS: usize = 64;

static CONFIG_MANIFEST_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Required Unix permission mode for repository `redb` configuration/git stores (`0600`).
#[cfg(unix)]
pub const REPOSITORY_CONFIG_FILE_MODE: u32 = 0o600;

/// Required Unix permission mode for `GlobalConfig` `config.redb` (`0600`, private to owner).
#[cfg(unix)]
pub const GLOBAL_CONFIG_FILE_MODE: u32 = 0o600;

/// Required Unix permission mode for `SystemConfig` `config.redb` (`0644`, world-readable).
#[cfg(unix)]
pub const SYSTEM_CONFIG_FILE_MODE: u32 = 0o644;

/// Return the exact Unix POSIX mode bits required for a configuration-bearing
/// [`DatabaseRole`], or `None` for non-config roles (`GC-13`, `RB-03j`).
#[cfg(unix)]
#[must_use]
pub const fn expected_config_file_mode(role: DatabaseRole) -> Option<u32> {
    match role {
        DatabaseRole::Repository => Some(REPOSITORY_CONFIG_FILE_MODE),
        DatabaseRole::GlobalConfig => Some(GLOBAL_CONFIG_FILE_MODE),
        DatabaseRole::SystemConfig => Some(SYSTEM_CONFIG_FILE_MODE),
        DatabaseRole::Capture | DatabaseRole::Derived => None,
    }
}

/// Extract the parent scope directory from a global/system configuration DB
/// path (such as `LIBRA_CONFIG_GLOBAL_DB` or `LIBRA_CONFIG_SYSTEM_DB`).
pub fn config_scope_dir_from_db_path(db_path: &Path) -> StorageResult<PathBuf> {
    let parent = db_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "configuration database path must include a parent scope directory".to_string(),
        })?;
    Ok(parent.to_path_buf())
}

/// Return `<scope_dir>/config.redb`.
#[must_use]
pub fn config_redb_path_in_scope_dir(scope_dir: &Path) -> PathBuf {
    scope_dir.join(CONFIG_REDB_FILE_NAME)
}

/// Return `<scope_dir>/config.db`.
#[must_use]
pub fn config_sqlite_path_in_scope_dir(scope_dir: &Path) -> PathBuf {
    scope_dir.join(CONFIG_SQLITE_FILE_NAME)
}

/// Return `<scope_dir>/config-format.json`.
#[must_use]
pub fn config_format_manifest_path(scope_dir: &Path) -> PathBuf {
    scope_dir.join(CONFIG_FORMAT_MANIFEST_FILE)
}

/// Return `<scope_dir>/config-migration.lock`.
#[must_use]
pub fn config_migration_lock_path(scope_dir: &Path) -> PathBuf {
    scope_dir.join(CONFIG_MIGRATION_LOCK_FILE)
}

/// Validate that `role` is a scoped external configuration role
/// (`GlobalConfig` or `SystemConfig`).
pub(crate) fn validate_config_scope_role(role: DatabaseRole) -> StorageResult<()> {
    match role {
        DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig => Ok(()),
        other => Err(StorageBackendError::BackendInternal {
            detail: format!("{other} role does not use a scoped config-format.json manifest"),
        }),
    }
}

const fn default_config_manifest_schema_version() -> u32 {
    CONFIG_FORMAT_MANIFEST_VERSION
}

const fn default_config_generation() -> u64 {
    1
}

/// Initialization/migration phase recorded in `config-format.json` while
/// `finalized == false` (`ADR-RB-02` #2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigInitPhase {
    /// Fresh `redb` config database 4-step initialization (`Step F1..F4`).
    FreshCreate,
    /// Two-phase `SQLite <-> redb` config scope migration.
    Migration,
}

/// Authoritative two-phase scope manifest stored at `<scope_dir>/config-format.json`
/// for [`DatabaseRole::GlobalConfig`] and [`DatabaseRole::SystemConfig`] (`ADR-RB-02` #2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFormatManifest {
    /// Manifest format version (currently [`CONFIG_FORMAT_MANIFEST_VERSION`] = `1`).
    #[serde(default = "default_config_manifest_schema_version")]
    pub schema_version: u32,
    /// Authoritative storage backend for this config scope (`sqlite` or `redb`).
    pub backend: StorageBackendKind,
    /// `false` during fresh creation or migration cutover; `true` once finalized.
    pub finalized: bool,
    /// Active unfinalized phase (`fresh_create` or `migration`), `None` once finalized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_phase: Option<ConfigInitPhase>,
    /// Monotonically increasing scope generation (`>= 1`).
    #[serde(default = "default_config_generation")]
    pub generation: u64,
    /// `true` when `config.db` in this scope directory is an intentional read-only
    /// tombstone/fence guarding a `redb` authority against legacy SQLite binaries.
    #[serde(default)]
    pub legacy_sqlite_tombstone: bool,
    /// `true` once staged target database bytes are ready for atomic rename publish.
    #[serde(default)]
    pub targets_staged_for_rename: bool,
    /// Source database cutover progress during two-phase migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_db_state: Option<Phase2DbState>,
    /// SHA-256 of verified migration backup archive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_sha256: Option<String>,
    /// Canonical logical SHA-256 of the source config database captured before cutover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_logical_sha256: Option<String>,
}

impl ConfigFormatManifest {
    /// Construct a finalized `sqlite` scope manifest (`legacy_sqlite_tombstone = false`).
    pub fn new_sqlite_finalized(generation: u64, role: DatabaseRole) -> StorageResult<Self> {
        let manifest = Self {
            schema_version: CONFIG_FORMAT_MANIFEST_VERSION,
            backend: StorageBackendKind::Sqlite,
            finalized: true,
            init_phase: None,
            generation,
            legacy_sqlite_tombstone: false,
            targets_staged_for_rename: false,
            source_db_state: None,
            backup_sha256: None,
            source_logical_sha256: None,
        };
        manifest.validate(role)?;
        Ok(manifest)
    }

    /// Construct a finalized `redb` scope manifest.
    pub fn new_redb_finalized(
        generation: u64,
        legacy_sqlite_tombstone: bool,
        role: DatabaseRole,
    ) -> StorageResult<Self> {
        let manifest = Self {
            schema_version: CONFIG_FORMAT_MANIFEST_VERSION,
            backend: StorageBackendKind::Redb,
            finalized: true,
            init_phase: None,
            generation,
            legacy_sqlite_tombstone,
            targets_staged_for_rename: false,
            source_db_state: None,
            backup_sha256: None,
            source_logical_sha256: None,
        };
        manifest.validate(role)?;
        Ok(manifest)
    }

    /// Construct an unfinalized `redb` fresh-create manifest (`init_phase = "fresh_create"`).
    pub fn new_redb_fresh_create(
        generation: u64,
        targets_staged_for_rename: bool,
        role: DatabaseRole,
    ) -> StorageResult<Self> {
        let manifest = Self {
            schema_version: CONFIG_FORMAT_MANIFEST_VERSION,
            backend: StorageBackendKind::Redb,
            finalized: false,
            init_phase: Some(ConfigInitPhase::FreshCreate),
            generation,
            legacy_sqlite_tombstone: true,
            targets_staged_for_rename,
            source_db_state: None,
            backup_sha256: None,
            source_logical_sha256: None,
        };
        manifest.validate(role)?;
        Ok(manifest)
    }

    /// Validate structural invariants of `config-format.json` for `role`.
    pub fn validate(&self, role: DatabaseRole) -> StorageResult<()> {
        validate_config_scope_role(role)?;
        if self.schema_version == 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: "config-format.json has invalid schema_version 0".to_string(),
            });
        }
        if self.schema_version > CONFIG_FORMAT_MANIFEST_VERSION {
            return Err(StorageBackendError::SchemaTooNew {
                role,
                found: self.schema_version,
                max_supported: CONFIG_FORMAT_MANIFEST_VERSION,
            });
        }
        if self.generation == 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: "config-format.json generation must be >= 1".to_string(),
            });
        }
        if self.backend == StorageBackendKind::Sqlite && self.legacy_sqlite_tombstone {
            return Err(StorageBackendError::BackendInternal {
                detail: "sqlite config-format.json cannot set legacy_sqlite_tombstone=true"
                    .to_string(),
            });
        }
        if self.backend == StorageBackendKind::Sqlite
            && self.init_phase == Some(ConfigInitPhase::FreshCreate)
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "sqlite config-format.json cannot use init_phase=fresh_create".to_string(),
            });
        }
        if self.finalized {
            if self.init_phase.is_some() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "finalized config-format.json must have init_phase=null".to_string(),
                });
            }
            if self.targets_staged_for_rename {
                return Err(StorageBackendError::BackendInternal {
                    detail:
                        "finalized config-format.json must have targets_staged_for_rename=false"
                            .to_string(),
                });
            }
        } else if self.init_phase == Some(ConfigInitPhase::FreshCreate)
            && !self.legacy_sqlite_tombstone
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "fresh_create config-format.json must set legacy_sqlite_tombstone=true"
                    .to_string(),
            });
        }

        for (field, val) in [
            ("backup_sha256", &self.backup_sha256),
            ("source_logical_sha256", &self.source_logical_sha256),
        ] {
            if let Some(digest) = val
                && digest.trim().is_empty()
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!("config-format.json `{field}` must not be empty"),
                });
            }
        }
        Ok(())
    }
}

/// Authoritative scope state detected directly from `<scope_dir>` WITHOUT
/// creating files or consulting `LIBRA_STORAGE_BACKEND` (`ADR-RB-02` #2, `RB-03b`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigScopeAuthority {
    /// A finalized `config-format.json` manifest governs this scope.
    Manifest {
        /// Validated finalized manifest.
        manifest: ConfigFormatManifest,
        /// Authoritative database path (`config.redb` or `config.db`).
        db_path: PathBuf,
    },
    /// No manifest exists, and only an unmigrated `config.db` exists.
    LegacySqlite {
        /// Path to `<scope_dir>/config.db`.
        db_path: PathBuf,
    },
    /// No manifest exists, and only an unmanifested `config.redb` exists.
    UnmanifestedRedb {
        /// Path to `<scope_dir>/config.redb`.
        db_path: PathBuf,
    },
    /// Scope directory does not exist or contains neither manifest nor database files.
    Empty,
}

impl ConfigScopeAuthority {
    /// Return the authoritative backend when determined by finalized manifest or
    /// legacy `config.db`, or `None` when empty / unmanifested.
    #[must_use]
    pub const fn backend(&self) -> Option<StorageBackendKind> {
        match self {
            Self::Manifest { manifest, .. } => Some(manifest.backend),
            Self::LegacySqlite { .. } => Some(StorageBackendKind::Sqlite),
            Self::UnmanifestedRedb { .. } | Self::Empty => None,
        }
    }
}

#[cfg(windows)]
fn durable_atomic_rename_config_manifest(tmp_path: &Path, final_path: &Path) -> StorageResult<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = tmp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = final_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `source` and `destination` are NUL-terminated UTF-16 buffers alive
    // for the duration of `MoveFileExW`.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(StorageBackendError::io(
            "atomically rename config-format.json temp file with write-through",
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn durable_atomic_rename_config_manifest(tmp_path: &Path, final_path: &Path) -> StorageResult<()> {
    fs::rename(tmp_path, final_path)
        .map_err(|err| StorageBackendError::io("atomically rename config-format.json", err))
}

struct ConfigManifestTempGuard(Option<PathBuf>);

impl Drop for ConfigManifestTempGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Atomically publish `<scope_dir>/config-format.json` via `.tmp` (`O_CREAT | O_EXCL`)
/// → role permission mode → `sync_all()` → atomic rename → `fsync_dir(scope_dir)`.
pub fn publish_config_format_manifest(
    scope_dir: &Path,
    role: DatabaseRole,
    manifest: &ConfigFormatManifest,
) -> StorageResult<PathBuf> {
    manifest.validate(role)?;

    #[cfg(not(any(unix, windows)))]
    return Err(StorageBackendError::BackendInternal {
        detail: "durable config-format.json publication is unsupported on this target platform"
            .to_string(),
    });

    let dir_meta = fs::metadata(scope_dir).map_err(|err| {
        StorageBackendError::io(
            "verify scope directory before config-format.json publish",
            err,
        )
    })?;
    if !dir_meta.is_dir() {
        return Err(StorageBackendError::BackendInternal {
            detail: "cannot publish config-format.json: scope path is not a directory".to_string(),
        });
    }

    let final_path = config_format_manifest_path(scope_dir);
    if let Ok(meta) = fs::symlink_metadata(&final_path)
        && (meta.file_type().is_symlink() || !meta.is_file())
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "refusing to overwrite non-regular config-format.json target".to_string(),
        });
    }

    let mut payload = serde_json::to_vec_pretty(manifest).map_err(|err| {
        StorageBackendError::BackendInternal {
            detail: format!(
                "failed to serialize config-format.json ({:?})",
                err.classify()
            ),
        }
    })?;
    payload.push(b'\n');

    #[cfg(unix)]
    let target_mode =
        expected_config_file_mode(role).ok_or_else(|| StorageBackendError::BackendInternal {
            detail: format!("{role} has no defined config-format.json file mode"),
        })?;

    let mut created = None;
    for _ in 0..MAX_CONFIG_MANIFEST_TEMP_ATTEMPTS {
        let seq = CONFIG_MANIFEST_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(
            "{CONFIG_FORMAT_MANIFEST_FILE}.{}.{}.tmp",
            std::process::id(),
            seq
        );
        let candidate = scope_dir.join(tmp_name);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&candidate) {
            Ok(file) => {
                created = Some((candidate, file));
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(StorageBackendError::io(
                    "create config-format.json temp file",
                    err,
                ));
            }
        }
    }

    let (tmp_path, mut file) = created.ok_or_else(|| StorageBackendError::BackendInternal {
        detail: "exhausted temp filename attempts while publishing config-format.json".to_string(),
    })?;
    let mut guard = ConfigManifestTempGuard(Some(tmp_path.clone()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(target_mode))
            .map_err(|err| StorageBackendError::io("fchmod config-format.json temp file", err))?;
    }

    file.write_all(&payload)
        .map_err(|err| StorageBackendError::io("write config-format.json temp file", err))?;
    file.sync_all()
        .map_err(|err| StorageBackendError::io("fsync config-format.json temp file", err))?;
    drop(file);

    durable_atomic_rename_config_manifest(&tmp_path, &final_path)?;
    guard.0 = None;
    fsync_dir(scope_dir)?;
    Ok(final_path)
}

/// Read and validate `<scope_dir>/config-format.json` if present (strictly read-only).
///
/// Returns `Ok(None)` when `config-format.json` does not exist on disk.
/// Rejects symlinks, directories, oversized files, invalid JSON, future
/// `schema_version` (`SchemaTooNew`), and structurally invalid manifests.
pub fn read_config_format_manifest(
    scope_dir: &Path,
    role: DatabaseRole,
) -> StorageResult<Option<ConfigFormatManifest>> {
    validate_config_scope_role(role)?;
    let path = config_format_manifest_path(scope_dir);
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(StorageBackendError::io("stat config-format.json", err)),
    };

    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail:
                "config-format.json must be a regular file (symlinks and directories are rejected)"
                    .to_string(),
        });
    }

    if meta.len() > MAX_CONFIG_MANIFEST_BYTES {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "config-format.json exceeds maximum allowed size ({} > {MAX_CONFIG_MANIFEST_BYTES} bytes)",
                meta.len()
            ),
        });
    }

    let file =
        File::open(&path).map_err(|err| StorageBackendError::io("open config-format.json", err))?;
    let mut raw = Vec::with_capacity(meta.len().min(MAX_CONFIG_MANIFEST_BYTES) as usize);
    file.take(MAX_CONFIG_MANIFEST_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|err| StorageBackendError::io("read config-format.json", err))?;
    if raw.len() as u64 > MAX_CONFIG_MANIFEST_BYTES {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "config-format.json exceeds maximum allowed size ({MAX_CONFIG_MANIFEST_BYTES} bytes)"
            ),
        });
    }

    let json_val: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|err| StorageBackendError::BackendInternal {
            detail: format!(
                "config-format.json contains invalid JSON ({:?})",
                err.classify()
            ),
        })?;

    if let Some(ver) = json_val
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        && ver > u64::from(CONFIG_FORMAT_MANIFEST_VERSION)
    {
        return Err(StorageBackendError::SchemaTooNew {
            role,
            found: u32::try_from(ver).unwrap_or(u32::MAX),
            max_supported: CONFIG_FORMAT_MANIFEST_VERSION,
        });
    }

    let manifest: ConfigFormatManifest =
        serde_json::from_value(json_val).map_err(|err| StorageBackendError::BackendInternal {
            detail: format!(
                "config-format.json failed schema validation ({:?})",
                err.classify()
            ),
        })?;
    manifest.validate(role)?;
    Ok(Some(manifest))
}

fn probe_regular_scope_file(path: &Path, label: &'static str) -> StorageResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "{label} in config scope must be a regular file (symlinks and directories are rejected)"
                    ),
                });
            }
            Ok(true)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(StorageBackendError::io(label, err)),
    }
}

/// Returns `true` if `path` is a regular file whose exact contents match
/// [`CONFIG_SQLITE_TOMBSTONE_SENTINEL`] (`LIBRA_CONFIG_AUTHORITY_IS_REDB\n`).
///
/// Rejects symlinks and directories fail-closed.
pub fn is_config_sqlite_tombstone_sentinel(path: &Path) -> StorageResult<bool> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(StorageBackendError::io("stat config.db tombstone", err)),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail:
                "config.db in config scope must be a regular file (symlinks and directories are rejected)"
                    .to_string(),
        });
    }
    if meta.len() != CONFIG_SQLITE_TOMBSTONE_SENTINEL.len() as u64 {
        return Ok(false);
    }
    let mut file = File::open(path)
        .map_err(|err| StorageBackendError::io("open config.db for tombstone check", err))?;
    let mut buf = [0u8; 32];
    let n = file
        .read(&mut buf)
        .map_err(|err| StorageBackendError::io("read config.db for tombstone check", err))?;
    Ok(
        n == CONFIG_SQLITE_TOMBSTONE_SENTINEL.len()
            && &buf[..n] == CONFIG_SQLITE_TOMBSTONE_SENTINEL,
    )
}

const SQLITE_HEADER_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const MAX_SQLITE_MASTER_PAGES: usize = 64;
const MAX_SQLITE_RECORD_BYTES: usize = 256 * 1024;

fn read_sqlite_varint(buf: &[u8]) -> Option<(u64, usize)> {
    let mut val = 0u64;
    for (i, &byte) in buf.iter().take(9).enumerate() {
        if i == 8 {
            val = (val << 8) | u64::from(byte);
            return Some((val, 9));
        }
        val = (val << 7) | u64::from(byte & 0x7F);
        if (byte & 0x80) == 0 {
            return Some((val, i + 1));
        }
    }
    None
}

fn read_sqlite_page(file: &mut File, page_no: u32, page_size: usize) -> StorageResult<Vec<u8>> {
    use std::io::Seek;

    if page_no == 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "invalid SQLite page number 0".to_string(),
        });
    }
    let offset = u64::from(page_no - 1)
        .checked_mul(page_size as u64)
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "SQLite page offset overflow".to_string(),
        })?;
    file.seek(io::SeekFrom::Start(offset))
        .map_err(|err| StorageBackendError::io("seek sqlite tombstone page", err))?;
    let mut buf = vec![0u8; page_size];
    file.read_exact(&mut buf)
        .map_err(|err| StorageBackendError::io("read sqlite tombstone page", err))?;
    Ok(buf)
}

fn sqlite_master_record_matches_fence(payload: &[u8]) -> bool {
    let Some((hdr_len_u64, mut hdr_pos)) = read_sqlite_varint(payload) else {
        return false;
    };
    let Ok(hdr_len) = usize::try_from(hdr_len_u64) else {
        return false;
    };
    if hdr_len < hdr_pos || hdr_len > payload.len() {
        return false;
    }

    let mut serial_types = Vec::with_capacity(5);
    while hdr_pos < hdr_len {
        let Some((st, used)) = read_sqlite_varint(&payload[hdr_pos..hdr_len]) else {
            return false;
        };
        hdr_pos += used;
        serial_types.push(st);
        if serial_types.len() > 8 {
            return false;
        }
    }
    if serial_types.len() != 5 {
        return false;
    }

    let mut body_pos = hdr_len;
    let mut cols: [Option<&str>; 5] = [None; 5];
    for (idx, st) in serial_types.into_iter().enumerate() {
        let col_len: usize = match st {
            0 | 8 | 9 => 0,
            1 => 1,
            2 => 2,
            3 => 3,
            4 => 4,
            5 => 6,
            6 | 7 => 8,
            10 | 11 => return false,
            n if n >= 12 => {
                let Ok(len) = usize::try_from((n - 12) / 2) else {
                    return false;
                };
                len
            }
            _ => return false,
        };
        let Some(end) = body_pos.checked_add(col_len) else {
            return false;
        };
        if end > payload.len() {
            return false;
        }
        if st >= 13 && (st & 1) == 1 {
            cols[idx] = std::str::from_utf8(&payload[body_pos..end]).ok();
        }
        body_pos = end;
    }

    cols[0] == Some("table")
        && cols[1] == Some(SQLITE_MIGRATED_FENCE_TABLE)
        && cols[2] == Some(SQLITE_MIGRATED_FENCE_TABLE)
        && cols[4] == Some(SQLITE_MIGRATED_FENCE_SQL)
}

fn sqlite_sidecar_has_bytes(db_path: &Path, suffix: &str) -> StorageResult<bool> {
    let mut os = db_path.as_os_str().to_os_string();
    os.push(suffix);
    let sidecar = PathBuf::from(os);
    match fs::symlink_metadata(&sidecar) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Ok(true);
            }
            Ok(meta.len() > 0)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(StorageBackendError::io(
            "stat sqlite tombstone sidecar",
            err,
        )),
    }
}

/// Returns `true` if `path` is a regular SQLite 3 database whose `sqlite_master`
/// B-tree (root page 1) contains the exact committed `__libra_migrated_fence__`
/// tombstone record (`ADR-RB-07` #1) and has no active non-empty `-journal` or
/// `-wal` sidecar.
///
/// Operates strictly read-only directly on SQLite B-tree page 1 (never writes
/// sidecars, works identically across `storage-sqlite` and `storage-redb`-only
/// builds, and ignores user data in `config_kv` pages).
pub fn is_config_sqlite_fenced_tombstone(path: &Path) -> StorageResult<bool> {
    use std::collections::HashSet;

    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(StorageBackendError::io("stat sqlite tombstone", err)),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail:
                "config.db in config scope must be a regular file (symlinks and directories are rejected)"
                    .to_string(),
        });
    }
    if meta.len() < 512 {
        return Ok(false);
    }
    if sqlite_sidecar_has_bytes(path, "-journal")? || sqlite_sidecar_has_bytes(path, "-wal")? {
        return Ok(false);
    }

    let mut file = File::open(path)
        .map_err(|err| StorageBackendError::io("open config.db for fence check", err))?;
    let mut header = [0u8; 100];
    if file.read_exact(&mut header).is_err() {
        return Ok(false);
    }
    if &header[0..16] != SQLITE_HEADER_MAGIC {
        return Ok(false);
    }
    let raw_ps = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if raw_ps == 1 {
        65536usize
    } else {
        usize::from(raw_ps)
    };
    if !(512..=65536).contains(&page_size) || !page_size.is_power_of_two() {
        return Ok(false);
    }
    let reserved = usize::from(header[20]);
    let Some(usable_size) = page_size.checked_sub(reserved) else {
        return Ok(false);
    };
    if usable_size < 480 {
        return Ok(false);
    }
    let text_encoding = u32::from_be_bytes([header[56], header[57], header[58], header[59]]);
    if text_encoding != 1 {
        return Ok(false);
    }
    let file_pages = meta.len() / (page_size as u64);
    if file_pages == 0 {
        return Ok(false);
    }

    let mut stack = vec![1u32];
    let mut visited = HashSet::new();
    while let Some(page_no) = stack.pop() {
        if visited.len() >= MAX_SQLITE_MASTER_PAGES || !visited.insert(page_no) {
            return Ok(false);
        }
        if u64::from(page_no) > file_pages {
            return Ok(false);
        }
        let Ok(page) = read_sqlite_page(&mut file, page_no, page_size) else {
            return Ok(false);
        };
        let hdr_off = if page_no == 1 { 100usize } else { 0usize };
        if hdr_off + 8 > usable_size {
            return Ok(false);
        }
        let page_type = page[hdr_off];
        let cell_count = usize::from(u16::from_be_bytes([page[hdr_off + 3], page[hdr_off + 4]]));

        match page_type {
            0x05 => {
                // Table B-tree interior page (12-byte header + right_child).
                let ptr_base = hdr_off + 12;
                let Some(ptr_end) = ptr_base.checked_add(cell_count.saturating_mul(2)) else {
                    return Ok(false);
                };
                if ptr_end > usable_size {
                    return Ok(false);
                }
                let right_child = u32::from_be_bytes([
                    page[hdr_off + 8],
                    page[hdr_off + 9],
                    page[hdr_off + 10],
                    page[hdr_off + 11],
                ]);
                if right_child > 0 {
                    stack.push(right_child);
                }
                for i in 0..cell_count {
                    let p_off = ptr_base + i * 2;
                    let cell_off = usize::from(u16::from_be_bytes([page[p_off], page[p_off + 1]]));
                    if cell_off + 4 > usable_size {
                        return Ok(false);
                    }
                    let left_child = u32::from_be_bytes([
                        page[cell_off],
                        page[cell_off + 1],
                        page[cell_off + 2],
                        page[cell_off + 3],
                    ]);
                    if left_child > 0 {
                        stack.push(left_child);
                    }
                }
            }
            0x0d => {
                // Table B-tree leaf page (8-byte header).
                let ptr_base = hdr_off + 8;
                let Some(ptr_end) = ptr_base.checked_add(cell_count.saturating_mul(2)) else {
                    return Ok(false);
                };
                if ptr_end > usable_size {
                    return Ok(false);
                }
                let max_inline = usable_size - 35;
                let min_inline = ((usable_size - 12) * 32 / 255).saturating_sub(23);

                for i in 0..cell_count {
                    let p_off = ptr_base + i * 2;
                    let cell_off = usize::from(u16::from_be_bytes([page[p_off], page[p_off + 1]]));
                    if cell_off >= usable_size {
                        return Ok(false);
                    }
                    let Some((payload_len_u64, n1)) =
                        read_sqlite_varint(&page[cell_off..usable_size])
                    else {
                        return Ok(false);
                    };
                    let Ok(payload_len) = usize::try_from(payload_len_u64) else {
                        return Ok(false);
                    };
                    if payload_len > MAX_SQLITE_RECORD_BYTES {
                        continue;
                    }
                    let after_len = cell_off + n1;
                    let Some((_, n2)) = read_sqlite_varint(&page[after_len..usable_size]) else {
                        return Ok(false);
                    };
                    let data_start = after_len + n2;

                    let payload = if payload_len <= max_inline {
                        let Some(data_end) = data_start.checked_add(payload_len) else {
                            return Ok(false);
                        };
                        if data_end > usable_size {
                            return Ok(false);
                        }
                        page[data_start..data_end].to_vec()
                    } else {
                        let k = min_inline + ((payload_len - min_inline) % (usable_size - 4));
                        let inline_len = if k <= max_inline { k } else { min_inline };
                        let Some(ovf_ptr_off) = data_start.checked_add(inline_len) else {
                            return Ok(false);
                        };
                        if ovf_ptr_off + 4 > usable_size {
                            return Ok(false);
                        }
                        let mut out = Vec::with_capacity(payload_len);
                        out.extend_from_slice(&page[data_start..ovf_ptr_off]);
                        let mut next_ovf = u32::from_be_bytes([
                            page[ovf_ptr_off],
                            page[ovf_ptr_off + 1],
                            page[ovf_ptr_off + 2],
                            page[ovf_ptr_off + 3],
                        ]);
                        let mut ovf_visited = HashSet::new();
                        while out.len() < payload_len {
                            if next_ovf == 0
                                || u64::from(next_ovf) > file_pages
                                || !ovf_visited.insert(next_ovf)
                                || ovf_visited.len() > MAX_SQLITE_MASTER_PAGES
                            {
                                return Ok(false);
                            }
                            let Ok(ovf_page) = read_sqlite_page(&mut file, next_ovf, page_size)
                            else {
                                return Ok(false);
                            };
                            next_ovf = u32::from_be_bytes([
                                ovf_page[0],
                                ovf_page[1],
                                ovf_page[2],
                                ovf_page[3],
                            ]);
                            let take = (payload_len - out.len()).min(usable_size - 4);
                            out.extend_from_slice(&ovf_page[4..4 + take]);
                        }
                        out
                    };

                    if sqlite_master_record_matches_fence(&payload) {
                        return Ok(true);
                    }
                }
            }
            _ => return Ok(false),
        }
    }

    Ok(false)
}

/// Verify that `sqlite_path` exists as a read-only regular file carrying either
/// the 31-byte `LIBRA_CONFIG_AUTHORITY_IS_REDB\n` sentinel or a valid committed
/// `__libra_migrated_fence__` SQLite fence (`ADR-RB-02` #2, `RB-03b`).
fn verify_config_sqlite_tombstone(sqlite_path: &Path, role: DatabaseRole) -> StorageResult<()> {
    let meta = match fs::symlink_metadata(sqlite_path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "{role} finalized redb manifest requires legacy_sqlite_tombstone=true, but config.db tombstone is missing"
                ),
            });
        }
        Err(err) => {
            return Err(StorageBackendError::io(
                "stat required config.db tombstone",
                err,
            ));
        }
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "{role} config.db tombstone must be a regular file (symlinks and directories are rejected)"
            ),
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if (mode & 0o222) != 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "{role} config.db tombstone must be read-only (non-writable), found mode {mode:04o}"
                ),
            });
        }
        if role == DatabaseRole::GlobalConfig && (mode & 0o077) != 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "GlobalConfig config.db tombstone must not grant group/other permissions, found mode {mode:04o}"
                ),
            });
        }
    }
    #[cfg(not(unix))]
    {
        if !meta.permissions().readonly() {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("{role} config.db tombstone must have readonly attribute set"),
            });
        }
    }

    if is_config_sqlite_tombstone_sentinel(sqlite_path)?
        || is_config_sqlite_fenced_tombstone(sqlite_path)?
    {
        return Ok(());
    }

    Err(StorageBackendError::BackendInternal {
        detail: format!(
            "{role} config.db is neither a valid redb sentinel nor a fenced migrated SQLite tombstone"
        ),
    })
}

/// Inspect `<scope_dir>` strictly read-only to establish config scope authority
/// BEFORE consulting `LIBRA_STORAGE_BACKEND` (`ADR-RB-02` #2, `RB-03b`).
///
/// Enforces:
/// 1. `config-format.json` with `finalized == false` ALWAYS fails closed with
///    [`StorageBackendError::ManifestNotFinalized`] (`LBR-IO-001`), ignoring
///    `LIBRA_STORAGE_BACKEND`.
/// 2. Missing `config-format.json` with BOTH `config.db` and `config.redb`
///    present fails closed with `LBR-IO-001` (split-brain guard).
/// 3. Finalized `redb` manifest with `legacy_sqlite_tombstone == true` requires
///    and verifies a read-only regular `config.db` tombstone (either the 31-byte
///    sentinel `LIBRA_CONFIG_AUTHORITY_IS_REDB\n` or a committed
///    `__libra_migrated_fence__` SQLite fence) before accepting the scope.
/// 4. Missing or empty scope directory returns [`ConfigScopeAuthority::Empty`]
///    without creating any directory, lock file, manifest, or database file
///    (`ADR-CF-06`).
pub fn inspect_config_scope_authority(
    scope_dir: &Path,
    role: DatabaseRole,
) -> StorageResult<ConfigScopeAuthority> {
    validate_config_scope_role(role)?;

    let dir_meta = match fs::metadata(scope_dir) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(scope_dir).is_ok() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "config scope path is a dangling symlink".to_string(),
                });
            }
            return Ok(ConfigScopeAuthority::Empty);
        }
        Err(err) => return Err(StorageBackendError::io("stat config scope directory", err)),
    };
    if !dir_meta.is_dir() {
        return Err(StorageBackendError::BackendInternal {
            detail: "config scope path must be a directory".to_string(),
        });
    }

    let sqlite_path = config_sqlite_path_in_scope_dir(scope_dir);
    let redb_path = config_redb_path_in_scope_dir(scope_dir);

    if let Some(manifest) = read_config_format_manifest(scope_dir, role)? {
        if !manifest.finalized {
            return Err(StorageBackendError::ManifestNotFinalized {
                detail: format!(
                    "{role} config-format.json for backend `{}` generation {} has finalized=false",
                    manifest.backend, manifest.generation
                ),
            });
        }
        let has_sqlite = probe_regular_scope_file(&sqlite_path, "config.db")?;
        let has_redb = probe_regular_scope_file(&redb_path, "config.redb")?;

        return match manifest.backend {
            StorageBackendKind::Redb => {
                if manifest.legacy_sqlite_tombstone {
                    verify_config_sqlite_tombstone(&sqlite_path, role)?;
                } else if has_sqlite {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "{role} config scope has finalized redb manifest without legacy_sqlite_tombstone=true while config.db coexists"
                        ),
                    });
                }
                if !has_redb {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "{role} finalized redb config-format.json is missing authoritative config.redb"
                        ),
                    });
                }
                Ok(ConfigScopeAuthority::Manifest {
                    manifest,
                    db_path: redb_path,
                })
            }
            StorageBackendKind::Sqlite => {
                if has_redb {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "{role} config scope has finalized sqlite manifest while config.redb coexists"
                        ),
                    });
                }
                if !has_sqlite {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "{role} finalized sqlite config-format.json is missing authoritative config.db"
                        ),
                    });
                }
                Ok(ConfigScopeAuthority::Manifest {
                    manifest,
                    db_path: sqlite_path,
                })
            }
        };
    }

    // No `config-format.json` present.
    let has_sqlite = probe_regular_scope_file(&sqlite_path, "config.db")?;
    let has_redb = probe_regular_scope_file(&redb_path, "config.redb")?;

    match (has_sqlite, has_redb) {
        (true, true) => Err(StorageBackendError::BackendInternal {
            detail: format!(
                "{role} config scope contains both config.db and config.redb without config-format.json (split-brain)"
            ),
        }),
        (true, false) => {
            if is_config_sqlite_tombstone_sentinel(&sqlite_path)?
                || is_config_sqlite_fenced_tombstone(&sqlite_path)?
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "{role} config scope contains a fenced/tombstone config.db without config-format.json"
                    ),
                });
            }
            Ok(ConfigScopeAuthority::LegacySqlite {
                db_path: sqlite_path,
            })
        }
        (false, true) => Ok(ConfigScopeAuthority::UnmanifestedRedb { db_path: redb_path }),
        (false, false) => Ok(ConfigScopeAuthority::Empty),
    }
}

/// Cross-runtime advisory lock on `<scope_dir>/config-migration.lock`
/// (`ADR-RB-02` #2, `RB-03h`).
///
/// Participated in by both `Sqlite` and `Redb` runtime paths as well as
/// `libra-migrate --global/--system`. Always enforces bounded wait deadlines
/// (`1ms -> 25ms` jittered backoff; supports `Duration::ZERO` for immediate
/// non-blocking checks) and fails closed with [`StorageBackendError::WriteLockBusy`]
/// (`LBR-IO-001`) when contention exceeds `deadline`.
#[derive(Debug)]
pub struct ConfigScopeLock {
    file: File,
    role: DatabaseRole,
    exclusive: bool,
}

impl Drop for ConfigScopeLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn verify_scope_dir_exists(scope_dir: &Path) -> StorageResult<()> {
    let dir_meta = match fs::metadata(scope_dir) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(scope_dir).is_ok() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "config scope path is a dangling symlink".to_string(),
                });
            }
            return Err(StorageBackendError::NotFound {
                resource: "configuration scope directory".to_string(),
            });
        }
        Err(err) => return Err(StorageBackendError::io("stat config scope directory", err)),
    };
    if !dir_meta.is_dir() {
        return Err(StorageBackendError::BackendInternal {
            detail: "config scope path must be a directory".to_string(),
        });
    }
    Ok(())
}

fn open_existing_lock_file(lock_path: &Path, writable: bool) -> StorageResult<Option<File>> {
    match fs::symlink_metadata(lock_path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "config-migration.lock must be a regular file (symlinks and directories are rejected)"
                        .to_string(),
                });
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(StorageBackendError::io("stat config-migration.lock", err)),
    }

    let mut opts = OpenOptions::new();
    opts.read(true);
    if writable {
        opts.write(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }

    let file = match opts.open(lock_path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(StorageBackendError::io("open config-migration.lock", err)),
    };

    let meta = file
        .metadata()
        .map_err(|err| StorageBackendError::io("fstat config-migration.lock", err))?;
    if !meta.file_type().is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail: "config-migration.lock must be a regular file".to_string(),
        });
    }
    Ok(Some(file))
}

fn open_or_create_lock_file(
    scope_dir: &Path,
    role: DatabaseRole,
    writable: bool,
) -> StorageResult<File> {
    verify_scope_dir_exists(scope_dir)?;
    let lock_path = config_migration_lock_path(scope_dir);

    for _ in 0..8 {
        if let Some(existing) = open_existing_lock_file(&lock_path, writable)? {
            return Ok(existing);
        }

        #[cfg(unix)]
        let target_mode = expected_config_file_mode(role).ok_or_else(|| {
            StorageBackendError::BackendInternal {
                detail: format!("{role} has no defined config-migration.lock mode"),
            }
        })?;

        let mut create_opts = OpenOptions::new();
        create_opts.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            create_opts
                .mode(target_mode)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        match create_opts.open(&lock_path) {
            Ok(file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(fs::Permissions::from_mode(target_mode))
                        .map_err(|err| {
                            StorageBackendError::io("fchmod config-migration.lock", err)
                        })?;
                }
                fsync_dir(scope_dir)?;
                if !writable {
                    drop(file);
                    if let Some(reopened) = open_existing_lock_file(&lock_path, false)? {
                        return Ok(reopened);
                    }
                    continue;
                }
                return Ok(file);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(StorageBackendError::io("create config-migration.lock", err));
            }
        }
    }

    Err(StorageBackendError::BackendInternal {
        detail: "exhausted retry attempts opening config-migration.lock".to_string(),
    })
}

fn acquire_lock_bounded(
    file: File,
    role: DatabaseRole,
    exclusive: bool,
    deadline: Duration,
) -> StorageResult<ConfigScopeLock> {
    let start = Instant::now();
    let end = start + deadline;
    let mut backoff_ms = 1u64;
    let mut attempt = 0u64;

    loop {
        let attempt_res = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        match attempt_res {
            Ok(()) => {
                return Ok(ConfigScopeLock {
                    file,
                    role,
                    exclusive,
                });
            }
            Err(fs::TryLockError::WouldBlock) => {
                let now = Instant::now();
                if now >= end {
                    let waited_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return Err(StorageBackendError::WriteLockBusy { role, waited_ms });
                }
                let remaining = end.saturating_duration_since(now);
                let jitter_ms = (attempt
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1)
                    >> 62)
                    % 3;
                let sleep_dur =
                    Duration::from_millis((backoff_ms + jitter_ms).min(25)).min(remaining);
                std::thread::sleep(sleep_dur);
                backoff_ms = backoff_ms.saturating_mul(2).min(25);
                attempt = attempt.saturating_add(1);
            }
            Err(fs::TryLockError::Error(err)) => {
                return Err(StorageBackendError::io(
                    "acquire config-migration.lock",
                    err,
                ));
            }
        }
    }
}

impl ConfigScopeLock {
    /// Acquire a bounded shared (`LOCK_SH`) lock on `<scope_dir>/config-migration.lock`,
    /// creating the lock file (`0600` Global / `0644` System) if absent.
    ///
    /// Multiple processes may hold shared locks concurrently while mutually
    /// excluding an exclusive migration or first-write lock (`LOCK_EX`).
    /// Returns [`StorageBackendError::WriteLockBusy`] (`LBR-IO-001`) if
    /// contention exceeds `deadline` (supports `Duration::ZERO`).
    pub fn try_lock_shared_bounded(
        scope_dir: &Path,
        role: DatabaseRole,
        deadline: Duration,
    ) -> StorageResult<Self> {
        validate_config_scope_role(role)?;
        let file = open_or_create_lock_file(scope_dir, role, false)?;
        acquire_lock_bounded(file, role, false, deadline)
    }

    /// Acquire a bounded exclusive (`LOCK_EX`) lock on `<scope_dir>/config-migration.lock`,
    /// creating the lock file (`0600` Global / `0644` System) if absent.
    ///
    /// Mutually excludes all concurrent shared readers/writers and other exclusive
    /// holders. Returns [`StorageBackendError::WriteLockBusy`] (`LBR-IO-001`) if
    /// contention exceeds `deadline` (supports `Duration::ZERO`).
    pub fn try_lock_exclusive_bounded(
        scope_dir: &Path,
        role: DatabaseRole,
        deadline: Duration,
    ) -> StorageResult<Self> {
        validate_config_scope_role(role)?;
        let file = open_or_create_lock_file(scope_dir, role, true)?;
        acquire_lock_bounded(file, role, true, deadline)
    }

    /// Acquire a bounded shared (`LOCK_SH`) lock ONLY if
    /// `<scope_dir>/config-migration.lock` already exists on disk.
    ///
    /// Strictly read-only: NEVER attempts to create `config-migration.lock`
    /// (`ADR-RB-02` #2, `RB-03h` AC4). Returns `Ok(None)` when the lock file is
    /// absent, or `Err(WriteLockBusy)` (`LBR-IO-001`) if an existing lock file
    /// is exclusively held past `deadline`.
    pub fn try_lock_shared_if_present_bounded(
        scope_dir: &Path,
        role: DatabaseRole,
        deadline: Duration,
    ) -> StorageResult<Option<Self>> {
        validate_config_scope_role(role)?;
        verify_scope_dir_exists(scope_dir)?;
        let lock_path = config_migration_lock_path(scope_dir);
        let Some(file) = open_existing_lock_file(&lock_path, false)? else {
            return Ok(None);
        };
        acquire_lock_bounded(file, role, false, deadline).map(Some)
    }

    /// Owner configuration role (`GlobalConfig` or `SystemConfig`).
    #[must_use]
    pub const fn role(&self) -> DatabaseRole {
        self.role
    }

    /// Returns `true` if this guard holds an exclusive (`LOCK_EX`) lock.
    #[must_use]
    pub const fn is_exclusive(&self) -> bool {
        self.exclusive
    }
}

/// Internal wrapper keeping [`ConfigScopeLock`] alive for the entire lifetime of
/// any returned `Arc<dyn ConfigStore>` so concurrent `LOCK_EX` migrations remain
/// blocked until the last store handle is dropped (`RB-03h`).
struct ScopeLockedConfigStore {
    inner: Arc<dyn ConfigStore>,
    lock: Option<ConfigScopeLock>,
}

impl std::fmt::Debug for ScopeLockedConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopeLockedConfigStore")
            .field("inner", &self.inner)
            .field("locked", &self.lock.is_some())
            .finish()
    }
}

#[async_trait]
impl ConfigStore for ScopeLockedConfigStore {
    fn role(&self) -> DatabaseRole {
        self.inner.role()
    }

    fn backend(&self) -> StorageBackendKind {
        self.inner.backend()
    }

    async fn get(&self, key: &str) -> StorageResult<Option<ConfigEntry>> {
        self.inner.get(key).await
    }

    async fn get_all(&self, key: &str) -> StorageResult<Vec<ConfigEntry>> {
        self.inner.get_all(key).await
    }

    async fn get_by_prefix(&self, prefix: &str) -> StorageResult<Vec<ConfigEntry>> {
        self.inner.get_by_prefix(prefix).await
    }

    async fn list_all(&self) -> StorageResult<Vec<ConfigEntry>> {
        self.inner.list_all().await
    }

    async fn set(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()> {
        self.inner.set(key, value, encrypted).await
    }

    async fn add(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()> {
        self.inner.add(key, value, encrypted).await
    }

    async fn unset(&self, key: &str) -> StorageResult<usize> {
        self.inner.unset(key).await
    }

    async fn unset_all(&self, key: &str) -> StorageResult<usize> {
        self.inner.unset_all(key).await
    }

    async fn remove_section(&self, section: &str) -> StorageResult<usize> {
        self.inner.remove_section(section).await
    }

    async fn rename_section(&self, old_section: &str, new_section: &str) -> StorageResult<usize> {
        self.inner.rename_section(old_section, new_section).await
    }

    async fn list_legacy_entries(&self) -> StorageResult<Vec<LegacyConfigEntry>> {
        self.inner.list_legacy_entries().await
    }

    async fn insert_legacy_entry(&self, entry: &LegacyConfigEntry) -> StorageResult<()> {
        self.inner.insert_legacy_entry(entry).await
    }

    async fn list_schema_receipts(&self) -> StorageResult<Vec<SchemaVersionReceipt>> {
        self.inner.list_schema_receipts().await
    }

    async fn record_schema_receipt(&self, receipt: &SchemaVersionReceipt) -> StorageResult<()> {
        self.inner.record_schema_receipt(receipt).await
    }
}

/// Guard holding an opened scoped [`ConfigStore`] whose coordination
/// [`ConfigScopeLock`] is retained for the entire lifetime of the store (and any
/// `Arc<dyn ConfigStore>` cloned from [`Self::store_arc`]) (`ADR-RB-02` #2, `RB-03h`).
#[derive(Debug)]
pub struct LockedConfigScope {
    store: Arc<ScopeLockedConfigStore>,
    role: DatabaseRole,
}

impl LockedConfigScope {
    /// Borrow the underlying lock-retaining [`ConfigStore`] trait object.
    #[must_use]
    pub fn store(&self) -> &dyn ConfigStore {
        self.store.as_ref()
    }

    /// Clone an `Arc<dyn ConfigStore>` that retains the underlying
    /// [`ConfigScopeLock`] until the last clone is dropped.
    #[must_use]
    pub fn store_arc(&self) -> Arc<dyn ConfigStore> {
        Arc::clone(&self.store) as Arc<dyn ConfigStore>
    }

    /// Returns `true` when an advisory `config-migration.lock` is actively held,
    /// or `false` when read-only access fell back directly to manifest inspection
    /// on a read-only `SystemConfig` directory without a lock file (`RB-03h` AC4).
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.store.lock.is_some()
    }

    /// Configuration scope role (`GlobalConfig` or `SystemConfig`).
    #[must_use]
    pub const fn role(&self) -> DatabaseRole {
        self.role
    }
}

/// Determine without attempting any file creation whether `scope_dir` is
/// non-writable for the current process (read-only permissions or read-only
/// filesystem mount) so read-only `SystemConfig` lookups can fall back without
/// trying to create `config-migration.lock` (`RB-03h` AC4).
fn is_scope_dir_non_writable_for_lock_create(scope_dir: &Path, dir_meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        let _ = dir_meta;
        if let Ok(c_dir) = CString::new(scope_dir.as_os_str().as_bytes()) {
            // SAFETY: `c_dir` is a valid NUL-terminated path; `faccessat` with
            // `W_OK | AT_EACCESS` asks the kernel for effective-credential write
            // access (including EROFS read-only mounts) without creating any file.
            let rc = unsafe {
                libc::faccessat(libc::AT_FDCWD, c_dir.as_ptr(), libc::W_OK, libc::AT_EACCESS)
            };
            if rc != 0 {
                let err = io::Error::last_os_error();
                if matches!(
                    err.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
                ) {
                    return true;
                }
            }
        }
        false
    }
    #[cfg(not(unix))]
    {
        let _ = scope_dir;
        dir_meta.permissions().readonly()
    }
}

fn scope_dir_has_any_tracked_entry(scope_dir: &Path) -> StorageResult<bool> {
    for candidate in [
        config_format_manifest_path(scope_dir),
        config_sqlite_path_in_scope_dir(scope_dir),
        config_redb_path_in_scope_dir(scope_dir),
        config_migration_lock_path(scope_dir),
    ] {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => return Ok(true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(StorageBackendError::io(
                    "probe config scope tracked files",
                    err,
                ));
            }
        }
    }
    Ok(false)
}

async fn open_store_from_authority(
    authority: ConfigScopeAuthority,
    role: DatabaseRole,
    writable: bool,
) -> StorageResult<Arc<dyn ConfigStore>> {
    match authority {
        ConfigScopeAuthority::Empty => Err(StorageBackendError::NotFound {
            resource: format!("{role} configuration scope"),
        }),
        ConfigScopeAuthority::Manifest { manifest, db_path } => match manifest.backend {
            StorageBackendKind::Redb => {
                #[cfg(feature = "storage-redb")]
                {
                    let store = if writable {
                        crate::storage::redb::RedbConfigStore::open_read_write(&db_path, role)?
                    } else {
                        crate::storage::redb::RedbConfigStore::open_read_only(&db_path, role)?
                    };
                    Ok(Arc::new(store))
                }
                #[cfg(not(feature = "storage-redb"))]
                {
                    let _ = (db_path, writable);
                    Err(StorageBackendError::UnsupportedBackend {
                        requested: StorageBackendKind::Redb.to_string(),
                        detail: format!(
                            "{role} config scope uses redb, which is not compiled into this binary"
                        ),
                    })
                }
            }
            StorageBackendKind::Sqlite => {
                #[cfg(feature = "storage-sqlite")]
                {
                    let store = if writable {
                        crate::storage::sqlite::SqliteConfigStore::open(&db_path, role).await?
                    } else {
                        crate::storage::sqlite::SqliteConfigStore::open_read_only(&db_path, role)
                            .await?
                    };
                    Ok(Arc::new(store))
                }
                #[cfg(not(feature = "storage-sqlite"))]
                {
                    let _ = (db_path, writable);
                    Err(StorageBackendError::UnsupportedBackend {
                        requested: StorageBackendKind::Sqlite.to_string(),
                        detail: format!(
                            "{role} config scope uses sqlite, which is not compiled into this binary"
                        ),
                    })
                }
            }
        },
        ConfigScopeAuthority::LegacySqlite { db_path } => {
            #[cfg(feature = "storage-sqlite")]
            {
                let store = if writable {
                    crate::storage::sqlite::SqliteConfigStore::open(&db_path, role).await?
                } else {
                    crate::storage::sqlite::SqliteConfigStore::open_read_only(&db_path, role)
                        .await?
                };
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                let _ = (db_path, writable);
                Err(StorageBackendError::UnsupportedBackend {
                    requested: StorageBackendKind::Sqlite.to_string(),
                    detail: format!(
                        "{role} config scope has an unmigrated legacy sqlite config.db; run `libra-migrate`"
                    ),
                })
            }
        }
        ConfigScopeAuthority::UnmanifestedRedb { .. } => {
            Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "{role} config scope contains unmanifested config.redb without config-format.json"
                ),
            })
        }
    }
}

/// Open an existing `GlobalConfig` or `SystemConfig` scope writable under a
/// bounded shared [`ConfigScopeLock`] (`ADR-RB-02` #2, `RB-03b`, `RB-03h`).
///
/// Keeps `LOCK_SH` held inside the returned [`LockedConfigScope`] guard for the
/// duration of the caller's write operation so steady-state same-backend writers
/// run concurrently while mutually excluding `LOCK_EX` migrations.
pub async fn open_locked_config_scope_read_write(
    scope_dir: &Path,
    role: DatabaseRole,
    lock_deadline: Duration,
) -> StorageResult<LockedConfigScope> {
    validate_config_scope_role(role)?;
    let dir_owned = scope_dir.to_path_buf();
    let (lock, authority) = tokio::task::spawn_blocking(move || {
        match fs::metadata(&dir_owned) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(StorageBackendError::BackendInternal {
                    detail: "config scope path must be a directory".to_string(),
                });
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if fs::symlink_metadata(&dir_owned).is_ok() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "config scope path is a dangling symlink".to_string(),
                    });
                }
                return Err(StorageBackendError::NotFound {
                    resource: format!("{role} configuration scope"),
                });
            }
            Err(err) => return Err(StorageBackendError::io("stat config scope directory", err)),
        }
        if !scope_dir_has_any_tracked_entry(&dir_owned)? {
            return Err(StorageBackendError::NotFound {
                resource: format!("{role} configuration scope"),
            });
        }
        let lock = ConfigScopeLock::try_lock_shared_bounded(&dir_owned, role, lock_deadline)?;
        let authority = inspect_config_scope_authority(&dir_owned, role)?;
        Ok((lock, authority))
    })
    .await
    .map_err(|err| StorageBackendError::BackendInternal {
        detail: format!("config scope writable lock task failed: {err}"),
    })??;

    let store = open_store_from_authority(authority, role, true).await?;
    Ok(LockedConfigScope {
        store: Arc::new(ScopeLockedConfigStore {
            inner: store,
            lock: Some(lock),
        }),
        role,
    })
}

/// Open an existing `GlobalConfig` or `SystemConfig` scope writable using the
/// default bounded lock deadline (`2s`).
pub async fn open_config_scope_read_write(
    scope_dir: &Path,
    role: DatabaseRole,
) -> StorageResult<Arc<dyn ConfigStore>> {
    open_config_scope_read_write_with_deadline(scope_dir, role, DEFAULT_CONFIG_SCOPE_LOCK_TIMEOUT)
        .await
}

/// Open an existing `GlobalConfig` or `SystemConfig` scope writable with a
/// caller-specified bounded `ConfigScopeLock` deadline.
pub async fn open_config_scope_read_write_with_deadline(
    scope_dir: &Path,
    role: DatabaseRole,
    lock_deadline: Duration,
) -> StorageResult<Arc<dyn ConfigStore>> {
    let locked = open_locked_config_scope_read_write(scope_dir, role, lock_deadline).await?;
    Ok(locked.store_arc())
}

/// Open a `GlobalConfig` or `SystemConfig` scope strictly read-only under a
/// bounded shared [`ConfigScopeLock`] (or direct read-only manifest inspection
/// fallback when `config-migration.lock` is absent in a read-only `SystemConfig`
/// directory) (`ADR-RB-02` #2, `ADR-CF-06`, `RB-03b`, `RB-03h`).
///
/// Guarantees:
/// - Never reads `LIBRA_STORAGE_BACKEND` to bypass an unfinalized manifest.
/// - Empty/missing scope directory immediately returns `NotFound` without
///   creating any directory or `config-migration.lock` (`ADR-CF-06`).
/// - Non-root read-only `SystemConfig` directory without `config-migration.lock`
///   never attempts to create the lock file and falls back safely to read-only
///   manifest inspection (`RB-03h` AC4).
/// - When `config-migration.lock` exists, acquires bounded `LOCK_SH` strictly
///   via read-only fd (`O_RDONLY | O_NOFOLLOW`) and fails closed with
///   `WriteLockBusy` (`LBR-IO-001`) if exclusively held past `lock_deadline`.
pub async fn open_locked_config_scope_read_only(
    scope_dir: &Path,
    role: DatabaseRole,
    lock_deadline: Duration,
) -> StorageResult<LockedConfigScope> {
    validate_config_scope_role(role)?;
    let dir_owned = scope_dir.to_path_buf();
    let (lock, authority) = tokio::task::spawn_blocking(move || {
        let dir_meta = match fs::metadata(&dir_owned) {
            Ok(meta) if meta.is_dir() => meta,
            Ok(_) => {
                return Err(StorageBackendError::BackendInternal {
                    detail: "config scope path must be a directory".to_string(),
                });
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if fs::symlink_metadata(&dir_owned).is_ok() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "config scope path is a dangling symlink".to_string(),
                    });
                }
                return Err(StorageBackendError::NotFound {
                    resource: format!("{role} configuration scope"),
                });
            }
            Err(err) => return Err(StorageBackendError::io("stat config scope directory", err)),
        };

        if !scope_dir_has_any_tracked_entry(&dir_owned)? {
            return Err(StorageBackendError::NotFound {
                resource: format!("{role} configuration scope"),
            });
        }

        // First try locking an already-existing `config-migration.lock` strictly
        // read-only (succeeds even in `0555` non-root `SystemConfig` dirs).
        let lock = match ConfigScopeLock::try_lock_shared_if_present_bounded(
            &dir_owned,
            role,
            lock_deadline,
        )? {
            Some(lock) => Some(lock),
            None => {
                // `ADR-RB-02` #2 / `RB-03h` AC4: unlocked read-only fallback is
                // restricted strictly to non-writable `SystemConfig` directories
                // where `config-migration.lock` does not yet exist. All writable
                // directories and all `GlobalConfig` directories must acquire
                // `LOCK_SH` or fail closed (`LBR-IO-001`).
                if role == DatabaseRole::SystemConfig
                    && is_scope_dir_non_writable_for_lock_create(&dir_owned, &dir_meta)
                {
                    None
                } else {
                    match ConfigScopeLock::try_lock_shared_bounded(&dir_owned, role, lock_deadline)
                    {
                        Ok(lock) => Some(lock),
                        Err(err)
                            if role == DatabaseRole::SystemConfig
                                && matches!(
                                    err.io_kind(),
                                    Some(
                                        io::ErrorKind::PermissionDenied
                                            | io::ErrorKind::ReadOnlyFilesystem
                                    )
                                ) =>
                        {
                            // Permit unlocked fallback ONLY for `SystemConfig`
                            // after confirming BOTH `config-migration.lock` is
                            // still absent AND the directory is non-writable for
                            // effective credentials. Any error on an existing
                            // lock file fails closed (`LBR-IO-001`).
                            let lock_still_absent = matches!(
                                fs::symlink_metadata(config_migration_lock_path(&dir_owned)),
                                Err(e) if e.kind() == io::ErrorKind::NotFound
                            );
                            let dir_now_non_writable = fs::metadata(&dir_owned)
                                .map(|m| is_scope_dir_non_writable_for_lock_create(&dir_owned, &m))
                                .unwrap_or(false);
                            if lock_still_absent && dir_now_non_writable {
                                None
                            } else {
                                return Err(err);
                            }
                        }
                        Err(err) => return Err(err),
                    }
                }
            }
        };

        let authority = inspect_config_scope_authority(&dir_owned, role)?;
        Ok((lock, authority))
    })
    .await
    .map_err(|err| StorageBackendError::BackendInternal {
        detail: format!("config scope read-only lock task failed: {err}"),
    })??;

    let store = open_store_from_authority(authority, role, false).await?;
    Ok(LockedConfigScope {
        store: Arc::new(ScopeLockedConfigStore { inner: store, lock }),
        role,
    })
}

/// Open a `GlobalConfig` or `SystemConfig` scope strictly read-only using the
/// default bounded lock deadline (`2s`).
pub async fn open_config_scope_read_only(
    scope_dir: &Path,
    role: DatabaseRole,
) -> StorageResult<Arc<dyn ConfigStore>> {
    open_config_scope_read_only_with_deadline(scope_dir, role, DEFAULT_CONFIG_SCOPE_LOCK_TIMEOUT)
        .await
}

/// Open a `GlobalConfig` or `SystemConfig` scope strictly read-only with a
/// caller-specified bounded `ConfigScopeLock` deadline.
pub async fn open_config_scope_read_only_with_deadline(
    scope_dir: &Path,
    role: DatabaseRole,
    lock_deadline: Duration,
) -> StorageResult<Arc<dyn ConfigStore>> {
    let locked = open_locked_config_scope_read_only(scope_dir, role, lock_deadline).await?;
    Ok(locked.store_arc())
}

/// Outcome of Step F1 non-overwriting `std::fs::hard_link` reservation of
/// `config.db` with the 31-byte `LIBRA_CONFIG_AUTHORITY_IS_REDB\n` sentinel
/// (`ADR-RB-02` #2 Step F1, `RB-03d`/`RB-03f`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TombstoneReserveOutcome {
    /// Atomically linked the pre-synced read-only sentinel into `config.db`
    /// and unlinked the temporary alias + `fsync_dir`.
    Reserved,
    /// `hard_link` returned `AlreadyExists` because a legacy writer created
    /// `config.db` first. The temporary file was removed, `config.db` was left
    /// completely untouched, and callers MUST fail closed `LBR-CLI-003` without
    /// creating `config.redb`.
    AlreadyExists,
}

static TOMBSTONE_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
const MAX_TOMBSTONE_TEMP_ATTEMPTS: usize = 64;

trait TombstoneHardLinkOps {
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()>;
}

struct OsTombstoneHardLinkOps;

impl TombstoneHardLinkOps for OsTombstoneHardLinkOps {
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        fs::hard_link(src, dst)
    }
}

struct TombstoneTempGuard(Option<PathBuf>);

impl Drop for TombstoneTempGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            #[cfg(windows)]
            if let Ok(meta) = fs::metadata(&path) {
                let mut perms = meta.permissions();
                perms.set_readonly(false);
                let _ = fs::set_permissions(&path, perms);
            }
            let _ = fs::remove_file(path);
        }
    }
}

fn reserve_config_sqlite_tombstone_hard_link_with_ops(
    scope_dir: &Path,
    role: DatabaseRole,
    ops: &impl TombstoneHardLinkOps,
) -> StorageResult<TombstoneReserveOutcome> {
    validate_config_scope_role(role)?;
    verify_scope_dir_exists(scope_dir)?;

    let sqlite_path = config_sqlite_path_in_scope_dir(scope_dir);
    let mut created = None;
    for _ in 0..MAX_TOMBSTONE_TEMP_ATTEMPTS {
        let seq = TOMBSTONE_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(
            "{CONFIG_SQLITE_FILE_NAME}.tombstone.{}.{}.tmp",
            std::process::id(),
            seq
        );
        let candidate = scope_dir.join(tmp_name);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        match opts.open(&candidate) {
            Ok(file) => {
                created = Some((candidate, file));
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(StorageBackendError::io(
                    "create config.db tombstone temp file",
                    err,
                ));
            }
        }
    }

    let (tmp_path, mut file) = created.ok_or_else(|| StorageBackendError::BackendInternal {
        detail: "exhausted temp filename attempts while staging config.db tombstone".to_string(),
    })?;
    let mut cleanup = TombstoneTempGuard(Some(tmp_path.clone()));

    file.write_all(CONFIG_SQLITE_TOMBSTONE_SENTINEL)
        .map_err(|err| StorageBackendError::io("write config.db tombstone sentinel", err))?;
    file.sync_all()
        .map_err(|err| StorageBackendError::io("fsync config.db tombstone sentinel", err))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tomb_mode = expected_config_tombstone_mode(role).ok_or_else(|| {
            StorageBackendError::BackendInternal {
                detail: format!("{role} has no defined config.db tombstone mode"),
            }
        })?;
        file.set_permissions(fs::Permissions::from_mode(tomb_mode))
            .map_err(|err| StorageBackendError::io("fchmod config.db tombstone sentinel", err))?;
        file.sync_all()
            .map_err(|err| StorageBackendError::io("fsync config.db tombstone mode", err))?;
    }
    drop(file);

    // Strictly non-overwriting `hard_link(tmp, config.db)`. NEVER downgrade to
    // overwriting `rename` or 0-byte `create_new` when the filesystem does not
    // support hard links (`Unsupported` / `ENOSYS` / `EXDEV`).
    match ops.hard_link(&tmp_path, &sqlite_path) {
        Ok(()) => {
            fs::remove_file(&tmp_path).map_err(|err| {
                StorageBackendError::io("unlink config.db tombstone temp alias", err)
            })?;
            cleanup.0 = None;
            #[cfg(windows)]
            {
                let mut perms = fs::metadata(&sqlite_path)
                    .map_err(|err| StorageBackendError::io("stat config.db tombstone", err))?
                    .permissions();
                perms.set_readonly(true);
                fs::set_permissions(&sqlite_path, perms).map_err(|err| {
                    StorageBackendError::io("set readonly on config.db tombstone", err)
                })?;
            }
            fsync_dir(scope_dir)?;
            Ok(TombstoneReserveOutcome::Reserved)
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            // Legacy writer won the race; cleanup drops `tmp_path` without
            // touching `config.db`.
            drop(cleanup);
            Ok(TombstoneReserveOutcome::AlreadyExists)
        }
        Err(err) => {
            // Unsupported/ENOSYS/EXDEV or other I/O error: clean up `.tmp` and
            // fail closed `LBR-IO-001` without ever attempting `rename`.
            drop(cleanup);
            Err(StorageBackendError::io(
                "hard_link config.db tombstone sentinel",
                err,
            ))
        }
    }
}

/// Atomically reserve `<scope_dir>/config.db` with the pre-synced read-only
/// 31-byte sentinel `LIBRA_CONFIG_AUTHORITY_IS_REDB\n` (`0400` Global / `0444`
/// System) via strictly non-overwriting `std::fs::hard_link` (`ADR-RB-02` #2
/// Step F1, `RB-03d`/`RB-03f`).
///
/// Never creates a 0-byte `config.db` and never falls back to overwriting
/// `rename` if the filesystem rejects `hard_link`.
pub fn reserve_config_sqlite_tombstone_hard_link(
    scope_dir: &Path,
    role: DatabaseRole,
) -> StorageResult<TombstoneReserveOutcome> {
    reserve_config_sqlite_tombstone_hard_link_with_ops(scope_dir, role, &OsTombstoneHardLinkOps)
}

fn ensure_scope_dir_exists_for_write(scope_dir: &Path, role: DatabaseRole) -> StorageResult<()> {
    match fs::metadata(scope_dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(StorageBackendError::BackendInternal {
            detail: "config scope path exists and is not a directory".to_string(),
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(scope_dir).is_ok() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "config scope path is a dangling symlink".to_string(),
                });
            }
            fs::create_dir_all(scope_dir).map_err(|create_err| {
                StorageBackendError::io("create config scope directory", create_err)
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let dir_mode = match role {
                    DatabaseRole::GlobalConfig => 0o700,
                    DatabaseRole::SystemConfig => 0o755,
                    _ => 0o700,
                };
                let _ = fs::set_permissions(scope_dir, fs::Permissions::from_mode(dir_mode));
            }
            #[cfg(not(unix))]
            let _ = role;
            Ok(())
        }
        Err(err) => Err(StorageBackendError::io("stat config scope directory", err)),
    }
}

#[cfg(feature = "storage-redb")]
fn fresh_create_redb_config_scope_under_ex_lock<FBeforeLink, FAfterReserve>(
    scope_dir: &Path,
    role: DatabaseRole,
    before_hard_link: FBeforeLink,
    after_tombstone_before_redb: FAfterReserve,
) -> StorageResult<Arc<dyn ConfigStore>>
where
    FBeforeLink: FnOnce(),
    FAfterReserve: FnOnce(&Path),
{
    let redb_path = config_redb_path_in_scope_dir(scope_dir);
    let redb_tmp_path = scope_dir.join(format!("{CONFIG_REDB_FILE_NAME}.tmp"));

    // Step F1: Reserve `config.db` sentinel BEFORE creating `config.redb`.
    before_hard_link();
    match reserve_config_sqlite_tombstone_hard_link(scope_dir, role)? {
        TombstoneReserveOutcome::Reserved => {}
        TombstoneReserveOutcome::AlreadyExists => {
            return Err(StorageBackendError::BackendMismatch {
                detail: format!(
                    "{role} legacy config.db already exists (created concurrently by a legacy binary); refusing to create config.redb — run `libra-migrate`"
                ),
            });
        }
    }
    after_tombstone_before_redb(scope_dir);

    // Step F2: Persist `init_phase="fresh_create", finalized=false, targets_staged_for_rename=false`.
    let step_f2_manifest = ConfigFormatManifest::new_redb_fresh_create(1, false, role)?;
    publish_config_format_manifest(scope_dir, role, &step_f2_manifest)?;

    // Step F3: Stage `config.redb.tmp` (with exact role mode `0600`/`0644`),
    // publish `targets_staged_for_rename=true`, and atomically rename to `config.redb`.
    if let Ok(meta) = fs::symlink_metadata(&redb_tmp_path) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(StorageBackendError::BackendInternal {
                detail: "refusing to overwrite non-regular config.redb.tmp".to_string(),
            });
        }
        fs::remove_file(&redb_tmp_path)
            .map_err(|err| StorageBackendError::io("remove stale config.redb.tmp", err))?;
    }
    if fs::symlink_metadata(&redb_path).is_ok() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("{role} config.redb unexpectedly exists during fresh creation"),
        });
    }

    {
        let staged = crate::storage::redb::RedbConfigStore::create(&redb_tmp_path, role)?;
        drop(staged);
    }

    let step_f3_manifest = ConfigFormatManifest::new_redb_fresh_create(1, true, role)?;
    publish_config_format_manifest(scope_dir, role, &step_f3_manifest)?;

    durable_atomic_rename_config_manifest(&redb_tmp_path, &redb_path)?;
    fsync_dir(scope_dir)?;

    // Step F4: Publish terminal `finalized=true, init_phase=null, legacy_sqlite_tombstone=true`.
    let step_f4_manifest = ConfigFormatManifest::new_redb_finalized(1, true, role)?;
    publish_config_format_manifest(scope_dir, role, &step_f4_manifest)?;

    let store = crate::storage::redb::RedbConfigStore::open_read_write(&redb_path, role)?;
    Ok(Arc::new(store))
}

/// Open or initialize a `GlobalConfig` or `SystemConfig` scope for writing
/// under `ADR-RB-02` #2's double-checked bounded locking and Step F1–F4
/// protocol (`RB-03d`/`RB-03f`/`RB-03e`/`RB-03c`/`RB-03g`).
///
/// - Steady-state finalized scope matching `runtime_backend`: acquires bounded
///   `LOCK_SH` so same-backend writers execute concurrently while mutually
///   excluding `LOCK_EX` migrations.
/// - Missing `config-format.json` (empty dir or unmanifested legacy store):
///   acquires bounded `LOCK_EX`, re-validates on-disk state under `LOCK_EX`,
///   and serializes first-write creation so competing `Sqlite` and `Redb`
///   writers produce exactly one single-authority scope (`LBR-CLI-003` for the
///   non-authoritative runtime).
/// - For fresh `Redb` creation in an empty directory, Step F1 atomically
///   reserves `config.db` via non-overwriting `hard_link` BEFORE creating
///   `config.redb`, yielding cleanly (`LBR-CLI-003`) to any un-locked legacy
///   binary that created `config.db` first.
pub async fn open_or_create_locked_config_scope_for_write(
    scope_dir: &Path,
    role: DatabaseRole,
    runtime_backend: StorageBackendKind,
    lock_deadline: Duration,
) -> StorageResult<LockedConfigScope> {
    open_or_create_locked_config_scope_for_write_with_hooks(
        scope_dir,
        role,
        runtime_backend,
        lock_deadline,
        || {},
        |_| {},
        || {},
    )
    .await
}

async fn open_or_create_locked_config_scope_for_write_with_hooks<
    FBeforeLink,
    FAfterReserve,
    FBeforeSqliteManifest,
>(
    scope_dir: &Path,
    role: DatabaseRole,
    runtime_backend: StorageBackendKind,
    lock_deadline: Duration,
    before_hard_link: FBeforeLink,
    after_tombstone_before_redb: FAfterReserve,
    before_sqlite_manifest: FBeforeSqliteManifest,
) -> StorageResult<LockedConfigScope>
where
    FBeforeLink: FnOnce() + Send + 'static,
    FAfterReserve: FnOnce(&Path) + Send + 'static,
    FBeforeSqliteManifest: FnOnce() + Send + 'static,
{
    validate_config_scope_role(role)?;

    // Reject a runtime backend that is not compiled into this binary immediately.
    match runtime_backend {
        StorageBackendKind::Sqlite => {
            #[cfg(not(feature = "storage-sqlite"))]
            return Err(StorageBackendError::UnsupportedBackend {
                requested: StorageBackendKind::Sqlite.to_string(),
                detail: "sqlite storage backend is not compiled into this binary".to_string(),
            });
        }
        StorageBackendKind::Redb => {
            #[cfg(not(feature = "storage-redb"))]
            return Err(StorageBackendError::UnsupportedBackend {
                requested: StorageBackendKind::Redb.to_string(),
                detail: "redb storage backend is not compiled into this binary".to_string(),
            });
        }
    }

    let dir_owned = scope_dir.to_path_buf();

    enum LockPlan {
        Existing {
            lock: ConfigScopeLock,
            authority: ConfigScopeAuthority,
        },
        FreshCreateUnderExclusive {
            lock: ConfigScopeLock,
        },
    }

    let plan = tokio::task::spawn_blocking(move || {
        ensure_scope_dir_exists_for_write(&dir_owned, role)?;

        // Peek `config-format.json` to decide whether steady-state `LOCK_SH`
        // suffices or first-write/unmanifested `LOCK_EX` serialization is needed.
        if let Some(peek) = read_config_format_manifest(&dir_owned, role)?
            && peek.finalized
        {
            let lock = ConfigScopeLock::try_lock_shared_bounded(&dir_owned, role, lock_deadline)?;
            let authority = inspect_config_scope_authority(&dir_owned, role)?;
            // If manifest disappeared or changed while acquiring LOCK_SH, re-evaluate cleanly.
            if matches!(authority, ConfigScopeAuthority::Manifest { .. }) {
                return Ok(LockPlan::Existing { lock, authority });
            }
            drop(lock);
        }

        // First-write / missing manifest / unfinalized state: serialize strictly under `LOCK_EX`.
        let lock = ConfigScopeLock::try_lock_exclusive_bounded(&dir_owned, role, lock_deadline)?;
        let authority = inspect_config_scope_authority(&dir_owned, role)?;
        match authority {
            ConfigScopeAuthority::Empty => Ok(LockPlan::FreshCreateUnderExclusive { lock }),
            other => Ok(LockPlan::Existing {
                lock,
                authority: other,
            }),
        }
    })
    .await
    .map_err(|err| StorageBackendError::BackendInternal {
        detail: format!("config scope write lock task failed: {err}"),
    })??;

    match plan {
        LockPlan::Existing { lock, authority } => match authority {
            ConfigScopeAuthority::Manifest { manifest, db_path } => {
                if manifest.backend != runtime_backend {
                    return Err(StorageBackendError::BackendMismatch {
                        detail: format!(
                            "{role} config scope authority is `{}` (finalized), incompatible with `{runtime_backend}` writer; run `libra-migrate`",
                            manifest.backend
                        ),
                    });
                }
                let store = open_store_from_authority(
                    ConfigScopeAuthority::Manifest { manifest, db_path },
                    role,
                    true,
                )
                .await?;
                Ok(LockedConfigScope {
                    store: Arc::new(ScopeLockedConfigStore {
                        inner: store,
                        lock: Some(lock),
                    }),
                    role,
                })
            }
            ConfigScopeAuthority::LegacySqlite { db_path } => match runtime_backend {
                StorageBackendKind::Sqlite => {
                    let store = open_store_from_authority(
                        ConfigScopeAuthority::LegacySqlite { db_path },
                        role,
                        true,
                    )
                    .await?;
                    Ok(LockedConfigScope {
                        store: Arc::new(ScopeLockedConfigStore {
                            inner: store,
                            lock: Some(lock),
                        }),
                        role,
                    })
                }
                StorageBackendKind::Redb => Err(StorageBackendError::BackendMismatch {
                    detail: format!(
                        "{role} config scope has an unmigrated legacy config.db; refusing to create config.redb — run `libra-migrate`"
                    ),
                }),
            },
            ConfigScopeAuthority::UnmanifestedRedb { .. } => match runtime_backend {
                StorageBackendKind::Sqlite => Err(StorageBackendError::BackendMismatch {
                    detail: format!(
                        "{role} config scope contains config.redb without config.db; incompatible with sqlite writer"
                    ),
                }),
                StorageBackendKind::Redb => Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "{role} config scope contains unmanifested config.redb without config-format.json"
                    ),
                }),
            },
            ConfigScopeAuthority::Empty => Err(StorageBackendError::BackendInternal {
                detail: "unexpected empty scope after existing-authority classification"
                    .to_string(),
            }),
        },
        LockPlan::FreshCreateUnderExclusive { lock } => match runtime_backend {
            StorageBackendKind::Sqlite => {
                #[cfg(feature = "storage-sqlite")]
                {
                    let sqlite_path = config_sqlite_path_in_scope_dir(scope_dir);
                    let dir_for_manifest = scope_dir.to_path_buf();
                    // Run SQLite creation + finalized manifest publication inside
                    // an inner spawned task owning `lock` across BOTH steps so
                    // cancelling the outer caller future cannot drop `LOCK_EX`
                    // while `config.db` exists without a finalized manifest.
                    let (lock, store) = tokio::spawn(async move {
                        tokio::task::spawn_blocking(before_sqlite_manifest)
                            .await
                            .map_err(|err| StorageBackendError::BackendInternal {
                                detail: format!("sqlite pre-create hook task failed: {err}"),
                            })?;
                        let store =
                            crate::storage::sqlite::SqliteConfigStore::create(&sqlite_path, role)
                                .await?;
                        let lock = tokio::task::spawn_blocking(move || {
                            let manifest = ConfigFormatManifest::new_sqlite_finalized(1, role)?;
                            publish_config_format_manifest(&dir_for_manifest, role, &manifest)?;
                            Ok::<ConfigScopeLock, StorageBackendError>(lock)
                        })
                        .await
                        .map_err(|err| {
                            StorageBackendError::BackendInternal {
                                detail: format!(
                                    "sqlite config manifest publish task failed: {err}"
                                ),
                            }
                        })??;
                        Ok::<
                            (ConfigScopeLock, crate::storage::sqlite::SqliteConfigStore),
                            StorageBackendError,
                        >((lock, store))
                    })
                    .await
                    .map_err(|err| {
                        StorageBackendError::BackendInternal {
                            detail: format!("sqlite fresh config create task failed: {err}"),
                        }
                    })??;
                    Ok(LockedConfigScope {
                        store: Arc::new(ScopeLockedConfigStore {
                            inner: Arc::new(store),
                            lock: Some(lock),
                        }),
                        role,
                    })
                }
                #[cfg(not(feature = "storage-sqlite"))]
                {
                    let _ = (lock, before_sqlite_manifest);
                    Err(StorageBackendError::UnsupportedBackend {
                        requested: StorageBackendKind::Sqlite.to_string(),
                        detail: "sqlite storage backend is not compiled into this binary"
                            .to_string(),
                    })
                }
            }
            StorageBackendKind::Redb => {
                #[cfg(feature = "storage-redb")]
                {
                    let dir_for_create = scope_dir.to_path_buf();
                    // Move `lock` INTO `spawn_blocking` so `LOCK_EX` cannot be
                    // released prematurely if the async caller is cancelled while
                    // Step F1..F4 creation is still running.
                    let (lock, store) = tokio::task::spawn_blocking(move || {
                        let store = fresh_create_redb_config_scope_under_ex_lock(
                            &dir_for_create,
                            role,
                            before_hard_link,
                            after_tombstone_before_redb,
                        )?;
                        Ok::<(ConfigScopeLock, Arc<dyn ConfigStore>), StorageBackendError>((
                            lock, store,
                        ))
                    })
                    .await
                    .map_err(|err| {
                        StorageBackendError::BackendInternal {
                            detail: format!("redb fresh config create task failed: {err}"),
                        }
                    })??;
                    Ok(LockedConfigScope {
                        store: Arc::new(ScopeLockedConfigStore {
                            inner: store,
                            lock: Some(lock),
                        }),
                        role,
                    })
                }
                #[cfg(not(feature = "storage-redb"))]
                {
                    let _ = (lock, before_hard_link, after_tombstone_before_redb);
                    Err(StorageBackendError::UnsupportedBackend {
                        requested: StorageBackendKind::Redb.to_string(),
                        detail: "redb storage backend is not compiled into this binary".to_string(),
                    })
                }
            }
        },
    }
}

/// One flat configuration entry stored in `config_kv`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEntry {
    /// Full dotted config key, e.g. `remote.origin.fetch` or `user.name`.
    pub key: String,
    /// Plaintext value, or hex-encoded vault ciphertext when `encrypted == true`.
    pub value: String,
    /// Whether `value` is vault-encrypted ciphertext.
    pub encrypted: bool,
}

/// One legacy 3-column configuration row stored in `config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyConfigEntry {
    /// Section name (`[configuration "name"]`).
    pub configuration: String,
    /// Optional subsection name.
    pub name: Option<String>,
    /// Variable name within the section.
    pub key: String,
    /// Variable value.
    pub value: String,
}

/// One schema migration receipt stored in `schema_versions` (repository) or
/// `configuration_schema_versions` (global/system configuration).
///
/// Initial migration receipts written at store creation record an RFC 3339 UTC
/// timestamp (`chrono::Utc::now().to_rfc3339()`), matching
/// `src/internal/db/migration.rs` and `src/internal/db/schema.rs`. Explicit
/// receipts persisted via [`ConfigStore::record_schema_receipt`] round-trip
/// `applied_at` verbatim across both SQLite and `redb`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaVersionReceipt {
    /// Monotonic numeric schema migration version.
    pub version: i64,
    /// Human-readable migration name.
    pub name: String,
    /// RFC 3339 UTC timestamp string recording when the receipt was applied.
    pub applied_at: String,
}

/// One scoped metadata record stored in `metadata_kv`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataEntryRecord {
    /// Metadata scope (e.g. `"branch"`, `"agent_traces_inflight"`).
    pub scope: String,
    /// Opaque scope target (e.g. local branch short name or session ID).
    pub target: String,
    /// Metadata key within `(scope, target)`.
    pub key: String,
    /// Metadata value payload.
    pub value: String,
    /// Type tag (e.g. `"text"`, `"integer"`).
    pub value_type: String,
    /// ISO-8601 UTC creation timestamp.
    pub created_at: String,
    /// ISO-8601 UTC last-update timestamp.
    pub updated_at: String,
}

/// Result of an atomic compare-and-swap operation on `metadata_kv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataCasOutcome {
    /// The expected value matched and the mutation committed atomically.
    Swapped {
        /// Prior record before the swap (`None` for insert-if-absent).
        previous: Option<MetadataEntryRecord>,
        /// Resulting record after the swap (`None` for compare-and-delete).
        current: Option<MetadataEntryRecord>,
    },
    /// The expected value did not match; no write occurred.
    Conflict {
        /// Actual record currently stored in `metadata_kv`.
        actual: Option<MetadataEntryRecord>,
    },
}

/// Validate that `role` is allowed to back a [`ConfigStore`] (`Repository`,
/// `GlobalConfig`, or `SystemConfig`).
pub(crate) fn validate_config_role(role: DatabaseRole) -> StorageResult<()> {
    match role {
        DatabaseRole::Repository | DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig => {
            Ok(())
        }
        other => Err(StorageBackendError::BackendInternal {
            detail: format!("{other} database role cannot back a ConfigStore"),
        }),
    }
}

/// Validate that `role` is [`DatabaseRole::Repository`] before accessing
/// repository-only `metadata_kv`.
pub(crate) fn validate_metadata_role(role: DatabaseRole) -> StorageResult<()> {
    if role != DatabaseRole::Repository {
        return Err(StorageBackendError::RoleMismatch {
            expected: DatabaseRole::Repository,
            found: role,
        });
    }
    Ok(())
}

/// Validate a Git section or `section.subsection` identifier for
/// `remove_section` / `rename_section`.
pub(crate) fn validate_section_name(section: &str) -> StorageResult<()> {
    if section.is_empty() || section.starts_with('.') || section.ends_with('.') {
        return Err(StorageBackendError::BackendInternal {
            detail: "invalid config section name".to_string(),
        });
    }
    Ok(())
}

/// Returns whether `key` belongs to Git `section` (`section` or
/// `section.subsection`), matching Git's exact section/subsection identity
/// (`src/command/config.rs::key_in_section`).
///
/// Bare section `"branch"` matches `"branch.autosetup"` but NOT
/// `"branch.main.remote"` (which belongs to subsection `"branch.main"`).
#[must_use]
pub fn key_in_section(key: &str, section: &str) -> bool {
    if section.is_empty() || section.starts_with('.') || section.ends_with('.') {
        return false;
    }
    let Some((key_sec, key_rest)) = key.split_once('.') else {
        return false;
    };
    let (target_sec, target_sub) = match section.split_once('.') {
        Some((s, sub)) => (s, Some(sub)),
        None => (section, None),
    };
    if key_sec != target_sec {
        return false;
    }
    match key_rest.rsplit_once('.') {
        Some((key_sub, var_name)) => !var_name.is_empty() && target_sub == Some(key_sub),
        None => !key_rest.is_empty() && target_sub.is_none(),
    }
}

/// Object-safe configuration store abstraction backed by SQLite or `redb 5.0`
/// (`ADR-RB-04` #1).
#[async_trait]
pub trait ConfigStore: Send + Sync + std::fmt::Debug {
    /// Owner [`DatabaseRole`] (`Repository`, `GlobalConfig`, or `SystemConfig`).
    fn role(&self) -> DatabaseRole;

    /// Active storage engine backend.
    fn backend(&self) -> StorageBackendKind;

    /// Return the last-written value for exact `key` (`None` when absent).
    async fn get(&self, key: &str) -> StorageResult<Option<ConfigEntry>>;

    /// Return all values for exact `key` in ascending insertion order.
    async fn get_all(&self, key: &str) -> StorageResult<Vec<ConfigEntry>>;

    /// Return all entries whose key starts with `prefix`, ordered by `(key ASC, id ASC)`.
    async fn get_by_prefix(&self, prefix: &str) -> StorageResult<Vec<ConfigEntry>>;

    /// Return all entries in ascending insertion order.
    async fn list_all(&self) -> StorageResult<Vec<ConfigEntry>>;

    /// Upsert a single-valued key, replacing any other-cased ASCII variants.
    ///
    /// Fails closed if multiple exact values already exist, or if `encrypted ==
    /// false` while an existing exact or other-cased variant is already marked
    /// `encrypted == true` (callers must encrypt first and pass `encrypted =
    /// true` so plaintext is never persisted under an encrypted flag).
    async fn set(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()>;

    /// Append a value for `key` (supports multi-value keys).
    ///
    /// Enforces strict same-key encryption homogeneity: fails closed if
    /// `encrypted` conflicts with existing entries for `key` (both plaintext
    /// appended to an encrypted key and encrypted appended to a plaintext key
    /// are rejected).
    async fn add(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()>;

    /// Delete the single entry for `key` (returns `0` or `1`; fails closed if
    /// multiple rows exist).
    async fn unset(&self, key: &str) -> StorageResult<usize>;

    /// Delete every entry for exact `key` and return the number removed.
    async fn unset_all(&self, key: &str) -> StorageResult<usize>;

    /// Atomically remove every entry belonging to `section` (`key_in_section`).
    /// Returns `Err(NotFound)` when no keys belong to `section`.
    async fn remove_section(&self, section: &str) -> StorageResult<usize>;

    /// Atomically rename every entry in `old_section` to `new_section`,
    /// preserving values, multi-value order, and encryption flags.
    async fn rename_section(&self, old_section: &str, new_section: &str) -> StorageResult<usize>;

    /// List legacy 3-column `config` rows in insertion order.
    async fn list_legacy_entries(&self) -> StorageResult<Vec<LegacyConfigEntry>>;

    /// Insert one legacy 3-column `config` row.
    async fn insert_legacy_entry(&self, entry: &LegacyConfigEntry) -> StorageResult<()>;

    /// List schema ledger receipts (`schema_versions` or `configuration_schema_versions`)
    /// ordered by `version ASC`. Initial migration receipts carry RFC 3339 UTC
    /// `applied_at` timestamps; caller-supplied receipts preserve `applied_at`
    /// verbatim.
    async fn list_schema_receipts(&self) -> StorageResult<Vec<SchemaVersionReceipt>>;

    /// Upsert a schema ledger receipt, preserving `applied_at` verbatim.
    async fn record_schema_receipt(&self, receipt: &SchemaVersionReceipt) -> StorageResult<()>;
}

/// Object-safe scoped metadata KV store abstraction (`metadata_kv`, `ADR-RB-04` #1).
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Get one `(scope, target, key)` metadata entry, or `None` when absent.
    async fn get_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
    ) -> StorageResult<Option<MetadataEntryRecord>>;

    /// Upsert `(scope, target, key)` and return the previous value if one existed.
    async fn set_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
        value: &str,
        value_type: &str,
    ) -> StorageResult<Option<String>>;

    /// Atomically compare `current.value` against `expected` and either upsert
    /// `new_value = Some((value, value_type))` or delete `new_value = None`.
    async fn cas_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
        expected: Option<&str>,
        new_value: Option<(&str, &str)>,
    ) -> StorageResult<MetadataCasOutcome>;

    /// Delete `(scope, target, key)` and return whether a row was removed.
    async fn unset_metadata(&self, scope: &str, target: &str, key: &str) -> StorageResult<bool>;

    /// List entries for `(scope, target)` ordered by `key ASC`, optionally
    /// filtered by `key_prefix`.
    async fn list_metadata(
        &self,
        scope: &str,
        target: &str,
        key_prefix: Option<&str>,
    ) -> StorageResult<Vec<MetadataEntryRecord>>;

    /// List every entry in `scope` across all targets ordered by `(target ASC, key ASC)`.
    async fn list_metadata_scope(&self, scope: &str) -> StorageResult<Vec<MetadataEntryRecord>>;

    /// Delete every entry for `(scope, target)` and return the removed count.
    async fn delete_metadata_target(&self, scope: &str, target: &str) -> StorageResult<u64>;

    /// Atomically move all entries from `old_target` to `new_target` (replacing
    /// any pre-existing entries on `new_target`).
    async fn rename_metadata_target(
        &self,
        scope: &str,
        old_target: &str,
        new_target: &str,
    ) -> StorageResult<()>;

    /// Atomically copy all entries from `from_target` to `to_target` (replacing
    /// any pre-existing entries on `to_target`; self-copy is a no-op).
    async fn copy_metadata_target(
        &self,
        scope: &str,
        from_target: &str,
        to_target: &str,
    ) -> StorageResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn dual_backend_config_and_metadata_equivalence() {
        use std::sync::Arc;

        use tempfile::tempdir;

        use crate::{
            internal::vault::{decrypt_token, encrypt_token},
            storage::{redb::RedbConfigStore, sqlite::SqliteConfigStore},
        };

        let test_unseal_key = [0x5Au8; 32];
        let encrypt_hex = |plaintext: &str| -> String {
            let ct =
                encrypt_token(&test_unseal_key, plaintext.as_bytes()).expect("encrypt test secret");
            hex::encode(ct)
        };
        let decrypt_hex = |ciphertext_hex: &str| -> String {
            let bytes = hex::decode(ciphertext_hex).expect("decode hex ciphertext");
            decrypt_token(&test_unseal_key, &bytes).expect("decrypt persisted ciphertext")
        };

        let ct_case_mix_1 = encrypt_hex("super-secret-case-1");
        let ct_case_mix_2 = encrypt_hex("super-secret-case-2");
        let ct_vault_tok_1 = encrypt_hex("api-token-secret-one");
        let ct_vault_tok_2 = encrypt_hex("api-token-secret-two");

        let dir = tempdir().expect("create temp dir for dual-backend equivalence");
        let sqlite_path = dir.path().join("libra.db");
        let redb_path = dir.path().join("git.gen-00000001.redb");

        let repo_before_create = chrono::Utc::now();
        let sqlite_store = SqliteConfigStore::create(&sqlite_path, DatabaseRole::Repository)
            .await
            .expect("create SqliteConfigStore");
        let redb_store = RedbConfigStore::create(&redb_path, DatabaseRole::Repository)
            .expect("create RedbConfigStore");
        let repo_after_create = chrono::Utc::now();

        // Verify object-safety (`ADR-RB-04` #1): both backends work uniformly behind `Arc<dyn Trait>`.
        let cfg_backends: Vec<Arc<dyn ConfigStore>> =
            vec![Arc::new(sqlite_store.clone()), Arc::new(redb_store.clone())];
        let meta_backends: Vec<Arc<dyn MetadataStore>> =
            vec![Arc::new(sqlite_store.clone()), Arc::new(redb_store.clone())];

        assert_eq!(cfg_backends[0].role(), DatabaseRole::Repository);
        assert_eq!(cfg_backends[1].role(), DatabaseRole::Repository);
        assert_eq!(cfg_backends[0].backend(), StorageBackendKind::Sqlite);
        assert_eq!(cfg_backends[1].backend(), StorageBackendKind::Redb);

        // -----------------------------------------------------------------
        // AC1: Multi-value append (`add`), single-value `set` (including
        // case-variant replacement & encryption inheritance), and exact
        // read equivalence across `get` / `get_all` / `get_by_prefix` / `list_all`.
        // -----------------------------------------------------------------
        for store in &cfg_backends {
            store
                .set("user.name", "Alice", false)
                .await
                .expect("set user.name");
            // Case-variant replacement replaces `user.name` with `user.Name`.
            store
                .set("user.Name", "Alice Updated", false)
                .await
                .expect("set user.Name");

            // When BOTH an exact key AND an encrypted other-cased variant
            // coexist (e.g. via `add`), `set` MUST reject plaintext replacement
            // (never persisting plaintext under `encrypted = true`), and once
            // the caller encrypts the replacement (`encrypted = true`), `set`
            // removes the other-cased variant and persists decryptable ciphertext.
            store
                .add("case.mix", "v1", false)
                .await
                .expect("add case.mix plaintext");
            store
                .add("Case.Mix", &ct_case_mix_1, true)
                .await
                .expect("add Case.Mix encrypted");
            let downgrade_err = store
                .set("case.mix", "leaked-plaintext-case", false)
                .await
                .expect_err(
                    "set with plaintext must fail closed when other-cased variant is encrypted",
                );
            assert_eq!(downgrade_err.stable_code_str(), "LBR-IO-001");
            // Existing rows must remain untouched after rejected downgrade.
            assert_eq!(
                store
                    .get_all("case.mix")
                    .await
                    .expect("case.mix untouched after rejected set"),
                vec![ConfigEntry {
                    key: "case.mix".to_string(),
                    value: "v1".to_string(),
                    encrypted: false,
                }]
            );
            assert_eq!(
                store
                    .get_all("Case.Mix")
                    .await
                    .expect("Case.Mix untouched after rejected set"),
                vec![ConfigEntry {
                    key: "Case.Mix".to_string(),
                    value: ct_case_mix_1.clone(),
                    encrypted: true,
                }]
            );

            // Caller encrypts before calling `set(..., true)`: succeeds, removes
            // `Case.Mix`, and stores decryptable ciphertext.
            store.set("case.mix", &ct_case_mix_2, true).await.expect(
                "set case.mix with encrypted ciphertext must succeed and collapse Case.Mix",
            );
            let case_mix_after = store
                .get_all("case.mix")
                .await
                .expect("get_all case.mix after encrypted set");
            assert_eq!(
                case_mix_after,
                vec![ConfigEntry {
                    key: "case.mix".to_string(),
                    value: ct_case_mix_2.clone(),
                    encrypted: true,
                }]
            );
            assert_eq!(decrypt_hex(&case_mix_after[0].value), "super-secret-case-2");
            assert_eq!(
                store
                    .get_all("Case.Mix")
                    .await
                    .expect("Case.Mix must be removed by set(case.mix)"),
                Vec::new()
            );

            // Direct plaintext overwrite of an exact encrypted key must also fail closed.
            let exact_downgrade_err = store
                .set("case.mix", "leaked-exact-plaintext", false)
                .await
                .expect_err("set with plaintext must fail closed on exact encrypted key");
            assert_eq!(exact_downgrade_err.stable_code_str(), "LBR-IO-001");

            store
                .add("case.plain", "p1", false)
                .await
                .expect("add case.plain");
            store
                .add("CASE.PLAIN", "p2", false)
                .await
                .expect("add CASE.PLAIN");
            store
                .set("case.plain", "p3", false)
                .await
                .expect("set case.plain must remove CASE.PLAIN");
            assert_eq!(
                store
                    .get_all("case.plain")
                    .await
                    .expect("get_all case.plain"),
                vec![ConfigEntry {
                    key: "case.plain".to_string(),
                    value: "p3".to_string(),
                    encrypted: false,
                }]
            );
            assert_eq!(
                store
                    .get_all("CASE.PLAIN")
                    .await
                    .expect("CASE.PLAIN must be removed"),
                Vec::new()
            );

            store
                .add("remote.origin.url", "https://example.com/repo.git", false)
                .await
                .expect("add remote.origin.url");
            store
                .add(
                    "remote.origin.fetch",
                    "+refs/heads/*:refs/remotes/origin/*",
                    false,
                )
                .await
                .expect("add fetch 1");
            store
                .add("remote.origin.fetch", "+refs/tags/*:refs/tags/*", false)
                .await
                .expect("add fetch 2");
            store
                .add("remote.origin.fetch", "+refs/notes/*:refs/notes/*", false)
                .await
                .expect("add fetch 3");

            // `set` must reject multi-valued keys on both backends.
            let multi_set_err = store
                .set("remote.origin.fetch", "single", false)
                .await
                .expect_err("set on multi-valued key must fail");
            assert_eq!(multi_set_err.stable_code_str(), "LBR-IO-001");

            // Mixed plaintext/encrypted `add` must fail closed in BOTH
            // directions (encrypted onto plaintext AND plaintext onto encrypted),
            // while caller-encrypted `add(..., true)` succeeds and round-trips.
            let mix_enc_on_plain_err = store
                .add("remote.origin.url", &ct_vault_tok_1, true)
                .await
                .expect_err("mixing encrypted onto plaintext key must fail");
            assert_eq!(mix_enc_on_plain_err.stable_code_str(), "LBR-IO-001");

            store
                .add("vault.token.api", &ct_vault_tok_1, true)
                .await
                .expect("add encrypted 1");
            let mix_plain_on_enc_err = store
                .add("vault.token.api", "leaked-plaintext-token", false)
                .await
                .expect_err("adding plaintext to an encrypted key must fail closed");
            assert_eq!(mix_plain_on_enc_err.stable_code_str(), "LBR-IO-001");
            store
                .add("vault.token.api", &ct_vault_tok_2, true)
                .await
                .expect("add encrypted 2 with caller ciphertext");

            // Bare section vs subsection isolation fixture for AC2/AC3.
            store
                .set("branch.autosetuprebase", "always", false)
                .await
                .expect("set bare branch key");
            store
                .set("branch.main.remote", "origin", false)
                .await
                .expect("set subsection branch.main.remote");
            store
                .set("branch.main.merge", "refs/heads/main", false)
                .await
                .expect("set subsection branch.main.merge");
        }

        assert_eq!(
            cfg_backends[0].get("user.name").await.expect("sqlite get"),
            cfg_backends[1].get("user.name").await.expect("redb get")
        );
        assert_eq!(
            cfg_backends[0].get("user.Name").await.expect("sqlite get"),
            cfg_backends[1].get("user.Name").await.expect("redb get")
        );
        assert_eq!(
            cfg_backends[0]
                .get("remote.origin.fetch")
                .await
                .expect("sqlite last-one-wins"),
            cfg_backends[1]
                .get("remote.origin.fetch")
                .await
                .expect("redb last-one-wins")
        );
        assert_eq!(
            cfg_backends[0]
                .get_all("remote.origin.fetch")
                .await
                .expect("sqlite get_all"),
            cfg_backends[1]
                .get_all("remote.origin.fetch")
                .await
                .expect("redb get_all")
        );
        assert_eq!(
            cfg_backends[0]
                .get_all("vault.token.api")
                .await
                .expect("sqlite encrypted get_all"),
            cfg_backends[1]
                .get_all("vault.token.api")
                .await
                .expect("redb encrypted get_all")
        );
        let rb_vault_entries = cfg_backends[1]
            .get_all("vault.token.api")
            .await
            .expect("redb encrypted");
        assert!(
            rb_vault_entries.iter().all(|e| e.encrypted),
            "all vault.token.api entries must remain marked encrypted"
        );
        let decrypted_tokens: Vec<String> = rb_vault_entries
            .iter()
            .map(|e| decrypt_hex(&e.value))
            .collect();
        assert_eq!(
            decrypted_tokens,
            vec![
                "api-token-secret-one".to_string(),
                "api-token-secret-two".to_string()
            ],
            "persisted encrypted entries must decrypt cleanly to original secrets"
        );
        // Verify raw database files on disk NEVER contain any secret plaintext
        // or any rejected plaintext downgrade payload.
        for db_file in [&sqlite_path, &redb_path] {
            let raw_bytes = std::fs::read(db_file).expect("read raw db bytes for secret audit");
            for forbidden in [
                b"super-secret-case-1".as_slice(),
                b"super-secret-case-2".as_slice(),
                b"leaked-plaintext-case".as_slice(),
                b"leaked-exact-plaintext".as_slice(),
                b"api-token-secret-one".as_slice(),
                b"api-token-secret-two".as_slice(),
                b"leaked-plaintext-token".as_slice(),
            ] {
                assert!(
                    !raw_bytes.windows(forbidden.len()).any(|w| w == forbidden),
                    "raw storage file {:?} must never contain plaintext secret {:?}",
                    db_file.file_name(),
                    String::from_utf8_lossy(forbidden)
                );
            }
        }
        assert_eq!(
            cfg_backends[0]
                .get_by_prefix("remote.")
                .await
                .expect("sqlite prefix"),
            cfg_backends[1]
                .get_by_prefix("remote.")
                .await
                .expect("redb prefix")
        );
        assert_eq!(
            cfg_backends[0].list_all().await.expect("sqlite list_all"),
            cfg_backends[1].list_all().await.expect("redb list_all")
        );

        // -----------------------------------------------------------------
        // AC2: `rename_section` equivalence (multi-value order preservation,
        // encrypted flag preservation, bare-vs-subsection isolation, and
        // identical rejection of missing source / existing destination / same name).
        // -----------------------------------------------------------------
        let mut rename_counts = Vec::new();
        for store in &cfg_backends {
            let same_err = store
                .rename_section("remote.origin", "remote.origin")
                .await
                .expect_err("identical rename_section must fail");
            assert_eq!(same_err.stable_code_str(), "LBR-IO-001");

            let missing_err = store
                .rename_section("remote.doesnotexist", "remote.upstream")
                .await
                .expect_err("missing source section must fail");
            assert!(matches!(missing_err, StorageBackendError::NotFound { .. }));

            let dest_exists_err = store
                .rename_section("remote.origin", "branch.main")
                .await
                .expect_err("existing destination section must fail");
            assert_eq!(dest_exists_err.stable_code_str(), "LBR-IO-001");

            // Rename `remote.origin` -> `remote.upstream` (1 url + 3 fetch values).
            let moved = store
                .rename_section("remote.origin", "remote.upstream")
                .await
                .expect("rename_section remote.origin -> remote.upstream");
            rename_counts.push(moved);

            // Rename bare section `branch` -> `branchdefaults` must NOT touch `branch.main.*`.
            let bare_moved = store
                .rename_section("branch", "branchdefaults")
                .await
                .expect("rename bare section branch -> branchdefaults");
            assert_eq!(bare_moved, 1);
        }
        assert_eq!(rename_counts, vec![4, 4]);
        assert_eq!(
            cfg_backends[0]
                .get_all("remote.origin.fetch")
                .await
                .expect("sqlite old section empty"),
            Vec::new()
        );
        assert_eq!(
            cfg_backends[0]
                .get_all("remote.upstream.fetch")
                .await
                .expect("sqlite renamed multi-value"),
            cfg_backends[1]
                .get_all("remote.upstream.fetch")
                .await
                .expect("redb renamed multi-value")
        );
        assert_eq!(
            cfg_backends[0]
                .get_by_prefix("branch.")
                .await
                .expect("sqlite subsection intact"),
            cfg_backends[1]
                .get_by_prefix("branch.")
                .await
                .expect("redb subsection intact")
        );
        assert_eq!(
            cfg_backends[0].list_all().await.expect("sqlite list_all"),
            cfg_backends[1].list_all().await.expect("redb list_all")
        );

        // -----------------------------------------------------------------
        // AC3: `unset`, `unset_all`, and `remove_section` equivalence.
        // -----------------------------------------------------------------
        for store in &cfg_backends {
            // `unset` rejects multi-valued key.
            let multi_unset_err = store
                .unset("remote.upstream.fetch")
                .await
                .expect_err("unset on multi-valued key must fail");
            assert_eq!(multi_unset_err.stable_code_str(), "LBR-IO-001");

            // `unset` removes singleton key and returns 0 when already absent.
            assert_eq!(store.unset("user.Name").await.expect("unset user.Name"), 1);
            assert_eq!(store.unset("user.Name").await.expect("unset absent"), 0);

            // `unset_all` removes all multi-values for key.
            assert_eq!(
                store
                    .unset_all("remote.upstream.fetch")
                    .await
                    .expect("unset_all fetch"),
                3
            );
            assert_eq!(
                store
                    .unset_all("remote.upstream.fetch")
                    .await
                    .expect("unset_all absent"),
                0
            );

            // `remove_section` deletes exact section members only and fails with NotFound if absent.
            assert_eq!(
                store
                    .remove_section("branch.main")
                    .await
                    .expect("remove_section branch.main"),
                2
            );
            let missing_rm = store
                .remove_section("branch.main")
                .await
                .expect_err("second remove_section must return NotFound");
            assert!(matches!(missing_rm, StorageBackendError::NotFound { .. }));
        }

        assert_eq!(
            cfg_backends[0]
                .list_all()
                .await
                .expect("sqlite post-delete"),
            cfg_backends[1].list_all().await.expect("redb post-delete")
        );

        // Literal case-sensitive and SQL-wildcard (`%`, `_`) prefix,
        // `rename_section`, and `remove_section` equivalence (`Sqlite` LIKE
        // wildcards/case-folding must NEVER leak into any section or prefix op).
        for store in &cfg_backends {
            store
                .set("prefix.lower", "v-lower", false)
                .await
                .expect("set prefix.lower");
            store
                .set("Prefix.upper", "v-upper", false)
                .await
                .expect("set Prefix.upper");
            store
                .set("a_b.exact", "v-under", false)
                .await
                .expect("set a_b.exact");
            store
                .set("axb.other", "v-x", false)
                .await
                .expect("set axb.other");
            store
                .set("a%b.exact", "v-pct", false)
                .await
                .expect("set a%b.exact");
            store
                .set("aZZZb.other", "v-zzz", false)
                .await
                .expect("set aZZZb.other");

            // Section fixtures exercising ASCII case, `_`, and `%` in
            // `rename_section` and `remove_section`. Use `add` for `Sec_a.k1`
            // so both `sec_a.k1` and `Sec_a.k1` coexist literally (`set`
            // replaces other-cased variants of the same key).
            store
                .set("sec_a.k1", "v-under-1", false)
                .await
                .expect("set sec_a.k1");
            store
                .set("sec_a.k2", "v-under-2", false)
                .await
                .expect("set sec_a.k2");
            store
                .set("secXa.k1", "v-x", false)
                .await
                .expect("set secXa.k1");
            store
                .add("Sec_a.k1", "v-upper-sec", false)
                .await
                .expect("add Sec_a.k1");
            store
                .set("sec%b.k1", "v-pct", false)
                .await
                .expect("set sec%b.k1");
            store
                .set("secZZZb.k1", "v-zzz", false)
                .await
                .expect("set secZZZb.k1");
            store
                .set("SEC_NEW.k1", "v-upper-dest", false)
                .await
                .expect("set SEC_NEW.k1");
        }
        for prefix in ["prefix.", "Prefix.", "a_b.", "a%b."] {
            let sq = cfg_backends[0]
                .get_by_prefix(prefix)
                .await
                .expect("sqlite literal prefix");
            let rb = cfg_backends[1]
                .get_by_prefix(prefix)
                .await
                .expect("redb literal prefix");
            assert_eq!(sq, rb, "prefix `{prefix}` diverged between SQLite and Redb");
            assert_eq!(
                sq.len(),
                1,
                "literal prefix `{prefix}` must match exactly 1 key (no case/wildcard leak): {sq:?}"
            );
        }

        for store in &cfg_backends {
            // Exact destination collision must fail closed.
            let dest_collision = store
                .rename_section("sec_a", "sec%b")
                .await
                .expect_err("rename_section onto existing literal sec%b must fail");
            assert_eq!(dest_collision.stable_code_str(), "LBR-IO-001");

            // Case-distinct destination `sec_new` must NOT collide with `SEC_NEW`,
            // and `_` in `sec_a` must NOT touch `secXa` or `Sec_a`.
            assert_eq!(
                store
                    .rename_section("sec_a", "sec_new")
                    .await
                    .expect("rename_section sec_a -> sec_new"),
                2
            );
            // `%` in `sec%b` must rename ONLY `sec%b` and NOT touch `secZZZb`.
            assert_eq!(
                store
                    .rename_section("sec%b", "sec%c")
                    .await
                    .expect("rename_section sec%b -> sec%c"),
                1
            );
        }
        assert_eq!(
            cfg_backends[0]
                .list_all()
                .await
                .expect("sqlite list_all after case/wildcard rename_section"),
            cfg_backends[1]
                .list_all()
                .await
                .expect("redb list_all after case/wildcard rename_section")
        );
        for store in &cfg_backends {
            assert_eq!(
                store
                    .get("secXa.k1")
                    .await
                    .expect("get secXa.k1")
                    .map(|e| e.value),
                Some("v-x".to_string())
            );
            assert_eq!(
                store
                    .get("Sec_a.k1")
                    .await
                    .expect("get Sec_a.k1")
                    .map(|e| e.value),
                Some("v-upper-sec".to_string())
            );
            assert_eq!(
                store
                    .get("SEC_NEW.k1")
                    .await
                    .expect("get SEC_NEW.k1")
                    .map(|e| e.value),
                Some("v-upper-dest".to_string())
            );
            assert_eq!(
                store
                    .get("secZZZb.k1")
                    .await
                    .expect("get secZZZb.k1")
                    .map(|e| e.value),
                Some("v-zzz".to_string())
            );
            assert_eq!(
                store
                    .get("sec%c.k1")
                    .await
                    .expect("get sec%c.k1")
                    .map(|e| e.value),
                Some("v-pct".to_string())
            );

            // `remove_section` must delete ONLY exact case & literal `_`/`%` section members.
            assert_eq!(
                store
                    .remove_section("sec_new")
                    .await
                    .expect("remove_section sec_new"),
                2
            );
            assert_eq!(
                store
                    .remove_section("sec%c")
                    .await
                    .expect("remove_section sec%c"),
                1
            );
            assert_eq!(
                store
                    .remove_section("Sec_a")
                    .await
                    .expect("remove_section Sec_a"),
                1
            );
            assert!(
                store
                    .get("SEC_NEW.k1")
                    .await
                    .expect("SEC_NEW must survive remove_section(sec_new)")
                    .is_some()
            );
            assert!(
                store
                    .get("secZZZb.k1")
                    .await
                    .expect("secZZZb must survive remove_section(sec%c)")
                    .is_some()
            );
            assert!(
                store
                    .get("secXa.k1")
                    .await
                    .expect("secXa must survive remove_section(Sec_a)")
                    .is_some()
            );
        }
        assert_eq!(
            cfg_backends[0]
                .list_all()
                .await
                .expect("sqlite list_all after case/wildcard remove_section"),
            cfg_backends[1]
                .list_all()
                .await
                .expect("redb list_all after case/wildcard remove_section")
        );

        // Legacy `config` table and COMPLETE initial + updated schema receipts
        // equivalence across Repository, GlobalConfig, and SystemConfig roles.
        let validate_initial_receipts = |receipts: &[SchemaVersionReceipt],
                                         before: chrono::DateTime<chrono::Utc>,
                                         after: chrono::DateTime<chrono::Utc>|
         -> Vec<(i64, String)> {
            let min_ts = before - chrono::Duration::seconds(2);
            let max_ts = after + chrono::Duration::seconds(2);
            assert!(
                !receipts.is_empty(),
                "fresh store must record initial schema migration receipts"
            );
            for r in receipts {
                let parsed = chrono::DateTime::parse_from_rfc3339(&r.applied_at).unwrap_or_else(
                    |err| {
                        panic!(
                            "schema receipt version={} applied_at `{}` is not valid RFC 3339: {err}",
                            r.version, r.applied_at
                        )
                    },
                );
                assert_eq!(
                    parsed.offset().local_minus_utc(),
                    0,
                    "schema receipt applied_at `{}` must be UTC",
                    r.applied_at
                );
                let utc = parsed.with_timezone(&chrono::Utc);
                assert!(
                    utc >= min_ts && utc <= max_ts,
                    "schema receipt applied_at `{}` outside creation window [{min_ts}, {max_ts}]",
                    r.applied_at
                );
            }
            receipts
                .iter()
                .map(|r| (r.version, r.name.clone()))
                .collect()
        };

        let sq_repo_init = cfg_backends[0]
            .list_schema_receipts()
            .await
            .expect("sqlite initial repo receipts");
        let rb_repo_init = cfg_backends[1]
            .list_schema_receipts()
            .await
            .expect("redb initial repo receipts");
        assert_eq!(
            validate_initial_receipts(&sq_repo_init, repo_before_create, repo_after_create),
            validate_initial_receipts(&rb_repo_init, repo_before_create, repo_after_create),
            "fresh Repository stores must have identical initial schema receipts"
        );

        let legacy_row = LegacyConfigEntry {
            configuration: "remote".to_string(),
            name: Some("upstream".to_string()),
            key: "url".to_string(),
            value: "https://example.com/repo.git".to_string(),
        };
        let extra_repo_receipt = SchemaVersionReceipt {
            version: 2026112201,
            name: "rb03_test_receipt".to_string(),
            applied_at: "2026-11-22T00:00:00Z".to_string(),
        };
        for store in &cfg_backends {
            store
                .insert_legacy_entry(&legacy_row)
                .await
                .expect("insert legacy row");
            store
                .record_schema_receipt(&extra_repo_receipt)
                .await
                .expect("record schema receipt");
        }
        assert_eq!(
            cfg_backends[0]
                .list_legacy_entries()
                .await
                .expect("sqlite legacy"),
            cfg_backends[1]
                .list_legacy_entries()
                .await
                .expect("redb legacy")
        );
        let sq_repo_after = cfg_backends[0]
            .list_schema_receipts()
            .await
            .expect("sqlite repo receipts after append");
        let rb_repo_after = cfg_backends[1]
            .list_schema_receipts()
            .await
            .expect("redb repo receipts after append");
        assert_eq!(
            sq_repo_after.len(),
            sq_repo_init.len() + 1,
            "appending one receipt must grow Repository ledger by exactly 1"
        );
        assert_eq!(sq_repo_after.len(), rb_repo_after.len());
        assert_eq!(
            sq_repo_after.last(),
            Some(&extra_repo_receipt),
            "SQLite must persist recorded SchemaVersionReceipt verbatim including applied_at"
        );
        assert_eq!(
            rb_repo_after.last(),
            Some(&extra_repo_receipt),
            "Redb must persist recorded SchemaVersionReceipt verbatim including applied_at"
        );
        assert_eq!(
            sq_repo_after.last(),
            rb_repo_after.last(),
            "appended Repository receipt must match full struct across backends"
        );
        assert_eq!(
            validate_initial_receipts(
                &sq_repo_after[..sq_repo_init.len()],
                repo_before_create,
                repo_after_create,
            ),
            validate_initial_receipts(
                &rb_repo_after[..rb_repo_init.len()],
                repo_before_create,
                repo_after_create,
            ),
            "initial Repository schema receipts must remain identical after append"
        );

        // -----------------------------------------------------------------
        // AC4: `MetadataStore` `metadata_kv` CAS read/write equivalence.
        // -----------------------------------------------------------------
        let normalize_meta = |r: &MetadataEntryRecord| {
            (
                r.scope.clone(),
                r.target.clone(),
                r.key.clone(),
                r.value.clone(),
                r.value_type.clone(),
            )
        };

        let mut cas_summaries = Vec::new();
        for store in &meta_backends {
            // 1. Assert-absent no-op (None -> None) succeeds on absent key.
            let noop = store
                .cas_metadata("branch", "main", "protect", None, None)
                .await
                .expect("cas None->None on absent");
            assert!(matches!(
                noop,
                MetadataCasOutcome::Swapped {
                    previous: None,
                    current: None
                }
            ));

            // 2. Compare-and-update fails when key is absent.
            let miss = store
                .cas_metadata(
                    "branch",
                    "main",
                    "protect",
                    Some("false"),
                    Some(("true", "text")),
                )
                .await
                .expect("cas Some->Some on absent");
            assert!(matches!(
                miss,
                MetadataCasOutcome::Conflict { actual: None }
            ));

            // 3. Insert-if-absent (None -> Some) succeeds once, conflicts second time.
            let inserted = store
                .cas_metadata("branch", "main", "protect", None, Some(("false", "text")))
                .await
                .expect("cas insert-if-absent");
            let created_ts = match inserted {
                MetadataCasOutcome::Swapped {
                    previous: None,
                    current: Some(rec),
                } => {
                    assert_eq!(rec.value, "false");
                    assert_eq!(rec.value_type, "text");
                    rec.created_at
                }
                other => panic!("expected Swapped on insert-if-absent, got {other:?}"),
            };

            let dup = store
                .cas_metadata("branch", "main", "protect", None, Some(("true", "text")))
                .await
                .expect("duplicate cas insert-if-absent");
            match dup {
                MetadataCasOutcome::Conflict { actual: Some(rec) } => {
                    assert_eq!(rec.value, "false");
                }
                other => panic!("expected Conflict on duplicate insert-if-absent, got {other:?}"),
            }

            // 4. Compare-and-update with wrong expected conflicts; with matching expected swaps
            //    and preserves original `created_at`.
            let wrong = store
                .cas_metadata(
                    "branch",
                    "main",
                    "protect",
                    Some("wrong"),
                    Some(("true", "text")),
                )
                .await
                .expect("cas wrong expected");
            assert!(matches!(
                wrong,
                MetadataCasOutcome::Conflict { actual: Some(_) }
            ));

            let updated = store
                .cas_metadata(
                    "branch",
                    "main",
                    "protect",
                    Some("false"),
                    Some(("true", "text")),
                )
                .await
                .expect("cas matching update");
            match updated {
                MetadataCasOutcome::Swapped {
                    previous: Some(prev),
                    current: Some(curr),
                } => {
                    assert_eq!(prev.value, "false");
                    assert_eq!(curr.value, "true");
                    assert_eq!(
                        curr.created_at, created_ts,
                        "CAS update must preserve created_at"
                    );
                }
                other => panic!("expected Swapped on matching CAS update, got {other:?}"),
            }

            // 5. Additional keys (including case & SQL wildcard literals), target
            //    list/copy/rename, and compare-and-delete.
            store
                .set_metadata("branch", "main", "lineage.parent", "develop", "text")
                .await
                .expect("set lineage.parent");
            store
                .set_metadata("branch", "main", "archive", "false", "text")
                .await
                .expect("set archive");
            store
                .set_metadata("branch", "main", "p_1", "under", "text")
                .await
                .expect("set p_1");
            store
                .set_metadata("branch", "main", "px1", "x", "text")
                .await
                .expect("set px1");
            store
                .set_metadata("branch", "main", "P_1", "upper", "text")
                .await
                .expect("set P_1");
            store
                .set_metadata("branch", "main", "p%1", "pct", "text")
                .await
                .expect("set p%1");
            store
                .set_metadata("branch", "main", "pZZZ1", "zzz", "text")
                .await
                .expect("set pZZZ1");
            store
                .copy_metadata_target("branch", "main", "release")
                .await
                .expect("copy target main -> release");
            store
                .rename_metadata_target("branch", "release", "release-v1")
                .await
                .expect("rename target release -> release-v1");

            let bad_del = store
                .cas_metadata("branch", "main", "protect", Some("false"), None)
                .await
                .expect("cas delete mismatch");
            assert!(matches!(
                bad_del,
                MetadataCasOutcome::Conflict { actual: Some(_) }
            ));

            let good_del = store
                .cas_metadata("branch", "main", "protect", Some("true"), None)
                .await
                .expect("cas delete match");
            assert!(matches!(
                good_del,
                MetadataCasOutcome::Swapped {
                    previous: Some(_),
                    current: None
                }
            ));
            assert_eq!(
                store
                    .get_metadata("branch", "main", "protect")
                    .await
                    .expect("get after cas delete"),
                None
            );

            let scope_rows: Vec<_> = store
                .list_metadata_scope("branch")
                .await
                .expect("list_metadata_scope")
                .iter()
                .map(normalize_meta)
                .collect();
            let prefix_rows: Vec<_> = store
                .list_metadata("branch", "main", Some("lineage."))
                .await
                .expect("list_metadata prefix")
                .iter()
                .map(normalize_meta)
                .collect();
            let under_rows: Vec<_> = store
                .list_metadata("branch", "main", Some("p_"))
                .await
                .expect("list_metadata p_")
                .iter()
                .map(normalize_meta)
                .collect();
            let pct_rows: Vec<_> = store
                .list_metadata("branch", "main", Some("p%"))
                .await
                .expect("list_metadata p%")
                .iter()
                .map(normalize_meta)
                .collect();
            assert_eq!(under_rows.len(), 1, "p_ must not match px1 or P_1");
            assert_eq!(under_rows[0].2, "p_1");
            assert_eq!(pct_rows.len(), 1, "p% must not match pZZZ1");
            assert_eq!(pct_rows[0].2, "p%1");
            cas_summaries.push((scope_rows, prefix_rows, under_rows, pct_rows));
        }

        assert_eq!(
            cas_summaries[0], cas_summaries[1],
            "SqliteConfigStore and RedbConfigStore metadata_kv CAS & CRUD results must match item-for-item"
        );
        assert_eq!(cas_summaries[0].1.len(), 1);
        assert_eq!(cas_summaries[0].1[0].2, "lineage.parent");

        // -----------------------------------------------------------------
        // Concurrent CAS race-freedom regression on BOTH SQLite and Redb:
        // 8 contending tasks race insert-if-absent (`None -> Some`) and then
        // compare-and-update (`Some(winner) -> Some(v2)`) across independent
        // handles, asserting exactly 1 Swapped, 7 Conflicts with matching
        // `actual`, and exact final record.
        // -----------------------------------------------------------------
        const CAS_CONTENDERS: usize = 8;
        let cas_sqlite_path = dir.path().join("cas-race.db");
        let cas_redb_path = dir.path().join("cas-race.redb");
        let _seed_sq = SqliteConfigStore::create(&cas_sqlite_path, DatabaseRole::Repository)
            .await
            .expect("create sqlite db for CAS race");
        let _seed_rb = RedbConfigStore::create(&cas_redb_path, DatabaseRole::Repository)
            .expect("create redb db for CAS race");
        drop(_seed_sq);
        drop(_seed_rb);

        for is_redb in [false, true] {
            let mut handles: Vec<Arc<dyn MetadataStore>> = Vec::with_capacity(CAS_CONTENDERS);
            for _ in 0..CAS_CONTENDERS {
                if is_redb {
                    let h =
                        RedbConfigStore::open_read_write(&cas_redb_path, DatabaseRole::Repository)
                            .expect("open concurrent redb handle for CAS race");
                    handles.push(Arc::new(h));
                } else {
                    let h = SqliteConfigStore::open(&cas_sqlite_path, DatabaseRole::Repository)
                        .await
                        .expect("open concurrent sqlite handle for CAS race");
                    handles.push(Arc::new(h));
                }
            }

            // Phase A: Concurrent insert-if-absent (`None -> Some`).
            let barrier_a = Arc::new(tokio::sync::Barrier::new(CAS_CONTENDERS));
            let mut join_a = tokio::task::JoinSet::new();
            for (idx, handle) in handles.iter().cloned().enumerate() {
                let b = Arc::clone(&barrier_a);
                join_a.spawn(async move {
                    b.wait().await;
                    let candidate = format!("owner-{idx}");
                    let outcome = handle
                        .cas_metadata(
                            "branch",
                            "cas-race",
                            "lease",
                            None,
                            Some((&candidate, "text")),
                        )
                        .await
                        .expect("concurrent cas insert-if-absent must not error");
                    (candidate, outcome)
                });
            }
            let mut swapped_a = Vec::new();
            let mut conflicts_a = Vec::new();
            while let Some(res) = join_a.join_next().await {
                let (candidate, outcome) = res.expect("CAS insert task panicked");
                match outcome {
                    MetadataCasOutcome::Swapped { previous, current } => {
                        assert_eq!(previous, None);
                        let curr = current.expect("Swapped insert must return current record");
                        assert_eq!(curr.value, candidate);
                        swapped_a.push(curr);
                    }
                    MetadataCasOutcome::Conflict { actual } => {
                        conflicts_a.push(actual.expect("loser must observe winning record"));
                    }
                }
            }
            assert_eq!(
                swapped_a.len(),
                1,
                "exactly 1 contender must win insert-if-absent (is_redb={is_redb})"
            );
            assert_eq!(conflicts_a.len(), CAS_CONTENDERS - 1);
            let winner_a = swapped_a.into_iter().next().expect("winner_a");
            for loser_actual in &conflicts_a {
                assert_eq!(
                    loser_actual.value, winner_a.value,
                    "every insert loser must observe winner value"
                );
            }
            let stored_a = handles[0]
                .get_metadata("branch", "cas-race", "lease")
                .await
                .expect("read after insert race")
                .expect("winning insert record must exist");
            assert_eq!(stored_a, winner_a);

            // Phase B: Concurrent compare-and-update (`Some(winner_a) -> Some(v2)`).
            let barrier_b = Arc::new(tokio::sync::Barrier::new(CAS_CONTENDERS));
            let mut join_b = tokio::task::JoinSet::new();
            for (idx, handle) in handles.iter().cloned().enumerate() {
                let b = Arc::clone(&barrier_b);
                let expected_val = winner_a.value.clone();
                join_b.spawn(async move {
                    b.wait().await;
                    let candidate = format!("v2-owner-{idx}");
                    let outcome = handle
                        .cas_metadata(
                            "branch",
                            "cas-race",
                            "lease",
                            Some(&expected_val),
                            Some((&candidate, "text")),
                        )
                        .await
                        .expect("concurrent cas compare-and-update must not error");
                    (candidate, outcome)
                });
            }
            let mut swapped_b = Vec::new();
            let mut conflicts_b = Vec::new();
            while let Some(res) = join_b.join_next().await {
                let (candidate, outcome) = res.expect("CAS update task panicked");
                match outcome {
                    MetadataCasOutcome::Swapped { previous, current } => {
                        let prev = previous.expect("Swapped update must return previous record");
                        assert_eq!(prev.value, winner_a.value);
                        let curr = current.expect("Swapped update must return current record");
                        assert_eq!(curr.value, candidate);
                        assert_eq!(curr.created_at, winner_a.created_at);
                        swapped_b.push(curr);
                    }
                    MetadataCasOutcome::Conflict { actual } => {
                        conflicts_b.push(actual.expect("update loser must observe v2 winner"));
                    }
                }
            }
            assert_eq!(
                swapped_b.len(),
                1,
                "exactly 1 contender must win compare-and-update (is_redb={is_redb})"
            );
            assert_eq!(conflicts_b.len(), CAS_CONTENDERS - 1);
            let winner_b = swapped_b.into_iter().next().expect("winner_b");
            for loser_actual in &conflicts_b {
                assert_eq!(
                    loser_actual.value, winner_b.value,
                    "every update loser must observe v2 winner value"
                );
            }
            let stored_b = handles[0]
                .get_metadata("branch", "cas-race", "lease")
                .await
                .expect("read after update race")
                .expect("winning update record must exist");
            assert_eq!(stored_b, winner_b);
        }

        // Complete initial + updated schema receipts equivalence AND MetadataStore role
        // rejection across BOTH GlobalConfig and SystemConfig roles.
        for (label, role) in [
            ("global", DatabaseRole::GlobalConfig),
            ("system", DatabaseRole::SystemConfig),
        ] {
            let scoped_before_create = chrono::Utc::now();
            let scoped_sqlite =
                SqliteConfigStore::create(dir.path().join(format!("{label}.db")), role)
                    .await
                    .expect("create scoped sqlite");
            let scoped_redb =
                RedbConfigStore::create(dir.path().join(format!("{label}.redb")), role)
                    .expect("create scoped redb");
            let scoped_after_create = chrono::Utc::now();

            let sq_init = scoped_sqlite
                .list_schema_receipts()
                .await
                .expect("sqlite initial scoped receipts");
            let rb_init = scoped_redb
                .list_schema_receipts()
                .await
                .expect("redb initial scoped receipts");
            assert_eq!(
                validate_initial_receipts(&sq_init, scoped_before_create, scoped_after_create),
                validate_initial_receipts(&rb_init, scoped_before_create, scoped_after_create),
                "fresh {label} stores must have identical initial schema receipts"
            );

            let extra = SchemaVersionReceipt {
                version: 2026112202,
                name: format!("rb03_{label}_receipt"),
                applied_at: "2026-11-22T00:00:00Z".to_string(),
            };
            scoped_sqlite
                .record_schema_receipt(&extra)
                .await
                .expect("record sqlite scoped receipt");
            scoped_redb
                .record_schema_receipt(&extra)
                .await
                .expect("record redb scoped receipt");
            let sq_after = scoped_sqlite
                .list_schema_receipts()
                .await
                .expect("sqlite updated scoped receipts");
            let rb_after = scoped_redb
                .list_schema_receipts()
                .await
                .expect("redb updated scoped receipts");
            assert_eq!(sq_after.len(), sq_init.len() + 1);
            assert_eq!(sq_after.len(), rb_after.len());
            assert_eq!(
                sq_after.last(),
                Some(&extra),
                "SQLite {label} store must preserve appended SchemaVersionReceipt verbatim"
            );
            assert_eq!(
                rb_after.last(),
                Some(&extra),
                "Redb {label} store must preserve appended SchemaVersionReceipt verbatim"
            );
            assert_eq!(
                validate_initial_receipts(
                    &sq_after[..sq_init.len()],
                    scoped_before_create,
                    scoped_after_create,
                ),
                validate_initial_receipts(
                    &rb_after[..rb_init.len()],
                    scoped_before_create,
                    scoped_after_create,
                ),
                "initial {label} schema receipts must remain identical after append"
            );

            for store in [
                Arc::new(scoped_sqlite) as Arc<dyn MetadataStore>,
                Arc::new(scoped_redb) as Arc<dyn MetadataStore>,
            ] {
                let err = store
                    .get_metadata("branch", "main", "protect")
                    .await
                    .expect_err("non-Repository ConfigStore must reject MetadataStore operations");
                assert!(matches!(
                    err,
                    StorageBackendError::RoleMismatch {
                        expected: DatabaseRole::Repository,
                        found
                    } if found == role
                ));
            }
        }
    }

    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn redb_config_mvcc_concurrent_readers() {
        use std::{
            env, fs,
            path::PathBuf,
            process::{Child, Command, Stdio},
            time::{Duration, Instant},
        };

        use tempfile::tempdir;

        use crate::storage::{
            codec::encode_versioned,
            opener::{RedbRepairContext, begin_write_bounded_blocking, open_redb_writable},
            redb::{
                RedbConfigStore,
                config_store::{
                    CONFIG_KV_BY_KEY_INDEX, CONFIG_KV_TABLE, CONFIG_SCHEMA_VERSIONS_TABLE,
                    CONFIG_SEQ_CODEC_VERSION, CONFIG_SEQ_TABLE, METADATA_KV_TABLE,
                    SCHEMA_VERSIONS_TABLE,
                },
            },
        };

        const READER_COUNT: usize = 8;
        const MID_ROUND: usize = 15;
        const WRITER_ROUNDS: usize = 30;
        const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

        // Bounded cleanup guard ensuring no orphan child processes remain on panic.
        struct ChildBatchGuard {
            children: Vec<(String, Child)>,
        }
        impl Drop for ChildBatchGuard {
            fn drop(&mut self) {
                for (_, child) in &mut self.children {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }

        let validate_snapshot = |snapshot: &[ConfigEntry]| -> usize {
            assert_eq!(
                snapshot.len(),
                4,
                "MVCC snapshot must always contain exactly 4 entries (no torn rename), got {snapshot:?}"
            );
            let mut snapshot_gen: Option<usize> = None;
            let mut saw_seq = false;
            let mut saw_tag = false;
            let mut items = Vec::new();
            for entry in snapshot {
                let rest = entry
                    .key
                    .strip_prefix("mvcc.g")
                    .expect("key must begin with mvcc.g");
                let (gen_str, var_name) = rest
                    .split_once('.')
                    .expect("key must contain generation and variable");
                let g: usize = gen_str.parse().expect("generation must be integer");
                match snapshot_gen {
                    None => snapshot_gen = Some(g),
                    Some(prev_g) => assert_eq!(
                        g, prev_g,
                        "torn MVCC snapshot observed mixed generations {prev_g} and {g}: {snapshot:?}"
                    ),
                }
                match var_name {
                    "seq" => {
                        assert_eq!(entry.value, "atomic-v1");
                        saw_seq = true;
                    }
                    "tag" => {
                        assert_eq!(entry.value, "consistent");
                        saw_tag = true;
                    }
                    "item" => items.push(entry.value.clone()),
                    other => panic!("unexpected variable in snapshot: {other}"),
                }
            }
            let current_gen = snapshot_gen.expect("snapshot must have a generation");
            assert!(
                saw_seq && saw_tag,
                "snapshot missing paired keys: {snapshot:?}"
            );
            assert_eq!(
                items,
                vec!["alpha".to_string(), "beta".to_string()],
                "multi-value order must remain intact in every MVCC snapshot"
            );
            current_gen
        };

        // Isolated child-process mode: 1 writer + 8 concurrent `open_read_only` readers.
        if let Some(role_mode) = env::var_os("LIBRA_RB03_MVCC_ROLE") {
            let db_path = PathBuf::from(
                env::var_os("LIBRA_RB03_MVCC_DB").expect("LIBRA_RB03_MVCC_DB must be set"),
            );
            let sync_dir = PathBuf::from(
                env::var_os("LIBRA_RB03_MVCC_SYNC_DIR")
                    .expect("LIBRA_RB03_MVCC_SYNC_DIR must be set"),
            );
            let mode_str = role_mode.to_str().expect("role mode must be utf-8");

            let wait_for_all_readers = |suffix: &str| {
                let wait_start = Instant::now();
                loop {
                    let count = (0..READER_COUNT)
                        .filter(|idx| sync_dir.join(format!("reader-{idx}.{suffix}")).exists())
                        .count();
                    if count == READER_COUNT {
                        return;
                    }
                    assert!(
                        wait_start.elapsed() < Duration::from_secs(15),
                        "timed out waiting for all {READER_COUNT} MVCC readers at barrier `{suffix}` (got {count})"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            };

            if mode_str == "writer" {
                let store = RedbConfigStore::open_read_write(&db_path, DatabaseRole::Repository)
                    .expect("writer open_read_write");

                // Phase 1 barrier: wait until all 8 readers have ALREADY taken and
                // verified their initial gen-0 snapshot.
                wait_for_all_readers("init_snap");

                // Phase 2a: commit rounds 1..=MID_ROUND while all 8 readers loop concurrently.
                for round in 1..=MID_ROUND {
                    let old_sec = format!("mvcc.g{}", round - 1);
                    let new_sec = format!("mvcc.g{round}");
                    let moved = store
                        .rename_section(&old_sec, &new_sec)
                        .await
                        .expect("writer atomic rename_section (first half)");
                    assert_eq!(moved, 4, "each atomic round must move exactly 4 entries");
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }

                // Mid-write overlap barrier: hold writer alive at MID_ROUND until every
                // reader has observed an intermediate state (`MID_ROUND < WRITER_ROUNDS`).
                fs::write(sync_dir.join("writer.mid_reached"), b"mid")
                    .expect("signal writer.mid_reached");
                wait_for_all_readers("saw_mid");

                // Phase 2b: commit remaining rounds (MID_ROUND+1..=WRITER_ROUNDS) while all
                // 8 readers keep reading lock-free MVCC snapshots.
                for round in (MID_ROUND + 1)..=WRITER_ROUNDS {
                    let old_sec = format!("mvcc.g{}", round - 1);
                    let new_sec = format!("mvcc.g{round}");
                    let moved = store
                        .rename_section(&old_sec, &new_sec)
                        .await
                        .expect("writer atomic rename_section (second half)");
                    assert_eq!(moved, 4, "each atomic round must move exactly 4 entries");
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }

                fs::write(sync_dir.join("writer.done"), b"done").expect("signal writer.done");
                println!("RB03_MVCC_OK:writer");
                return;
            }

            if let Some(reader_id) = mode_str.strip_prefix("reader-") {
                let store = RedbConfigStore::open_read_only(&db_path, DatabaseRole::Repository)
                    .expect("reader open_read_only");
                assert!(
                    store.is_read_only(),
                    "reader handle must be strictly read-only"
                );

                // Phase 1: take and verify the initial gen-0 snapshot BEFORE releasing writer.
                let init_snapshot = store
                    .list_all()
                    .await
                    .expect("initial MVCC snapshot must succeed");
                let init_gen = validate_snapshot(&init_snapshot);
                assert_eq!(
                    init_gen, 0,
                    "initial snapshot before writer starts must be generation 0"
                );
                fs::write(
                    sync_dir.join(format!("reader-{reader_id}.init_snap")),
                    b"ready",
                )
                .expect("signal reader init_snap");

                let mut last_gen: usize = 0;
                let mut reads_completed: usize = 1;
                let mut distinct_gens = std::collections::BTreeSet::from([0usize]);
                let mut signaled_mid = false;
                let loop_start = Instant::now();

                loop {
                    let snapshot = store
                        .list_all()
                        .await
                        .expect("MVCC open_read_only snapshot must never hit lock errors");
                    let current_gen = validate_snapshot(&snapshot);
                    assert!(
                        current_gen >= last_gen,
                        "MVCC reader observed generation regression {current_gen} < {last_gen}"
                    );
                    last_gen = current_gen;
                    distinct_gens.insert(current_gen);
                    reads_completed += 1;

                    if !signaled_mid
                        && (MID_ROUND..WRITER_ROUNDS).contains(&current_gen)
                        && sync_dir.join("writer.mid_reached").exists()
                    {
                        fs::write(sync_dir.join(format!("reader-{reader_id}.saw_mid")), b"mid")
                            .expect("signal reader saw_mid");
                        signaled_mid = true;
                    }

                    if signaled_mid
                        && sync_dir.join("writer.done").exists()
                        && current_gen == WRITER_ROUNDS
                    {
                        break;
                    }
                    assert!(
                        loop_start.elapsed() < Duration::from_secs(25),
                        "reader-{reader_id} timed out waiting for writer overlap/completion (last_gen={last_gen}, signaled_mid={signaled_mid})"
                    );
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }

                assert!(
                    signaled_mid
                        && distinct_gens.contains(&0)
                        && distinct_gens.contains(&MID_ROUND)
                        && distinct_gens.contains(&WRITER_ROUNDS)
                        && distinct_gens.len() >= 3,
                    "reader-{reader_id} must prove live overlap across gen 0 -> mid ({MID_ROUND}) -> final ({WRITER_ROUNDS}), saw {distinct_gens:?}"
                );
                println!(
                    "RB03_MVCC_OK:reader-{reader_id}:reads={reads_completed}:gens={}",
                    distinct_gens.len()
                );
                return;
            }

            panic!("unexpected LIBRA_RB03_MVCC_ROLE: {mode_str}");
        }

        // Verify versioned codec enforcement on `CONFIG_KV_BY_KEY_INDEX` and `CONFIG_SEQ_TABLE`
        // (`ADR-RB-04` #2), non-overwriting concurrent `RedbConfigStore::create` safety, and
        // fail-closed missing-table checks.
        let codec_dir = tempdir().expect("create temp dir for codec and fault tests");

        // Concurrent `RedbConfigStore::create` race: exactly 1 creator succeeds,
        // all others fail with `AlreadyExists`, winner's data and receipts remain
        // untouched, and no staging temp files leak.
        {
            const CREATORS: usize = 4;
            let race_path = codec_dir.path().join("create-race.redb");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(CREATORS));
            let mut handles = Vec::with_capacity(CREATORS);
            for _ in 0..CREATORS {
                let p = race_path.clone();
                let b = std::sync::Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    b.wait();
                    RedbConfigStore::create(&p, DatabaseRole::Repository)
                }));
            }
            let mut winners = Vec::new();
            let mut already_exists = 0usize;
            for h in handles {
                match h.join().expect("create race thread panicked") {
                    Ok(store) => winners.push(store),
                    Err(StorageBackendError::Io {
                        kind: std::io::ErrorKind::AlreadyExists,
                        ..
                    }) => already_exists += 1,
                    Err(other) => {
                        panic!("expected Io(AlreadyExists) for create loser, got {other:?}")
                    }
                }
            }
            assert_eq!(winners.len(), 1, "exactly 1 concurrent creator must win");
            assert_eq!(already_exists, CREATORS - 1);
            let winner = winners.into_iter().next().expect("winner store");
            winner
                .set("race.owner", "first", false)
                .await
                .expect("winner set key");
            let winner_receipts = winner
                .list_schema_receipts()
                .await
                .expect("winner initial receipts");

            let late_err = RedbConfigStore::create(&race_path, DatabaseRole::Repository)
                .expect_err("second create on existing file must fail with AlreadyExists");
            assert!(
                matches!(
                    late_err,
                    StorageBackendError::Io {
                        kind: std::io::ErrorKind::AlreadyExists,
                        ..
                    }
                ),
                "expected Io(AlreadyExists) for late create, got {late_err:?}"
            );
            assert_eq!(
                winner
                    .get("race.owner")
                    .await
                    .expect("winner key must survive late create")
                    .map(|e| e.value),
                Some("first".to_string())
            );
            assert_eq!(
                winner
                    .list_schema_receipts()
                    .await
                    .expect("winner receipts must survive late create"),
                winner_receipts
            );
            let leaked_temps: Vec<_> = fs::read_dir(codec_dir.path())
                .expect("read codec_dir")
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
                .collect();
            assert!(
                leaked_temps.is_empty(),
                "create race must not leave staging temp files: {leaked_temps:?}"
            );

            // Unix permissions regression: Repository and GlobalConfig stores
            // must be created strictly `0600`; SystemConfig must be `0644`.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let repo_mode = fs::metadata(&race_path)
                    .expect("stat race_path")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(
                    repo_mode, 0o600,
                    "Repository RedbConfigStore must be created with mode 0600, got {repo_mode:04o}"
                );
                let global_perm_path = codec_dir.path().join("perm-global.redb");
                let _global_store =
                    RedbConfigStore::create(&global_perm_path, DatabaseRole::GlobalConfig)
                        .expect("create GlobalConfig redb store");
                let global_mode = fs::metadata(&global_perm_path)
                    .expect("stat global_perm_path")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(
                    global_mode, 0o600,
                    "GlobalConfig RedbConfigStore must be created with mode 0600, got {global_mode:04o}"
                );
                let system_perm_path = codec_dir.path().join("perm-system.redb");
                let _system_store =
                    RedbConfigStore::create(&system_perm_path, DatabaseRole::SystemConfig)
                        .expect("create SystemConfig redb store");
                let system_mode = fs::metadata(&system_perm_path)
                    .expect("stat system_perm_path")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(
                    system_mode, 0o644,
                    "SystemConfig RedbConfigStore must be created with mode 0644, got {system_mode:04o}"
                );

                // Exclusive creation, symlink refusal, parent-directory swap, and
                // post-commit target replacement regressions (Unix):
                // 1) Pre-existing symlink at target MUST fail closed with
                //    `AlreadyExists` (`openat(O_CREAT | O_EXCL | O_NOFOLLOW)`)
                //    without modifying `victim.secret` (`0600`).
                // 2) Renaming/replacing the parent directory during `create` MUST
                //    fail closed (`LBR-IO-001`) because the requested pathname no
                //    longer resolves to the pinned parent & created inode.
                // 3) Unlinking target, replacing target with a symlink to victim,
                //    or replacing target with an attacker DB before `create`
                //    returns MUST fail closed (`LBR-IO-001`), NEVER unlink or
                //    modify the replacement/victim, and NEVER return `Ok` with a
                //    detached store.
                let victim_path = codec_dir.path().join("victim.secret");
                fs::write(&victim_path, b"top-secret-contents").expect("write victim");
                fs::set_permissions(&victim_path, fs::Permissions::from_mode(0o600))
                    .expect("chmod 0600 victim");

                // Case 1: Pre-existing symlink at target fails with AlreadyExists; victim untouched.
                let pre_symlink_target = codec_dir.path().join("preexisting-symlink.redb");
                std::os::unix::fs::symlink(&victim_path, &pre_symlink_target)
                    .expect("create preexisting symlink");
                let pre_sym_err =
                    RedbConfigStore::create(&pre_symlink_target, DatabaseRole::SystemConfig)
                        .expect_err("create over preexisting symlink must fail closed");
                assert!(matches!(
                    pre_sym_err,
                    StorageBackendError::Io {
                        kind: std::io::ErrorKind::AlreadyExists,
                        ..
                    }
                ));
                assert_eq!(pre_sym_err.stable_code_str(), "LBR-IO-001");
                assert_eq!(
                    fs::metadata(&victim_path)
                        .expect("stat victim after preexisting symlink create")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
                assert_eq!(
                    fs::read(&victim_path).expect("read victim after preexisting symlink create"),
                    b"top-secret-contents"
                );

                // Case 2: Parent directory renamed and replaced during `create` must fail closed.
                let parent_swap_root = codec_dir.path().join("parent-swap-case");
                let orig_parent = parent_swap_root.join("target-parent");
                let moved_parent = parent_swap_root.join("moved-orig-parent");
                fs::create_dir_all(&orig_parent).expect("create orig_parent");
                let parent_swap_target = orig_parent.join("pinned-parent.redb");
                let parent_swap_err = RedbConfigStore::create_with_test_hooks(
                    &parent_swap_target,
                    DatabaseRole::SystemConfig,
                    || Ok(()),
                    |_| {
                        fs::rename(&orig_parent, &moved_parent)
                            .expect("rename parent dir away before create returns");
                        fs::create_dir(&orig_parent)
                            .expect("place replacement dir at old parent path");
                    },
                )
                .expect_err("create must fail closed when requested parent dir was replaced");
                assert!(matches!(
                    parent_swap_err,
                    StorageBackendError::BackendInternal { .. }
                ));
                assert_eq!(parent_swap_err.stable_code_str(), "LBR-IO-001");
                assert!(
                    !parent_swap_target.exists(),
                    "substituted replacement parent dir must NOT receive the database"
                );

                // Case 3a: Target unlinked after publish and before return must fail closed.
                let unlinked_target = codec_dir.path().join("post-commit-unlinked.redb");
                let unlink_err = RedbConfigStore::create_with_test_hooks(
                    &unlinked_target,
                    DatabaseRole::SystemConfig,
                    || Ok(()),
                    |created_path| {
                        fs::remove_file(created_path).expect("unlink created file");
                    },
                )
                .expect_err("unlinking target before create returns must fail closed");
                assert!(matches!(
                    unlink_err,
                    StorageBackendError::Io {
                        kind: std::io::ErrorKind::NotFound,
                        ..
                    }
                ));
                assert_eq!(unlink_err.stable_code_str(), "LBR-IO-001");

                // Case 3b: Target replaced with a symlink to `victim.secret` before return must
                // fail closed WITHOUT modifying or deleting the symlink or victim.
                let swap_symlink_target = codec_dir.path().join("post-commit-symlink-swap.redb");
                let symlink_err = RedbConfigStore::create_with_test_hooks(
                    &swap_symlink_target,
                    DatabaseRole::SystemConfig,
                    || Ok(()),
                    |created_path| {
                        fs::remove_file(created_path).expect("unlink created file");
                        std::os::unix::fs::symlink(&victim_path, created_path)
                            .expect("replace target path with symlink to victim");
                    },
                )
                .expect_err("replacing target with symlink before create returns must fail closed");
                assert!(matches!(
                    symlink_err,
                    StorageBackendError::Io { .. } | StorageBackendError::BackendInternal { .. }
                ));
                assert_eq!(symlink_err.stable_code_str(), "LBR-IO-001");
                assert_eq!(
                    fs::metadata(&victim_path)
                        .expect("stat victim after post-commit symlink swap")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
                assert_eq!(
                    fs::read(&victim_path).expect("read victim after post-commit symlink swap"),
                    b"top-secret-contents"
                );

                // Case 3c: Target replaced with another process's valid DB before return must
                // fail closed WITHOUT deleting or modifying that other process's database.
                let other_proc_seed = codec_dir.path().join("other-proc-seed.redb");
                {
                    let seed =
                        RedbConfigStore::create(&other_proc_seed, DatabaseRole::SystemConfig)
                            .expect("create other-process seed DB");
                    seed.set("other.proc", "preserved", false)
                        .await
                        .expect("seed other.proc");
                }
                let swap_db_target = codec_dir.path().join("post-commit-other-db.redb");
                let db_swap_err = RedbConfigStore::create_with_test_hooks(
                    &swap_db_target,
                    DatabaseRole::SystemConfig,
                    || Ok(()),
                    |created_path| {
                        fs::remove_file(created_path).expect("unlink original file");
                        fs::copy(&other_proc_seed, created_path)
                            .expect("place another process's valid redb DB at target path");
                    },
                )
                .expect_err(
                    "replacing target with another DB before create returns must fail closed",
                );
                assert!(matches!(
                    db_swap_err,
                    StorageBackendError::BackendInternal { .. }
                ));
                assert_eq!(db_swap_err.stable_code_str(), "LBR-IO-001");
                // Crucially, error path MUST NOT delete the other process's replacement database.
                let surviving_other_db =
                    RedbConfigStore::open_read_only(&swap_db_target, DatabaseRole::SystemConfig)
                        .expect("error path must NOT unlink another process's replacement DB");
                assert_eq!(
                    surviving_other_db
                        .get("other.proc")
                        .await
                        .expect("read preserved other-process DB")
                        .map(|e| e.value),
                    Some("preserved".to_string())
                );
            }

            // Failure-injection during creation before publication MUST leave
            // zero file at `target` and zero temp residue, so immediate retry
            // succeeds cleanly across all platforms.
            {
                let retry_dir = tempdir().expect("create temp dir for create failure/retry test");
                let retry_target = retry_dir.path().join("retry-after-fail.redb");
                let injected_err = RedbConfigStore::create_with_test_hooks(
                    &retry_target,
                    DatabaseRole::Repository,
                    || {
                        Err(StorageBackendError::BackendInternal {
                            detail: "simulated ENOSPC before publication".to_string(),
                        })
                    },
                    |_| {},
                )
                .expect_err("injected failure before publication must error");
                assert!(matches!(
                    injected_err,
                    StorageBackendError::BackendInternal { .. }
                ));
                assert!(
                    fs::symlink_metadata(&retry_target).is_err(),
                    "failed creation must NEVER leave an incomplete file at target"
                );
                let leftovers: Vec<_> = fs::read_dir(retry_dir.path())
                    .expect("read retry_dir")
                    .filter_map(|e| e.ok())
                    .collect();
                assert!(
                    leftovers.is_empty(),
                    "failed creation must leave zero residue in parent dir: {leftovers:?}"
                );

                // Immediate retry must succeed cleanly and be fully usable.
                let retried = RedbConfigStore::create(&retry_target, DatabaseRole::Repository)
                    .expect("retry create after failed attempt must succeed");
                retried
                    .set("retry.verified", "yes", false)
                    .await
                    .expect("write to retried store");
                assert_eq!(
                    retried
                        .get("retry.verified")
                        .await
                        .expect("read back from retried store")
                        .map(|e| e.value),
                    Some("yes".to_string())
                );
            }

            // Async write cancellation safety: cancelling/timing out a write
            // while waiting on the write lock OR while executing before commit
            // MUST abort cleanly with zero committed mutations.
            {
                let cancel_path = codec_dir.path().join("cancel-safety.redb");
                let cancel_store = RedbConfigStore::create(&cancel_path, DatabaseRole::Repository)
                    .expect("create cancel-safety store");

                // 1. Cancel while queued/waiting on an external write lock.
                {
                    let raw_db = open_redb_writable(
                        &cancel_path,
                        DatabaseRole::Repository,
                        RedbRepairContext::WriteCommand,
                    )
                    .expect("open raw db to hold write lock");
                    let held_wtx = begin_write_bounded_blocking(
                        &raw_db,
                        DatabaseRole::Repository,
                        Duration::from_secs(5),
                    )
                    .expect("acquire external write lock");

                    let timed_out = tokio::time::timeout(
                        Duration::from_millis(60),
                        cancel_store.add("cancel.queued", "must-not-commit", false),
                    )
                    .await;
                    assert!(
                        timed_out.is_err(),
                        "write waiting on held lock must time out and cancel"
                    );
                    drop(held_wtx);
                }
                // Give the cancelled blocking worker ample time to observe cancellation and exit.
                tokio::time::sleep(Duration::from_millis(80)).await;
                assert_eq!(
                    cancel_store
                        .get("cancel.queued")
                        .await
                        .expect("read after queued write cancellation"),
                    None,
                    "cancelled queued write must never commit after lock release"
                );

                // 2. Cancel while executing `op(&wtx)` inside `spawn_blocking` before commit.
                let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
                let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
                let store_for_task = cancel_store.clone();
                let write_task = tokio::spawn(async move {
                    store_for_task
                        .add_with_pre_commit_hook(
                            "cancel.inflight",
                            "must-not-commit",
                            false,
                            move || {
                                let _ = entered_tx.send(());
                                let _ = release_rx.recv();
                            },
                        )
                        .await
                });

                // Wait until the blocking worker has already mutated the uncommitted `WriteTransaction`.
                tokio::task::spawn_blocking(move || {
                    entered_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("write task must reach pre-commit hook")
                })
                .await
                .expect("wait for pre-commit hook");

                // Abort the async task (dropping its future and running `CancelOnDrop`).
                write_task.abort();
                let _ = write_task.await;

                // Release the paused worker so it checks cancellation and aborts `wtx`.
                release_tx.send(()).expect("release paused write worker");
                tokio::time::sleep(Duration::from_millis(50)).await;

                assert_eq!(
                    cancel_store
                        .get("cancel.inflight")
                        .await
                        .expect("read after in-flight pre-commit cancellation"),
                    None,
                    "cancelled in-flight write must abort and never commit"
                );
                // Store remains healthy for subsequent writes.
                cancel_store
                    .add("cancel.after", "healthy", false)
                    .await
                    .expect("subsequent write after cancellation must succeed");
                assert_eq!(
                    cancel_store
                        .get("cancel.after")
                        .await
                        .expect("read cancel.after")
                        .map(|e| e.value),
                    Some("healthy".to_string())
                );
            }

            // Windows permissive-parent ACL regression: verify that creating
            // `Repository` and `GlobalConfig` stores inside a directory with an
            // explicit Everyone (`WD`) Full Access inherited ACL still produces
            // a protected (`SE_DACL_PROTECTED`) owner-only file passing
            // `windows_fs::validate_private`, and that `SystemConfig` also
            // blocks inherited write access.
            #[cfg(windows)]
            {
                use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};

                use windows_sys::Win32::{
                    Foundation::{LocalFree, PSID},
                    Security::{
                        Authorization::{
                            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
                            SE_FILE_OBJECT, SetNamedSecurityInfoW,
                        },
                        DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
                        GetSecurityDescriptorDacl, SE_DACL_PROTECTED,
                    },
                };

                let perm_parent = codec_dir.path().join("win-permissive-parent");
                fs::create_dir_all(&perm_parent).expect("create win permissive parent");
                let sddl: Vec<u16> = std::ffi::OsStr::new("D:(A;OICI;FA;;;WD)")
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                let mut raw_sd: *mut std::ffi::c_void = std::ptr::null_mut();
                let ok = unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        sddl.as_ptr(),
                        1,
                        &mut raw_sd,
                        std::ptr::null_mut(),
                    )
                };
                assert_ne!(ok, 0, "convert permissive parent SDDL");
                let mut dacl_present = 0;
                let mut dacl = std::ptr::null_mut();
                let mut dacl_defaulted = 0;
                let got_dacl = unsafe {
                    GetSecurityDescriptorDacl(
                        raw_sd,
                        &mut dacl_present,
                        &mut dacl,
                        &mut dacl_defaulted,
                    )
                };
                assert_ne!(got_dacl, 0, "extract permissive DACL");
                let mut wide_parent: Vec<u16> = perm_parent
                    .as_os_str()
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                let set_rc = unsafe {
                    SetNamedSecurityInfoW(
                        wide_parent.as_mut_ptr(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut() as PSID,
                        std::ptr::null_mut() as PSID,
                        dacl,
                        std::ptr::null_mut(),
                    )
                };
                unsafe { LocalFree(raw_sd) };
                assert_eq!(set_rc, 0, "apply permissive Everyone ACL to parent dir");

                for (name, role) in [
                    ("repo-private.redb", DatabaseRole::Repository),
                    ("global-private.redb", DatabaseRole::GlobalConfig),
                ] {
                    let target = perm_parent.join(name);
                    let _store =
                        RedbConfigStore::create(&target, role).expect("create private redb store");
                    let f = fs::File::open(&target).expect("open created redb store");
                    crate::utils::windows_fs::validate_private(&f).expect(
                        "Repository/GlobalConfig store under permissive parent must enforce protected owner-only DACL",
                    );
                }

                let sys_target = perm_parent.join("system-protected.redb");
                let _sys_store = RedbConfigStore::create(&sys_target, DatabaseRole::SystemConfig)
                    .expect("create SystemConfig redb store");
                let sys_file = fs::File::open(&sys_target).expect("open system redb store");
                let mut sd_out = std::ptr::null_mut();
                let rc = unsafe {
                    GetSecurityInfo(
                        sys_file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut sd_out,
                    )
                };
                assert_eq!(rc, 0, "query SystemConfig DACL");
                let mut control = 0u16;
                let mut rev = 0u32;
                let ctrl_ok =
                    unsafe { GetSecurityDescriptorControl(sd_out, &mut control, &mut rev) };
                unsafe { LocalFree(sd_out) };
                assert_ne!(ctrl_ok, 0, "read SystemConfig SD control flags");
                assert_ne!(
                    u32::from(control) & SE_DACL_PROTECTED,
                    0,
                    "SystemConfig store must set SE_DACL_PROTECTED to block inherited permissive parent write ACLs"
                );
            }
        }

        // Missing required table fault tests: deleting any required table from a
        // valid `META_SCHEMA` DB must fail closed on fresh open AND on already-open
        // read/write handles (never silently returning empty or recreating tables).
        for (case_label, role) in [
            ("missing_config_kv", DatabaseRole::Repository),
            ("missing_metadata_kv", DatabaseRole::Repository),
            ("missing_schema_versions", DatabaseRole::Repository),
            ("missing_config_schema_versions", DatabaseRole::GlobalConfig),
        ] {
            let fault_path = codec_dir.path().join(format!("{case_label}.redb"));
            {
                let seed = RedbConfigStore::create(&fault_path, role)
                    .expect("create missing-table fault fixture");
                seed.set("fault.key", "present", false)
                    .await
                    .expect("seed fault.key");
                if role == DatabaseRole::Repository {
                    seed.set_metadata("branch", "main", "fault", "present", "text")
                        .await
                        .expect("seed fault metadata");
                }
            }
            let open_ro = RedbConfigStore::open_read_only(&fault_path, role)
                .expect("open pre-fault read-only handle");
            let open_rw = RedbConfigStore::open_read_write(&fault_path, role)
                .expect("open pre-fault read-write handle");

            {
                let raw_db = open_redb_writable(&fault_path, role, RedbRepairContext::Fsck)
                    .expect("open raw db for missing-table fault");
                let wtx = begin_write_bounded_blocking(&raw_db, role, Duration::from_secs(5))
                    .expect("begin raw wtx for missing-table fault");
                let deleted = match case_label {
                    "missing_config_kv" => wtx.delete_table(CONFIG_KV_TABLE),
                    "missing_metadata_kv" => wtx.delete_table(METADATA_KV_TABLE),
                    "missing_schema_versions" => wtx.delete_table(SCHEMA_VERSIONS_TABLE),
                    "missing_config_schema_versions" => {
                        wtx.delete_table(CONFIG_SCHEMA_VERSIONS_TABLE)
                    }
                    _ => unreachable!(),
                }
                .expect("delete required table");
                assert!(deleted, "required table must have existed before deletion");
                wtx.commit().expect("commit required table deletion");
            }

            let fresh_ro_err = RedbConfigStore::open_read_only(&fault_path, role)
                .expect_err("fresh open_read_only must fail closed when required table is missing");
            assert!(
                matches!(fresh_ro_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on fresh open_read_only, got {fresh_ro_err:?}"
            );
            assert_eq!(fresh_ro_err.stable_code_str(), "LBR-IO-001");

            let fresh_rw_err = RedbConfigStore::open_read_write(&fault_path, role).expect_err(
                "fresh open_read_write must fail closed when required table is missing",
            );
            assert!(
                matches!(fresh_rw_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on fresh open_read_write, got {fresh_rw_err:?}"
            );
            assert_eq!(fresh_rw_err.stable_code_str(), "LBR-IO-001");

            // Already-open read handle must fail closed on BOTH direct-table AND
            // cross-table reads rather than treating a missing table as empty or
            // returning partial results from surviving tables.
            let open_read_err = match case_label {
                "missing_config_kv" => open_ro
                    .get("absent.key")
                    .await
                    .expect_err("already-open RO handle must fail closed on missing config_kv"),
                "missing_metadata_kv" => open_ro
                    .get_metadata("branch", "main", "fault")
                    .await
                    .expect_err("already-open RO handle must fail closed on missing metadata_kv"),
                "missing_schema_versions" | "missing_config_schema_versions" => open_ro
                    .list_schema_receipts()
                    .await
                    .expect_err("already-open RO handle must fail closed on missing schema ledger"),
                _ => unreachable!(),
            };
            assert!(
                matches!(open_read_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on already-open direct read, got {open_read_err:?}"
            );
            assert_eq!(open_read_err.stable_code_str(), "LBR-IO-001");

            let cross_table_err = match case_label {
                "missing_config_kv" => open_ro
                    .get_metadata("branch", "main", "fault")
                    .await
                    .expect_err("already-open RO metadata read must fail closed when config_kv is missing"),
                "missing_metadata_kv"
                | "missing_schema_versions"
                | "missing_config_schema_versions" => open_ro
                    .list_all()
                    .await
                    .expect_err("already-open RO config_kv list_all must fail closed when another required table is missing"),
                _ => unreachable!(),
            };
            assert!(
                matches!(cross_table_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on already-open cross-table read, got {cross_table_err:?}"
            );
            assert_eq!(cross_table_err.stable_code_str(), "LBR-IO-001");

            // Already-open write handle must fail closed rather than silently recreating deleted table.
            let open_write_err = open_rw
                .set("after.damage", "must-fail", false)
                .await
                .expect_err("already-open RW handle must refuse write on damaged DB");
            assert!(
                matches!(open_write_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on already-open write, got {open_write_err:?}"
            );
            assert_eq!(open_write_err.stable_code_str(), "LBR-IO-001");
        }

        // Sequence counter fault tests for BOTH `config_kv` and legacy `config`:
        // a deleted or regressed sequence counter on a non-empty table must fail
        // closed rather than reusing ID 1 and silently overwriting existing data.
        let regressed_zero_bytes =
            encode_versioned(CONFIG_SEQ_CODEC_VERSION, &0u64).expect("encode zero sequence");
        for (seq_fault, delete_entry) in [("missing_seq", true), ("regressed_seq", false)] {
            // 1. `config_kv` sequence fault.
            let kv_seq_path = codec_dir.path().join(format!("kv-{seq_fault}.redb"));
            {
                let seed = RedbConfigStore::create(&kv_seq_path, DatabaseRole::Repository)
                    .expect("create kv sequence fault fixture");
                seed.add("seq.original", "must-not-overwrite", false)
                    .await
                    .expect("seed seq.original");
            }
            {
                let raw_db = open_redb_writable(
                    &kv_seq_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Fsck,
                )
                .expect("open raw db for kv seq fault");
                let wtx = begin_write_bounded_blocking(
                    &raw_db,
                    DatabaseRole::Repository,
                    Duration::from_secs(5),
                )
                .expect("begin wtx for kv seq fault");
                {
                    let mut seq_table = wtx.open_table(CONFIG_SEQ_TABLE).expect("open seq table");
                    if delete_entry {
                        assert!(
                            seq_table
                                .remove("config_kv")
                                .expect("remove config_kv seq")
                                .is_some()
                        );
                    } else {
                        seq_table
                            .insert("config_kv", regressed_zero_bytes.as_slice())
                            .expect("regress config_kv seq to 0");
                    }
                }
                wtx.commit().expect("commit kv seq fault");
            }
            let kv_store = RedbConfigStore::open_read_write(&kv_seq_path, DatabaseRole::Repository)
                .expect("open kv seq fault fixture");
            let kv_err = kv_store
                .add("seq.new", "would-overwrite-id-1", false)
                .await
                .expect_err("config_kv insert must fail closed on missing/regressed sequence");
            assert!(
                matches!(kv_err, StorageBackendError::BackendInternal { .. }),
                "{seq_fault} config_kv: expected BackendInternal, got {kv_err:?}"
            );
            assert_eq!(kv_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                kv_store
                    .list_all()
                    .await
                    .expect("existing config_kv rows must stay intact"),
                vec![ConfigEntry {
                    key: "seq.original".to_string(),
                    value: "must-not-overwrite".to_string(),
                    encrypted: false,
                }]
            );

            // 2. Legacy `config` sequence fault.
            let leg_seq_path = codec_dir.path().join(format!("legacy-{seq_fault}.redb"));
            let original_legacy = LegacyConfigEntry {
                configuration: "core".to_string(),
                name: None,
                key: "bare".to_string(),
                value: "false".to_string(),
            };
            {
                let seed = RedbConfigStore::create(&leg_seq_path, DatabaseRole::Repository)
                    .expect("create legacy sequence fault fixture");
                seed.insert_legacy_entry(&original_legacy)
                    .await
                    .expect("seed original legacy row");
            }
            {
                let raw_db = open_redb_writable(
                    &leg_seq_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Fsck,
                )
                .expect("open raw db for legacy seq fault");
                let wtx = begin_write_bounded_blocking(
                    &raw_db,
                    DatabaseRole::Repository,
                    Duration::from_secs(5),
                )
                .expect("begin wtx for legacy seq fault");
                {
                    let mut seq_table = wtx.open_table(CONFIG_SEQ_TABLE).expect("open seq table");
                    if delete_entry {
                        assert!(
                            seq_table
                                .remove("config")
                                .expect("remove legacy config seq")
                                .is_some()
                        );
                    } else {
                        seq_table
                            .insert("config", regressed_zero_bytes.as_slice())
                            .expect("regress legacy config seq to 0");
                    }
                }
                wtx.commit().expect("commit legacy seq fault");
            }
            let leg_store =
                RedbConfigStore::open_read_write(&leg_seq_path, DatabaseRole::Repository)
                    .expect("open legacy seq fault fixture");
            let leg_err = leg_store
                .insert_legacy_entry(&LegacyConfigEntry {
                    configuration: "core".to_string(),
                    name: None,
                    key: "filemode".to_string(),
                    value: "true".to_string(),
                })
                .await
                .expect_err("legacy config insert must fail closed on missing/regressed sequence");
            assert!(
                matches!(leg_err, StorageBackendError::BackendInternal { .. }),
                "{seq_fault} legacy config: expected BackendInternal, got {leg_err:?}"
            );
            assert_eq!(leg_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                leg_store
                    .list_legacy_entries()
                    .await
                    .expect("existing legacy row must stay intact"),
                vec![original_legacy]
            );
        }

        for (case_name, bad_bytes, expect_future) in [
            ("corrupt_trailing_bytes", vec![1u8, 0u8, 0xFF, 0xEE], false),
            ("future_codec_version", vec![0xFFu8, 0xFFu8], true),
        ] {
            // 1. Corrupt/future secondary index value (`CONFIG_KV_BY_KEY_INDEX`).
            let idx_path = codec_dir.path().join(format!("idx-{case_name}.redb"));
            {
                let store = RedbConfigStore::create(&idx_path, DatabaseRole::Repository)
                    .expect("create idx fault fixture");
                store
                    .set("codec.check", "ok", false)
                    .await
                    .expect("seed codec.check");
            }
            {
                let raw_db = open_redb_writable(
                    &idx_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Fsck,
                )
                .expect("open raw db for index fault injection");
                let wtx = begin_write_bounded_blocking(
                    &raw_db,
                    DatabaseRole::Repository,
                    Duration::from_secs(5),
                )
                .expect("begin raw wtx for index fault injection");
                {
                    let mut idx_table = wtx
                        .open_table(CONFIG_KV_BY_KEY_INDEX)
                        .expect("open index table");
                    idx_table
                        .insert(("codec.check", 1u64), bad_bytes.as_slice())
                        .expect("inject bad index codec bytes");
                }
                wtx.commit().expect("commit bad index codec bytes");
            }
            let reopened = RedbConfigStore::open_read_only(&idx_path, DatabaseRole::Repository)
                .expect("reopen idx fixture");
            let idx_err = reopened
                .get("codec.check")
                .await
                .expect_err("corrupt/future index marker must fail closed");
            assert_eq!(idx_err.stable_code_str(), "LBR-IO-001");
            if expect_future {
                assert!(
                    matches!(
                        idx_err,
                        StorageBackendError::UnsupportedSchemaVersion { .. }
                    ),
                    "expected UnsupportedSchemaVersion for index {case_name}, got {idx_err:?}"
                );
            } else {
                assert!(
                    matches!(idx_err, StorageBackendError::CorruptPayload { .. }),
                    "expected CorruptPayload for index {case_name}, got {idx_err:?}"
                );
            }

            // 2. Corrupt/future sequence value (`CONFIG_SEQ_TABLE`).
            let seq_path = codec_dir.path().join(format!("seq-{case_name}.redb"));
            {
                let store = RedbConfigStore::create(&seq_path, DatabaseRole::Repository)
                    .expect("create seq fault fixture");
                store
                    .set("codec.seq", "ok", false)
                    .await
                    .expect("seed codec.seq");
            }
            {
                let raw_db = open_redb_writable(
                    &seq_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Fsck,
                )
                .expect("open raw db for seq fault injection");
                let wtx = begin_write_bounded_blocking(
                    &raw_db,
                    DatabaseRole::Repository,
                    Duration::from_secs(5),
                )
                .expect("begin raw wtx for seq fault injection");
                {
                    let mut seq_table = wtx.open_table(CONFIG_SEQ_TABLE).expect("open seq table");
                    seq_table
                        .insert("config_kv", bad_bytes.as_slice())
                        .expect("inject bad seq codec bytes");
                }
                wtx.commit().expect("commit bad seq codec bytes");
            }
            let reopened_rw = RedbConfigStore::open_read_write(&seq_path, DatabaseRole::Repository)
                .expect("reopen seq fixture");
            let seq_err = reopened_rw
                .add("codec.seq2", "next", false)
                .await
                .expect_err("corrupt/future sequence record must fail closed");
            assert_eq!(seq_err.stable_code_str(), "LBR-IO-001");
            if expect_future {
                assert!(
                    matches!(
                        seq_err,
                        StorageBackendError::UnsupportedSchemaVersion { .. }
                    ),
                    "expected UnsupportedSchemaVersion for seq {case_name}, got {seq_err:?}"
                );
            } else {
                assert!(
                    matches!(
                        seq_err,
                        StorageBackendError::CorruptPayload { .. }
                            | StorageBackendError::TruncatedPayload { .. }
                    ),
                    "expected CorruptPayload/TruncatedPayload for seq {case_name}, got {seq_err:?}"
                );
            }
        }

        // Parent orchestrator: create fixture with generation 0 (4 entries: 2 singletons + 2 multi-value).
        let dir = tempdir().expect("create temp dir for MVCC test");
        let db_path = dir.path().join("git.gen-00000001.redb");
        let sync_dir = dir.path().join("sync");
        fs::create_dir_all(&sync_dir).expect("create sync dir");

        {
            let store = RedbConfigStore::create(&db_path, DatabaseRole::Repository)
                .expect("create initial RedbConfigStore");
            store
                .set("mvcc.g0.seq", "atomic-v1", false)
                .await
                .expect("seed seq");
            store
                .set("mvcc.g0.tag", "consistent", false)
                .await
                .expect("seed tag");
            store
                .add("mvcc.g0.item", "alpha", false)
                .await
                .expect("seed item 1");
            store
                .add("mvcc.g0.item", "beta", false)
                .await
                .expect("seed item 2");
        }

        let exe = env::current_exe().expect("locate current test executable");
        let mut guard = ChildBatchGuard {
            children: Vec::with_capacity(READER_COUNT + 1),
        };

        let spawn_worker = |role_name: String| -> (String, Child) {
            let child = Command::new(&exe)
                .arg("--exact")
                .arg("storage::config_store::tests::redb_config_mvcc_concurrent_readers")
                .arg("--nocapture")
                .env("LIBRA_RB03_MVCC_ROLE", &role_name)
                .env("LIBRA_RB03_MVCC_DB", &db_path)
                .env("LIBRA_RB03_MVCC_SYNC_DIR", &sync_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn MVCC worker child");
            (role_name, child)
        };

        for idx in 0..READER_COUNT {
            guard.children.push(spawn_worker(format!("reader-{idx}")));
        }
        guard.children.push(spawn_worker("writer".to_string()));

        // Bounded wait loop keeping `guard` armed until each child exits cleanly.
        let deadline = Instant::now() + CHILD_TIMEOUT;
        let mut completed = Vec::with_capacity(READER_COUNT + 1);
        while !guard.children.is_empty() {
            let mut idx = 0;
            while idx < guard.children.len() {
                if let Some(_status) = guard.children[idx]
                    .1
                    .try_wait()
                    .expect("poll MVCC child status")
                {
                    let (name, child) = guard.children.swap_remove(idx);
                    let output = child.wait_with_output().expect("collect MVCC child output");
                    completed.push((name, output));
                } else {
                    idx += 1;
                }
            }
            if !guard.children.is_empty() {
                assert!(
                    Instant::now() < deadline,
                    "MVCC child processes exceeded {CHILD_TIMEOUT:?} timeout"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        assert_eq!(completed.len(), READER_COUNT + 1);
        for (name, output) in completed {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let sentinel = format!("RB03_MVCC_OK:{name}");
            assert!(
                output.status.success()
                    && stdout.contains(&sentinel)
                    && stdout.contains("1 passed; 0 failed"),
                "MVCC child `{name}` did not succeed cleanly (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
        }

        // Verify final database state via read-only opener matches WRITER_ROUNDS.
        let verify_store = RedbConfigStore::open_read_only(&db_path, DatabaseRole::Repository)
            .expect("reopen read-only after MVCC stress");
        let final_entries = verify_store.list_all().await.expect("final list_all");
        assert_eq!(final_entries.len(), 4);
        let expected_prefix = format!("mvcc.g{WRITER_ROUNDS}.");
        assert!(
            final_entries
                .iter()
                .all(|e| e.key.starts_with(&expected_prefix)),
            "final entries must all be at generation {WRITER_ROUNDS}: {final_entries:?}"
        );
    }

    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn redb_config_gc13_repo_isolation_and_scope_file_modes() {
        use std::{
            env, fs,
            path::PathBuf,
            process::{Command, Stdio},
        };

        use tempfile::tempdir;

        use crate::storage::redb::RedbConfigStore;

        // Isolated child mode so `LIBRA_CONFIG_GLOBAL_DB`, `LIBRA_CONFIG_SYSTEM_DB`,
        // and Unix `umask` mutations never affect parallel tests in the parent process.
        if let Some(child_mode) = env::var_os("LIBRA_RB03J_CHILD_MODE") {
            let mode_str = child_mode.to_string_lossy();
            match mode_str.as_ref() {
                "repo_isolation" => {
                    let repo_db = PathBuf::from(
                        env::var_os("LIBRA_RB03J_REPO_DB").expect("repo db path env"),
                    );
                    let store = RedbConfigStore::create(&repo_db, DatabaseRole::Repository)
                        .expect("create Repository RedbConfigStore");
                    store
                        .set("user.name", "Repo Isolated User", false)
                        .await
                        .expect("set repo user.name");
                    store
                        .set("user.global", "repo-must-not-touch-global", false)
                        .await
                        .expect("set repo user.global");
                    store
                        .set("core.system", "repo-must-not-touch-system", false)
                        .await
                        .expect("set repo core.system");
                    store
                        .add(
                            "remote.origin.fetch",
                            "+refs/heads/*:refs/remotes/origin/*",
                            false,
                        )
                        .await
                        .expect("add repo fetch");
                    store
                        .rename_section("remote.origin", "remote.upstream")
                        .await
                        .expect("rename repo section");
                    store
                        .remove_section("remote.upstream")
                        .await
                        .expect("remove repo section");
                    store
                        .insert_legacy_entry(&LegacyConfigEntry {
                            configuration: "core".to_string(),
                            name: None,
                            key: "bare".to_string(),
                            value: "false".to_string(),
                        })
                        .await
                        .expect("insert repo legacy entry");
                    store
                        .record_schema_receipt(&SchemaVersionReceipt {
                            version: 99,
                            name: "rb03j_isolation_receipt".to_string(),
                            applied_at: "2026-11-22T00:00:00Z".to_string(),
                        })
                        .await
                        .expect("record repo receipt");
                    store
                        .set_metadata("branch", "main", "note", "repo-only", "text")
                        .await
                        .expect("set repo metadata");
                    let cas = store
                        .cas_metadata(
                            "branch",
                            "main",
                            "note",
                            Some("repo-only"),
                            Some(("repo-only-v2", "text")),
                        )
                        .await
                        .expect("cas repo metadata");
                    assert!(matches!(cas, MetadataCasOutcome::Swapped { .. }));
                    println!("RB03J_CHILD_OK:repo_isolation");
                    return;
                }
                "global_mode_permissive_umask" => {
                    #[cfg(unix)]
                    // SAFETY: Single-threaded isolated child process; setting permissive umask
                    // proves `GlobalConfig` enforces `0600` explicitly rather than via umask.
                    unsafe {
                        libc::umask(0o000);
                    }
                    let global_db_env = PathBuf::from(
                        env::var_os(LIBRA_CONFIG_GLOBAL_DB_ENV).expect("global db env"),
                    );
                    let scope_dir = config_scope_dir_from_db_path(&global_db_env)
                        .expect("resolve global scope dir");
                    let store = RedbConfigStore::create_in_scope_dir(
                        &scope_dir,
                        DatabaseRole::GlobalConfig,
                    )
                    .expect("create GlobalConfig RedbConfigStore in empty scope dir");
                    assert_eq!(store.role(), DatabaseRole::GlobalConfig);
                    store
                        .set("user.email", "global@example.com", false)
                        .await
                        .expect("write global config key");
                    store
                        .add("alias.st", "status", false)
                        .await
                        .expect("add global alias");
                    // GlobalConfig must reject Repository-only `metadata_kv` mutations (`GC-13`).
                    let meta_err = store
                        .set_metadata("branch", "main", "k", "v", "text")
                        .await
                        .expect_err("GlobalConfig must reject metadata_kv writes");
                    assert!(matches!(
                        meta_err,
                        StorageBackendError::RoleMismatch {
                            expected: DatabaseRole::Repository,
                            found: DatabaseRole::GlobalConfig,
                        }
                    ));
                    println!("RB03J_CHILD_OK:global_mode_permissive_umask");
                    return;
                }
                "system_mode_restrictive_umask" => {
                    #[cfg(unix)]
                    // SAFETY: Single-threaded isolated child process; setting restrictive umask
                    // `0o077` proves `SystemConfig` enforces exact `0644` explicitly despite umask.
                    unsafe {
                        libc::umask(0o077);
                    }
                    let system_db_env = PathBuf::from(
                        env::var_os(LIBRA_CONFIG_SYSTEM_DB_ENV).expect("system db env"),
                    );
                    let scope_dir = config_scope_dir_from_db_path(&system_db_env)
                        .expect("resolve system scope dir");
                    let store = RedbConfigStore::create_in_scope_dir(
                        &scope_dir,
                        DatabaseRole::SystemConfig,
                    )
                    .expect("create SystemConfig RedbConfigStore in empty scope dir");
                    assert_eq!(store.role(), DatabaseRole::SystemConfig);
                    store
                        .set("core.editor", "vim", false)
                        .await
                        .expect("write system config key");
                    store
                        .add("http.extraHeader", "X-Test: 1", false)
                        .await
                        .expect("add system multi-value key");
                    // SystemConfig must reject Repository-only `metadata_kv` mutations (`GC-13`).
                    let meta_err = store
                        .set_metadata("branch", "main", "k", "v", "text")
                        .await
                        .expect_err("SystemConfig must reject metadata_kv writes");
                    assert!(matches!(
                        meta_err,
                        StorageBackendError::RoleMismatch {
                            expected: DatabaseRole::Repository,
                            found: DatabaseRole::SystemConfig,
                        }
                    ));
                    println!("RB03J_CHILD_OK:system_mode_restrictive_umask");
                    return;
                }
                other => panic!("unexpected LIBRA_RB03J_CHILD_MODE: {other}"),
            }
        }

        let exe = env::current_exe().expect("locate current test executable");
        let run_child = |mode: &str, envs: &[(&str, PathBuf)]| {
            let mut cmd = Command::new(&exe);
            cmd.arg("--exact")
                .arg("storage::config_store::tests::redb_config_gc13_repo_isolation_and_scope_file_modes")
                .arg("--nocapture")
                .env("LIBRA_RB03J_CHILD_MODE", mode)
                .env("LIBRA_TEST", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (k, v) in envs {
                cmd.env(k, v);
            }
            let output = cmd.output().expect("spawn RB-03j child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let sentinel = format!("RB03J_CHILD_OK:{mode}");
            assert!(
                output.status.success()
                    && stdout.contains(&sentinel)
                    && stdout.contains("1 passed; 0 failed"),
                "RB-03j child `{mode}` failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
        };

        let root = tempdir().expect("create RB-03j temp root");

        // -----------------------------------------------------------------
        // AC1 & AC2 (Part A): Absent `LIBRA_CONFIG_GLOBAL_DB` and
        // `LIBRA_CONFIG_SYSTEM_DB` fixtures must NEVER be created when a
        // `Repository` `RedbConfigStore` is created and written to.
        // -----------------------------------------------------------------
        let repo_a_dir = root.path().join("repo-a");
        fs::create_dir_all(&repo_a_dir).expect("create repo-a dir");
        let repo_a_db = repo_a_dir.join("git.gen-00000001.redb");
        let absent_global_dir = root.path().join("absent-global");
        let absent_global_db = absent_global_dir.join(CONFIG_SQLITE_FILE_NAME);
        let absent_system_dir = root.path().join("absent-system");
        let absent_system_db = absent_system_dir.join(CONFIG_SQLITE_FILE_NAME);

        run_child(
            "repo_isolation",
            &[
                ("LIBRA_RB03J_REPO_DB", repo_a_db.clone()),
                (LIBRA_CONFIG_GLOBAL_DB_ENV, absent_global_db.clone()),
                (LIBRA_CONFIG_SYSTEM_DB_ENV, absent_system_db.clone()),
            ],
        );

        assert!(
            !absent_global_dir.exists() && !absent_global_db.exists(),
            "GC-13 violation: Repository write created absent LIBRA_CONFIG_GLOBAL_DB fixture"
        );
        assert!(
            !absent_system_dir.exists() && !absent_system_db.exists(),
            "GC-13 violation: Repository write created absent LIBRA_CONFIG_SYSTEM_DB fixture"
        );
        assert!(
            !config_redb_path_in_scope_dir(&absent_global_dir).exists()
                && !config_redb_path_in_scope_dir(&absent_system_dir).exists(),
            "GC-13 violation: Repository write created config.redb in absent global/system scope"
        );

        // Verify `create_in_scope_dir` refuses `DatabaseRole::Repository`.
        let bad_scope_err =
            RedbConfigStore::create_in_scope_dir(&repo_a_dir, DatabaseRole::Repository)
                .expect_err("create_in_scope_dir must reject DatabaseRole::Repository");
        assert_eq!(bad_scope_err.stable_code_str(), "LBR-IO-001");

        // -----------------------------------------------------------------
        // AC3 & AC4: Empty `GlobalConfig` scope dir creates exact `0600`
        // `config.redb` (even under permissive `umask(0o000)`), and empty
        // `SystemConfig` scope dir creates exact `0644` `config.redb` (even
        // under restrictive `umask(0o077)`).
        // -----------------------------------------------------------------
        let global_scope_dir = root.path().join("empty-global-scope");
        let system_scope_dir = root.path().join("empty-system-scope");
        fs::create_dir_all(&global_scope_dir).expect("create empty global scope dir");
        fs::create_dir_all(&system_scope_dir).expect("create empty system scope dir");

        let global_env_db = config_sqlite_path_in_scope_dir(&global_scope_dir);
        let system_env_db = config_sqlite_path_in_scope_dir(&system_scope_dir);
        let global_redb = config_redb_path_in_scope_dir(&global_scope_dir);
        let system_redb = config_redb_path_in_scope_dir(&system_scope_dir);

        run_child(
            "global_mode_permissive_umask",
            &[(LIBRA_CONFIG_GLOBAL_DB_ENV, global_env_db.clone())],
        );
        run_child(
            "system_mode_restrictive_umask",
            &[(LIBRA_CONFIG_SYSTEM_DB_ENV, system_env_db.clone())],
        );

        assert!(
            global_redb.is_file(),
            "GlobalConfig config.redb must be created in empty scope dir"
        );
        assert!(
            system_redb.is_file(),
            "SystemConfig config.redb must be created in empty scope dir"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let global_mode = fs::symlink_metadata(&global_redb)
                .expect("stat GlobalConfig config.redb")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                global_mode, GLOBAL_CONFIG_FILE_MODE,
                "GlobalConfig config.redb must have exact 0600 permissions, got {global_mode:04o}"
            );

            let system_mode = fs::symlink_metadata(&system_redb)
                .expect("stat SystemConfig config.redb")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                system_mode, SYSTEM_CONFIG_FILE_MODE,
                "SystemConfig config.redb must have exact 0644 permissions, got {system_mode:04o}"
            );

            let repo_mode = fs::symlink_metadata(&repo_a_db)
                .expect("stat Repository redb store")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                repo_mode, REPOSITORY_CONFIG_FILE_MODE,
                "Repository redb store must have exact 0600 permissions, got {repo_mode:04o}"
            );

            assert_eq!(
                expected_config_file_mode(DatabaseRole::Repository),
                Some(0o600)
            );
            assert_eq!(
                expected_config_file_mode(DatabaseRole::GlobalConfig),
                Some(0o600)
            );
            assert_eq!(
                expected_config_file_mode(DatabaseRole::SystemConfig),
                Some(0o644)
            );
            assert_eq!(expected_config_file_mode(DatabaseRole::Capture), None);
            assert_eq!(expected_config_file_mode(DatabaseRole::Derived), None);
        }

        // Verify persisted values and cross-role rejection on GlobalConfig and SystemConfig.
        let global_ro = RedbConfigStore::open_read_only(&global_redb, DatabaseRole::GlobalConfig)
            .expect("open GlobalConfig read-only");
        assert_eq!(
            global_ro.get("user.email").await.expect("read global key"),
            Some(ConfigEntry {
                key: "user.email".to_string(),
                value: "global@example.com".to_string(),
                encrypted: false,
            })
        );
        let wrong_global_role =
            RedbConfigStore::open_read_only(&global_redb, DatabaseRole::Repository)
                .expect_err("opening GlobalConfig store as Repository must fail closed");
        assert!(matches!(
            wrong_global_role,
            StorageBackendError::RoleMismatch {
                expected: DatabaseRole::Repository,
                found: DatabaseRole::GlobalConfig,
            }
        ));
        drop(global_ro);

        let system_ro = RedbConfigStore::open_read_only(&system_redb, DatabaseRole::SystemConfig)
            .expect("open SystemConfig read-only");
        assert_eq!(
            system_ro.get("core.editor").await.expect("read system key"),
            Some(ConfigEntry {
                key: "core.editor".to_string(),
                value: "vim".to_string(),
                encrypted: false,
            })
        );
        let wrong_system_role =
            RedbConfigStore::open_read_only(&system_redb, DatabaseRole::GlobalConfig)
                .expect_err("opening SystemConfig store as GlobalConfig must fail closed");
        assert!(matches!(
            wrong_system_role,
            StorageBackendError::RoleMismatch {
                expected: DatabaseRole::GlobalConfig,
                found: DatabaseRole::SystemConfig,
            }
        ));
        drop(system_ro);

        // -----------------------------------------------------------------
        // AC1 & AC2 (Part B): Pre-existing populated `LIBRA_CONFIG_GLOBAL_DB`
        // and `LIBRA_CONFIG_SYSTEM_DB` fixtures (both `config.db` and
        // `config.redb`) must remain byte-for-byte untouched after full
        // `Repository` `RedbConfigStore` creation and mutations.
        // -----------------------------------------------------------------
        fs::write(&global_env_db, b"PREEXISTING_GLOBAL_SQLITE_FIXTURE\n")
            .expect("seed preexisting global config.db fixture");
        fs::write(&system_env_db, b"PREEXISTING_SYSTEM_SQLITE_FIXTURE\n")
            .expect("seed preexisting system config.db fixture");

        let global_db_before = fs::read(&global_env_db).expect("read global config.db before");
        let global_redb_before = fs::read(&global_redb).expect("read global config.redb before");
        let system_db_before = fs::read(&system_env_db).expect("read system config.db before");
        let system_redb_before = fs::read(&system_redb).expect("read system config.redb before");

        let repo_b_dir = root.path().join("repo-b");
        fs::create_dir_all(&repo_b_dir).expect("create repo-b dir");
        let repo_b_db = repo_b_dir.join("git.gen-00000001.redb");

        run_child(
            "repo_isolation",
            &[
                ("LIBRA_RB03J_REPO_DB", repo_b_db.clone()),
                (LIBRA_CONFIG_GLOBAL_DB_ENV, global_env_db.clone()),
                (LIBRA_CONFIG_SYSTEM_DB_ENV, system_env_db.clone()),
            ],
        );

        assert_eq!(
            fs::read(&global_env_db).expect("read global config.db after"),
            global_db_before,
            "GC-13 violation: Repository write modified preexisting LIBRA_CONFIG_GLOBAL_DB"
        );
        assert_eq!(
            fs::read(&global_redb).expect("read global config.redb after"),
            global_redb_before,
            "GC-13 violation: Repository write modified preexisting GlobalConfig config.redb"
        );
        assert_eq!(
            fs::read(&system_env_db).expect("read system config.db after"),
            system_db_before,
            "GC-13 violation: Repository write modified preexisting LIBRA_CONFIG_SYSTEM_DB"
        );
        assert_eq!(
            fs::read(&system_redb).expect("read system config.redb after"),
            system_redb_before,
            "GC-13 violation: Repository write modified preexisting SystemConfig config.redb"
        );

        // Confirm subsequent writable reopen & mutation on GlobalConfig and SystemConfig
        // preserves exact `0600` / `0644` modes on Unix.
        let global_rw = RedbConfigStore::open_read_write(&global_redb, DatabaseRole::GlobalConfig)
            .expect("reopen GlobalConfig rw");
        global_rw
            .set("user.name", "Global Mutated", false)
            .await
            .expect("mutate GlobalConfig");
        drop(global_rw);

        let system_rw = RedbConfigStore::open_read_write(&system_redb, DatabaseRole::SystemConfig)
            .expect("reopen SystemConfig rw");
        system_rw
            .set("core.pager", "less", false)
            .await
            .expect("mutate SystemConfig");
        drop(system_rw);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::symlink_metadata(&global_redb)
                    .expect("stat GlobalConfig after reopen write")
                    .permissions()
                    .mode()
                    & 0o777,
                GLOBAL_CONFIG_FILE_MODE,
                "GlobalConfig config.redb must retain exact 0600 mode after writes"
            );
            assert_eq!(
                fs::symlink_metadata(&system_redb)
                    .expect("stat SystemConfig after reopen write")
                    .permissions()
                    .mode()
                    & 0o777,
                SYSTEM_CONFIG_FILE_MODE,
                "SystemConfig config.redb must retain exact 0644 mode after writes"
            );
        }
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn config_scope_manifest_pending_dual_file_and_tombstone_acceptance() {
        use std::{
            env, fs,
            path::PathBuf,
            process::{Command, Stdio},
        };

        use tempfile::tempdir;

        use crate::storage::{
            backend::LIBRA_STORAGE_BACKEND_ENV, redb::RedbConfigStore, sqlite::SqliteConfigStore,
        };

        // Isolated child mode for AC1: prove `LIBRA_STORAGE_BACKEND=sqlite` and
        // `LIBRA_STORAGE_BACKEND=redb` CANNOT bypass `config-format.json(finalized=false)`.
        if let Some(child_dir) = env::var_os("LIBRA_RB03B_PENDING_DIR") {
            let role = match env::var("LIBRA_RB03B_ROLE").expect("role env").as_str() {
                "global" => DatabaseRole::GlobalConfig,
                "system" => DatabaseRole::SystemConfig,
                other => panic!("unexpected LIBRA_RB03B_ROLE: {other}"),
            };
            let dir_path = PathBuf::from(child_dir);
            let inspect_err = inspect_config_scope_authority(&dir_path, role)
                .expect_err("inspect must fail closed when finalized=false");
            assert!(
                matches!(
                    inspect_err,
                    StorageBackendError::ManifestNotFinalized { .. }
                ),
                "expected ManifestNotFinalized from inspect, got {inspect_err:?}"
            );
            assert_eq!(inspect_err.stable_code_str(), "LBR-IO-001");

            let open_err = open_config_scope_read_only(&dir_path, role)
                .await
                .expect_err("open_config_scope_read_only must fail closed when finalized=false");
            assert!(
                matches!(open_err, StorageBackendError::ManifestNotFinalized { .. }),
                "expected ManifestNotFinalized from open_read_only, got {open_err:?}"
            );
            assert_eq!(open_err.stable_code_str(), "LBR-IO-001");

            let open_rw_err = open_config_scope_read_write(&dir_path, role)
                .await
                .expect_err("open_config_scope_read_write must fail closed when finalized=false");
            assert!(
                matches!(
                    open_rw_err,
                    StorageBackendError::ManifestNotFinalized { .. }
                ),
                "expected ManifestNotFinalized from open_read_write, got {open_rw_err:?}"
            );
            assert_eq!(open_rw_err.stable_code_str(), "LBR-IO-001");
            println!("RB03B_PENDING_CHILD_OK");
            return;
        }

        let set_readonly_tombstone_mode = |path: &Path, role: DatabaseRole| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = expected_config_tombstone_mode(role).expect("tombstone mode");
                fs::set_permissions(path, fs::Permissions::from_mode(mode))
                    .expect("set readonly tombstone mode");
            }
            #[cfg(not(unix))]
            {
                let _ = role;
                let mut perms = fs::metadata(path).expect("stat tombstone").permissions();
                perms.set_readonly(true);
                fs::set_permissions(path, perms).expect("set readonly attribute");
            }
        };

        let clear_readonly_mode = |path: &Path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
            }
            #[cfg(not(unix))]
            {
                if let Ok(meta) = fs::metadata(path) {
                    let mut perms = meta.permissions();
                    perms.set_readonly(false);
                    let _ = fs::set_permissions(path, perms);
                }
            }
        };

        let exe = env::current_exe().expect("locate current test executable");
        let root = tempdir().expect("create RB-03b temp root");

        for (role_label, role) in [
            ("global", DatabaseRole::GlobalConfig),
            ("system", DatabaseRole::SystemConfig),
        ] {
            // -------------------------------------------------------------
            // AC1: `config-format.json` with `finalized == false` (both
            // `init_phase="migration"` and `init_phase="fresh_create"`) MUST
            // fail closed with `LBR-IO-001` ignoring `LIBRA_STORAGE_BACKEND`.
            // -------------------------------------------------------------
            for (phase_label, manifest) in [
                (
                    "migration",
                    ConfigFormatManifest {
                        schema_version: CONFIG_FORMAT_MANIFEST_VERSION,
                        backend: StorageBackendKind::Redb,
                        finalized: false,
                        init_phase: Some(ConfigInitPhase::Migration),
                        generation: 1,
                        legacy_sqlite_tombstone: false,
                        targets_staged_for_rename: false,
                        source_db_state: Some(Phase2DbState::Pending),
                        backup_sha256: Some("abc123".to_string()),
                        source_logical_sha256: Some("def456".to_string()),
                    },
                ),
                (
                    "fresh_create",
                    ConfigFormatManifest::new_redb_fresh_create(1, false, role)
                        .expect("build fresh_create manifest"),
                ),
            ] {
                let pending_dir = root
                    .path()
                    .join(format!("{role_label}-pending-{phase_label}"));
                fs::create_dir_all(&pending_dir).expect("create pending scope dir");
                publish_config_format_manifest(&pending_dir, role, &manifest)
                    .expect("publish unfinalized config-format.json");
                let read_back = read_config_format_manifest(&pending_dir, role)
                    .expect("read back unfinalized manifest")
                    .expect("manifest must exist");
                assert_eq!(read_back, manifest);

                for env_override in ["sqlite", "redb"] {
                    let output = Command::new(&exe)
                        .arg("--exact")
                        .arg("storage::config_store::tests::config_scope_manifest_pending_dual_file_and_tombstone_acceptance")
                        .arg("--nocapture")
                        .env("LIBRA_RB03B_PENDING_DIR", &pending_dir)
                        .env("LIBRA_RB03B_ROLE", role_label)
                        .env(LIBRA_STORAGE_BACKEND_ENV, env_override)
                        .env("LIBRA_TEST", "1")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .output()
                        .expect("spawn RB-03b pending child");
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    assert!(
                        output.status.success()
                            && stdout.contains("RB03B_PENDING_CHILD_OK")
                            && stdout.contains("1 passed; 0 failed"),
                        "RB-03b pending child ({role_label}/{phase_label}/env={env_override}) failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
                    );
                }
            }

            // -------------------------------------------------------------
            // AC2: Missing `config-format.json` with BOTH `config.db` and
            // `config.redb` present MUST fail closed with `LBR-IO-001`.
            // -------------------------------------------------------------
            let split_dir = root.path().join(format!("{role_label}-split-no-manifest"));
            fs::create_dir_all(&split_dir).expect("create split scope dir");
            let split_sqlite = config_sqlite_path_in_scope_dir(&split_dir);
            let split_redb = config_redb_path_in_scope_dir(&split_dir);
            SqliteConfigStore::create(&split_sqlite, role)
                .await
                .expect("create split config.db");
            RedbConfigStore::create(&split_redb, role).expect("create split config.redb");

            let split_inspect_err = inspect_config_scope_authority(&split_dir, role)
                .expect_err("dual files without config-format.json must fail closed");
            assert_eq!(split_inspect_err.stable_code_str(), "LBR-IO-001");
            let split_open_err = open_config_scope_read_only(&split_dir, role)
                .await
                .expect_err("open_config_scope_read_only must reject dual files without manifest");
            assert_eq!(split_open_err.stable_code_str(), "LBR-IO-001");
            let split_rw_err = open_config_scope_read_write(&split_dir, role)
                .await
                .expect_err("open_config_scope_read_write must reject dual files without manifest");
            assert_eq!(split_rw_err.stable_code_str(), "LBR-IO-001");

            // Also verify lone unmanifested sentinel tombstone fails closed with LBR-IO-001.
            let orphan_tombstone_dir = root.path().join(format!("{role_label}-orphan-tombstone"));
            fs::create_dir_all(&orphan_tombstone_dir).expect("create orphan tombstone dir");
            fs::write(
                config_sqlite_path_in_scope_dir(&orphan_tombstone_dir),
                CONFIG_SQLITE_TOMBSTONE_SENTINEL,
            )
            .expect("write orphan sentinel tombstone");
            assert!(
                is_config_sqlite_tombstone_sentinel(&config_sqlite_path_in_scope_dir(
                    &orphan_tombstone_dir
                ))
                .expect("detect tombstone sentinel")
            );
            let orphan_tombstone_err = open_config_scope_read_only(&orphan_tombstone_dir, role)
                .await
                .expect_err("orphan tombstone without manifest must fail closed");
            assert_eq!(orphan_tombstone_err.stable_code_str(), "LBR-IO-001");

            // -------------------------------------------------------------
            // AC3: `config-format.json(backend="redb", finalized=true,
            // legacy_sqlite_tombstone=true)` MUST require and verify a valid
            // read-only `config.db` tombstone (31-byte sentinel OR committed
            // `__libra_migrated_fence__` SQLite fence) and reject missing,
            // writable, or unfenced SQLite `config.db` files with `LBR-IO-001`.
            // -------------------------------------------------------------
            let tombstone_dir = root
                .path()
                .join(format!("{role_label}-finalized-tombstone"));
            fs::create_dir_all(&tombstone_dir).expect("create finalized tombstone scope dir");
            let tomb_sqlite = config_sqlite_path_in_scope_dir(&tombstone_dir);
            let tomb_redb = config_redb_path_in_scope_dir(&tombstone_dir);

            let redb_store = RedbConfigStore::create(&tomb_redb, role)
                .expect("create authoritative config.redb");
            redb_store
                .set("authority.source", "from-redb-with-tombstone", false)
                .await
                .expect("seed authoritative config.redb");
            drop(redb_store);

            let finalized_with_tombstone = ConfigFormatManifest::new_redb_finalized(1, true, role)
                .expect("build finalized redb tombstone manifest");
            publish_config_format_manifest(&tombstone_dir, role, &finalized_with_tombstone)
                .expect("publish finalized redb tombstone manifest");

            // AC3 negative 1: missing `config.db` while `legacy_sqlite_tombstone=true` MUST fail closed.
            let missing_tomb_err = open_config_scope_read_only(&tombstone_dir, role)
                .await
                .expect_err("missing config.db when legacy_sqlite_tombstone=true must fail closed");
            assert_eq!(missing_tomb_err.stable_code_str(), "LBR-IO-001");

            // AC3 negative 2: writable sentinel `config.db` MUST fail closed until read-only.
            fs::write(&tomb_sqlite, CONFIG_SQLITE_TOMBSTONE_SENTINEL)
                .expect("write writable sentinel config.db");
            let writable_tomb_err = open_config_scope_read_only(&tombstone_dir, role)
                .await
                .expect_err("writable sentinel config.db must fail closed");
            assert_eq!(writable_tomb_err.stable_code_str(), "LBR-IO-001");

            // AC3 positive 1: read-only 31-byte sentinel `config.db` (`0400`/`0444`) is accepted
            // for both read-only and read-write scoped opens.
            set_readonly_tombstone_mode(&tomb_sqlite, role);
            let authority = inspect_config_scope_authority(&tombstone_dir, role)
                .expect("finalized redb manifest with verified sentinel tombstone must succeed");
            assert_eq!(authority.backend(), Some(StorageBackendKind::Redb));
            assert!(matches!(
                authority,
                ConfigScopeAuthority::Manifest { ref db_path, .. } if db_path == &tomb_redb
            ));

            let opened = open_config_scope_read_only(&tombstone_dir, role)
                .await
                .expect("open_config_scope_read_only must open authoritative config.redb");
            assert_eq!(opened.backend(), StorageBackendKind::Redb);
            assert_eq!(opened.role(), role);
            assert_eq!(
                opened
                    .get("authority.source")
                    .await
                    .expect("read key from authoritative config.redb"),
                Some(ConfigEntry {
                    key: "authority.source".to_string(),
                    value: "from-redb-with-tombstone".to_string(),
                    encrypted: false,
                })
            );
            drop(opened);

            let opened_rw = open_config_scope_read_write(&tombstone_dir, role)
                .await
                .expect("open_config_scope_read_write must open authoritative config.redb");
            opened_rw
                .set("authority.rw", "ok", false)
                .await
                .expect("write through guarded read-write redb scope");
            drop(opened_rw);

            // AC3 negative 3: ordinary unfenced SQLite DB (even marked read-only and even storing
            // the fence SQL string inside `config_kv`) MUST fail closed with `LBR-IO-001`.
            let unfenced_dir = root
                .path()
                .join(format!("{role_label}-unfenced-sqlite-tombstone"));
            fs::create_dir_all(&unfenced_dir).expect("create unfenced sqlite scope dir");
            let unfenced_sqlite = config_sqlite_path_in_scope_dir(&unfenced_dir);
            let unfenced_redb = config_redb_path_in_scope_dir(&unfenced_dir);
            RedbConfigStore::create(&unfenced_redb, role).expect("create redb in unfenced scope");
            {
                let decoy_sqlite = SqliteConfigStore::create(&unfenced_sqlite, role)
                    .await
                    .expect("create unfenced decoy sqlite config.db");
                decoy_sqlite
                    .set(
                        SQLITE_MIGRATED_FENCE_TABLE,
                        SQLITE_MIGRATED_FENCE_SQL,
                        false,
                    )
                    .await
                    .expect("store decoy fence string inside config_kv");
            }
            // Switch decoy SQLite to DELETE journal mode (removing any WAL/SHM) on a single-connection
            // pool so only the absence of a real `sqlite_master` fence distinguishes it.
            {
                use sea_orm::{ConnectOptions, ConnectionTrait, Database, Statement};
                let mut opts = ConnectOptions::new("sqlite://rb03b-decoy-single");
                opts.max_connections(1).sqlx_logging(false);
                let db_file = unfenced_sqlite.clone();
                opts.map_sqlx_sqlite_opts(move |o| o.filename(&db_file).create_if_missing(false));
                let conn = Database::connect(opts)
                    .await
                    .expect("open single-conn decoy sqlite");
                conn.execute_raw(Statement::from_string(
                    sea_orm::DatabaseBackend::Sqlite,
                    "PRAGMA journal_mode = DELETE;",
                ))
                .await
                .expect("switch decoy sqlite to DELETE journal mode");
                conn.close().await.expect("close decoy sqlite");
            }
            set_readonly_tombstone_mode(&unfenced_sqlite, role);
            publish_config_format_manifest(&unfenced_dir, role, &finalized_with_tombstone)
                .expect("publish manifest in unfenced scope");
            assert!(
                !is_config_sqlite_fenced_tombstone(&unfenced_sqlite)
                    .expect("decoy sqlite fence check must run cleanly"),
                "unfenced sqlite storing fence text in config_kv must NOT pass sqlite_master check"
            );
            let unfenced_err = open_config_scope_read_only(&unfenced_dir, role)
                .await
                .expect_err("unfenced sqlite config.db must fail closed even when read-only");
            assert_eq!(unfenced_err.stable_code_str(), "LBR-IO-001");
            clear_readonly_mode(&unfenced_sqlite);

            // AC3 positive 2: real SQLite DB fenced via `ADR-RB-07` #1 `__libra_migrated_fence__`
            // rollback-journal fence and marked read-only (`0400`/`0444`) MUST be accepted!
            {
                use sea_orm::{ConnectOptions, ConnectionTrait, Database, Statement};
                let mut opts = ConnectOptions::new("sqlite://rb03b-fence-single");
                opts.max_connections(1).sqlx_logging(false);
                let db_file = unfenced_sqlite.clone();
                opts.map_sqlx_sqlite_opts(move |o| o.filename(&db_file).create_if_missing(false));
                let conn = Database::connect(opts)
                    .await
                    .expect("open single-conn sqlite for real fence");
                for stmt in [
                    "PRAGMA journal_mode = DELETE;".to_string(),
                    "PRAGMA synchronous = FULL;".to_string(),
                    "BEGIN EXCLUSIVE;".to_string(),
                    format!("CREATE TABLE {SQLITE_MIGRATED_FENCE_TABLE} (x INTEGER);"),
                    "PRAGMA writable_schema = ON;".to_string(),
                    format!(
                        "UPDATE sqlite_master SET sql = '{SQLITE_MIGRATED_FENCE_SQL}' \
                         WHERE name = '{SQLITE_MIGRATED_FENCE_TABLE}';"
                    ),
                    "PRAGMA writable_schema = OFF;".to_string(),
                    "COMMIT;".to_string(),
                ] {
                    conn.execute_raw(Statement::from_string(
                        sea_orm::DatabaseBackend::Sqlite,
                        stmt,
                    ))
                    .await
                    .expect("execute sqlite fence statement");
                }
                conn.close().await.expect("close fenced sqlite");
            }
            set_readonly_tombstone_mode(&unfenced_sqlite, role);
            assert!(
                is_config_sqlite_fenced_tombstone(&unfenced_sqlite)
                    .expect("detect real __libra_migrated_fence__"),
                "real __libra_migrated_fence__ SQLite DB must pass sqlite_master fence verification"
            );
            let fenced_opened = open_config_scope_read_only(&unfenced_dir, role)
                .await
                .expect("real fenced migrated SQLite tombstone must be accepted");
            assert_eq!(fenced_opened.backend(), StorageBackendKind::Redb);
            drop(fenced_opened);
            clear_readonly_mode(&unfenced_sqlite);

            // AC3 negative 4: flipping `legacy_sqlite_tombstone = false` while `config.db` still
            // exists MUST immediately fail closed with `LBR-IO-001`.
            let finalized_no_tombstone = ConfigFormatManifest::new_redb_finalized(1, false, role)
                .expect("build finalized redb non-tombstone manifest");
            publish_config_format_manifest(&tombstone_dir, role, &finalized_no_tombstone)
                .expect("publish finalized redb non-tombstone manifest");
            let unexpected_coexist_err = open_config_scope_read_only(&tombstone_dir, role)
                .await
                .expect_err("coexisting config.db without legacy_sqlite_tombstone=true must fail");
            assert_eq!(unexpected_coexist_err.stable_code_str(), "LBR-IO-001");
            clear_readonly_mode(&tomb_sqlite);

            // -------------------------------------------------------------
            // AC4: Empty or non-existent config scope dir opened read-only
            // MUST return `NotFound` and create ZERO directories, lock files,
            // manifests, or databases (`ADR-CF-06`).
            // -------------------------------------------------------------
            let nonexistent_dir = root.path().join(format!("{role_label}-nonexistent-scope"));
            let missing_err = open_config_scope_read_only(&nonexistent_dir, role)
                .await
                .expect_err("nonexistent scope dir read-only open must return NotFound");
            assert!(
                matches!(missing_err, StorageBackendError::NotFound { .. }),
                "expected NotFound for nonexistent scope dir, got {missing_err:?}"
            );
            assert!(
                !nonexistent_dir.exists(),
                "ADR-CF-06 violation: read-only open created missing scope directory"
            );
            assert!(
                !config_migration_lock_path(&nonexistent_dir).exists(),
                "ADR-CF-06 violation: read-only open created config-migration.lock"
            );

            let empty_dir = root.path().join(format!("{role_label}-empty-scope"));
            fs::create_dir_all(&empty_dir).expect("create empty scope dir");
            let empty_err = open_config_scope_read_only(&empty_dir, role)
                .await
                .expect_err("empty scope dir read-only open must return NotFound");
            assert!(
                matches!(empty_err, StorageBackendError::NotFound { .. }),
                "expected NotFound for empty scope dir, got {empty_err:?}"
            );
            let remaining_entries: Vec<_> = fs::read_dir(&empty_dir)
                .expect("read empty scope dir")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect empty scope dir entries");
            assert!(
                remaining_entries.is_empty(),
                "ADR-CF-06 violation: read-only open created files in empty scope dir: {remaining_entries:?}"
            );
        }

        // Extra defense-in-depth checks: future `schema_version` maps to `LBR-CFG-002`
        // (`ConfigSchemaFuture`), invalid JSON never leaks secret values, and
        // `DatabaseRole::Repository` is rejected by scope manifest APIs.
        let future_dir = root.path().join("future-manifest-schema");
        fs::create_dir_all(&future_dir).expect("create future manifest dir");
        fs::write(
            config_format_manifest_path(&future_dir),
            r#"{"schema_version":99,"backend":"redb","finalized":true,"generation":1}"#,
        )
        .expect("write future schema manifest");
        let future_err = inspect_config_scope_authority(&future_dir, DatabaseRole::GlobalConfig)
            .expect_err("future schema_version must fail closed");
        assert!(matches!(
            future_err,
            StorageBackendError::SchemaTooNew {
                role: DatabaseRole::GlobalConfig,
                found: 99,
                max_supported: CONFIG_FORMAT_MANIFEST_VERSION,
            }
        ));
        assert_eq!(future_err.stable_code_str(), "LBR-CONFIG-001");

        let corrupt_dir = root.path().join("corrupt-manifest");
        fs::create_dir_all(&corrupt_dir).expect("create corrupt manifest dir");
        let secret_marker = "sk-secret-must-not-leak-rb03b";
        fs::write(
            config_format_manifest_path(&corrupt_dir),
            format!(r#"{{"schema_version":1,"backend":"{secret_marker}","finalized":true}}"#),
        )
        .expect("write corrupt manifest with secret value");
        let corrupt_err = inspect_config_scope_authority(&corrupt_dir, DatabaseRole::GlobalConfig)
            .expect_err("corrupt backend value must fail closed");
        assert_eq!(corrupt_err.stable_code_str(), "LBR-IO-001");
        assert!(
            !corrupt_err.to_string().contains(secret_marker),
            "error output must not leak raw JSON field values: {corrupt_err}"
        );

        let repo_role_err = inspect_config_scope_authority(&future_dir, DatabaseRole::Repository)
            .expect_err("Repository role must be rejected by config scope manifest inspector");
        assert_eq!(repo_role_err.stable_code_str(), "LBR-IO-001");
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn config_scope_bounded_lock_shared_concurrency_and_timeout_error() {
        use std::{
            env, fs,
            path::PathBuf,
            process::{Child, Command, Stdio},
        };

        use tempfile::tempdir;

        use crate::storage::redb::RedbConfigStore;

        const CONCURRENT_WORKERS: usize = 4;
        const CHILD_TIMEOUT: Duration = Duration::from_secs(20);

        struct ChildReaper(Vec<(String, Child)>);
        impl Drop for ChildReaper {
            fn drop(&mut self) {
                for (_, mut child) in self.0.drain(..) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }

        // Child worker mode for genuine multi-process flock verification.
        if let Some(child_mode) = env::var_os("LIBRA_RB03H_CHILD_MODE") {
            let scope_dir =
                PathBuf::from(env::var_os("LIBRA_RB03H_SCOPE_DIR").expect("scope dir env"));
            let sync_dir =
                PathBuf::from(env::var_os("LIBRA_RB03H_SYNC_DIR").expect("sync dir env"));
            let role = match env::var("LIBRA_RB03H_ROLE").expect("role env").as_str() {
                "global" => DatabaseRole::GlobalConfig,
                "system" => DatabaseRole::SystemConfig,
                other => panic!("unexpected LIBRA_RB03H_ROLE: {other}"),
            };

            match child_mode.to_string_lossy().as_ref() {
                "shared_worker" => {
                    let worker_id = env::var("LIBRA_RB03H_WORKER_ID").expect("worker id env");
                    let locked = open_locked_config_scope_read_write(
                        &scope_dir,
                        role,
                        Duration::from_secs(5),
                    )
                    .await
                    .expect("shared worker must acquire LOCK_SH and open store");
                    assert!(locked.is_locked());

                    // Signal that this process currently holds `LOCK_SH`, then wait
                    // until ALL sibling processes simultaneously hold `LOCK_SH` too.
                    fs::write(sync_dir.join(format!("held-{worker_id}")), b"ok")
                        .expect("signal shared lock held");
                    let release_marker = sync_dir.join("release-shared");
                    let wait_deadline = Instant::now() + CHILD_TIMEOUT;
                    while !release_marker.exists() {
                        assert!(
                            Instant::now() < wait_deadline,
                            "shared worker `{worker_id}` timed out waiting for release signal"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }

                    // Perform a real same-backend read + write while still holding `LOCK_SH`.
                    let seeded = locked
                        .store()
                        .get("scope.seed")
                        .await
                        .expect("shared worker read");
                    assert_eq!(seeded.map(|e| e.value), Some("shared-ready".to_string()));
                    locked
                        .store()
                        .set(&format!("worker.{worker_id}"), "committed", false)
                        .await
                        .expect("shared worker write");
                    drop(locked);
                    println!("RB03H_CHILD_OK:shared_worker:{worker_id}");
                    return;
                }
                "hold_exclusive" => {
                    let _lock = ConfigScopeLock::try_lock_exclusive_bounded(
                        &scope_dir,
                        role,
                        Duration::from_secs(5),
                    )
                    .expect("exclusive holder must acquire LOCK_EX");
                    assert!(_lock.is_exclusive());
                    fs::write(sync_dir.join("ex-held"), b"ok").expect("signal LOCK_EX held");
                    let release_marker = sync_dir.join("release-ex");
                    let wait_deadline = Instant::now() + CHILD_TIMEOUT;
                    while !release_marker.exists() {
                        assert!(
                            Instant::now() < wait_deadline,
                            "exclusive holder timed out waiting for release"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    drop(_lock);
                    println!("RB03H_CHILD_OK:hold_exclusive");
                    return;
                }
                "hold_shared" => {
                    let _lock = ConfigScopeLock::try_lock_shared_bounded(
                        &scope_dir,
                        role,
                        Duration::from_secs(5),
                    )
                    .expect("shared holder must acquire LOCK_SH");
                    assert!(!_lock.is_exclusive());
                    fs::write(sync_dir.join("sh-held"), b"ok").expect("signal LOCK_SH held");
                    let release_marker = sync_dir.join("release-sh");
                    let wait_deadline = Instant::now() + CHILD_TIMEOUT;
                    while !release_marker.exists() {
                        assert!(
                            Instant::now() < wait_deadline,
                            "shared holder timed out waiting for release"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    drop(_lock);
                    println!("RB03H_CHILD_OK:hold_shared");
                    return;
                }
                other => panic!("unexpected LIBRA_RB03H_CHILD_MODE: {other}"),
            }
        }

        let exe = env::current_exe().expect("locate current test executable");
        let root = tempdir().expect("create RB-03h lock test temp root");
        let scope_dir = root.path().join("global-scope");
        let sync_dir = root.path().join("sync");
        fs::create_dir_all(&scope_dir).expect("create global scope dir");
        fs::create_dir_all(&sync_dir).expect("create sync dir");

        // Seed finalized redb scope + verified read-only sentinel tombstone.
        let redb_path = config_redb_path_in_scope_dir(&scope_dir);
        let sqlite_path = config_sqlite_path_in_scope_dir(&scope_dir);
        let seed = RedbConfigStore::create(&redb_path, DatabaseRole::GlobalConfig)
            .expect("create seeded redb config store");
        seed.set("scope.seed", "shared-ready", false)
            .await
            .expect("seed scope.seed");
        drop(seed);
        fs::write(&sqlite_path, CONFIG_SQLITE_TOMBSTONE_SENTINEL).expect("write tombstone");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &sqlite_path,
                fs::Permissions::from_mode(GLOBAL_CONFIG_TOMBSTONE_MODE),
            )
            .expect("chmod 0400 tombstone");
        }
        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(&sqlite_path)
                .expect("stat tombstone")
                .permissions();
            perms.set_readonly(true);
            fs::set_permissions(&sqlite_path, perms).expect("set readonly tombstone");
        }
        let manifest =
            ConfigFormatManifest::new_redb_finalized(1, true, DatabaseRole::GlobalConfig)
                .expect("build finalized global manifest");
        publish_config_format_manifest(&scope_dir, DatabaseRole::GlobalConfig, &manifest)
            .expect("publish finalized global manifest");

        // -----------------------------------------------------------------
        // AC1: Multi-process steady-state concurrent `LOCK_SH` acquisition
        // and concurrent same-backend reads/writes across 4 OS processes.
        // -----------------------------------------------------------------
        let mut reaper = ChildReaper(Vec::new());
        for idx in 0..CONCURRENT_WORKERS {
            let child = Command::new(&exe)
                .arg("--exact")
                .arg("storage::config_store::tests::config_scope_bounded_lock_shared_concurrency_and_timeout_error")
                .arg("--nocapture")
                .env("LIBRA_RB03H_CHILD_MODE", "shared_worker")
                .env("LIBRA_RB03H_SCOPE_DIR", &scope_dir)
                .env("LIBRA_RB03H_SYNC_DIR", &sync_dir)
                .env("LIBRA_RB03H_ROLE", "global")
                .env("LIBRA_RB03H_WORKER_ID", idx.to_string())
                .env("LIBRA_TEST", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn shared worker");
            reaper.0.push((format!("worker-{idx}"), child));
        }

        // Wait until ALL 4 OS processes simultaneously hold `LOCK_SH` (proving true
        // shared lock concurrency — if the lock were exclusive this would deadlock).
        let barrier_deadline = Instant::now() + CHILD_TIMEOUT;
        for idx in 0..CONCURRENT_WORKERS {
            let marker = sync_dir.join(format!("held-{idx}"));
            while !marker.exists() {
                assert!(
                    Instant::now() < barrier_deadline,
                    "timed out waiting for shared worker {idx} to acquire LOCK_SH concurrently"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        // While all 4 processes hold `LOCK_SH`, an exclusive lock MUST time out immediately
        // with `WriteLockBusy` (`LBR-IO-001`) under `Duration::ZERO` (AC3).
        let ex_while_sh_err = ConfigScopeLock::try_lock_exclusive_bounded(
            &scope_dir,
            DatabaseRole::GlobalConfig,
            Duration::ZERO,
        )
        .expect_err("try_lock_exclusive_bounded must fail closed while shared locks are held");
        assert!(
            matches!(
                ex_while_sh_err,
                StorageBackendError::WriteLockBusy {
                    role: DatabaseRole::GlobalConfig,
                    ..
                }
            ),
            "expected WriteLockBusy on exclusive vs shared contention, got {ex_while_sh_err:?}"
        );
        assert_eq!(ex_while_sh_err.stable_code_str(), "LBR-IO-001");

        // Release all 4 shared workers and confirm every worker's write committed cleanly.
        fs::write(sync_dir.join("release-shared"), b"go").expect("release shared workers");
        for (idx, (name, child)) in reaper.0.drain(..).enumerate() {
            let output = child.wait_with_output().expect("join shared worker");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success()
                    && stdout.contains(&format!("RB03H_CHILD_OK:shared_worker:{idx}"))
                    && stdout.contains("1 passed; 0 failed"),
                "shared worker `{name}` failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }

        let verify_ro = open_config_scope_read_only(&scope_dir, DatabaseRole::GlobalConfig)
            .await
            .expect("verify shared writes via read-only open");
        for idx in 0..CONCURRENT_WORKERS {
            assert_eq!(
                verify_ro
                    .get(&format!("worker.{idx}"))
                    .await
                    .expect("read worker key")
                    .map(|e| e.value),
                Some("committed".to_string())
            );
        }
        drop(verify_ro);

        // Verify `config-migration.lock` has exact `0600` mode for GlobalConfig on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let lock_mode = fs::symlink_metadata(config_migration_lock_path(&scope_dir))
                .expect("stat global config-migration.lock")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                lock_mode, GLOBAL_CONFIG_FILE_MODE,
                "GlobalConfig config-migration.lock must have mode 0600, got {lock_mode:04o}"
            );
        }

        // -----------------------------------------------------------------
        // AC2 & AC3: External process holds `LOCK_EX`; both
        // `try_lock_shared_bounded` AND `try_lock_exclusive_bounded` (plus
        // runtime RO/RW opens) MUST return typed `WriteLockBusy` (`LBR-IO-001`)
        // under `Duration::ZERO` and bounded non-zero deadline without hanging.
        // -----------------------------------------------------------------
        let ex_child = Command::new(&exe)
            .arg("--exact")
            .arg("storage::config_store::tests::config_scope_bounded_lock_shared_concurrency_and_timeout_error")
            .arg("--nocapture")
            .env("LIBRA_RB03H_CHILD_MODE", "hold_exclusive")
            .env("LIBRA_RB03H_SCOPE_DIR", &scope_dir)
            .env("LIBRA_RB03H_SYNC_DIR", &sync_dir)
            .env("LIBRA_RB03H_ROLE", "global")
            .env("LIBRA_TEST", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn exclusive holder child");
        reaper.0.push(("hold_exclusive".to_string(), ex_child));

        let ex_marker = sync_dir.join("ex-held");
        let ex_wait = Instant::now() + CHILD_TIMEOUT;
        while !ex_marker.exists() {
            assert!(
                Instant::now() < ex_wait,
                "timed out waiting for external LOCK_EX holder"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // AC2: `try_lock_shared_bounded` with `Duration::ZERO` -> `LBR-IO-001`.
        let sh_zero_err = ConfigScopeLock::try_lock_shared_bounded(
            &scope_dir,
            DatabaseRole::GlobalConfig,
            Duration::ZERO,
        )
        .expect_err("try_lock_shared_bounded(0) must fail while LOCK_EX is held");
        assert!(matches!(
            sh_zero_err,
            StorageBackendError::WriteLockBusy {
                role: DatabaseRole::GlobalConfig,
                ..
            }
        ));
        assert_eq!(sh_zero_err.stable_code_str(), "LBR-IO-001");

        // AC2: `try_lock_shared_bounded` with short non-zero deadline -> bounded `LBR-IO-001`.
        let t0 = Instant::now();
        let sh_short_err = ConfigScopeLock::try_lock_shared_bounded(
            &scope_dir,
            DatabaseRole::GlobalConfig,
            Duration::from_millis(45),
        )
        .expect_err("try_lock_shared_bounded(45ms) must fail while LOCK_EX is held");
        let elapsed_sh = t0.elapsed();
        assert_eq!(sh_short_err.stable_code_str(), "LBR-IO-001");
        assert!(
            elapsed_sh >= Duration::from_millis(35) && elapsed_sh < Duration::from_secs(2),
            "bounded shared lock wait elapsed out of expected window: {elapsed_sh:?}"
        );

        // AC2 on runtime RO & RW entrypoints.
        let ro_busy = open_config_scope_read_only_with_deadline(
            &scope_dir,
            DatabaseRole::GlobalConfig,
            Duration::ZERO,
        )
        .await
        .expect_err("runtime RO open must return LBR-IO-001 when LOCK_EX is held");
        assert_eq!(ro_busy.stable_code_str(), "LBR-IO-001");

        let rw_busy = open_config_scope_read_write_with_deadline(
            &scope_dir,
            DatabaseRole::GlobalConfig,
            Duration::ZERO,
        )
        .await
        .expect_err("runtime RW open must return LBR-IO-001 when LOCK_EX is held");
        assert_eq!(rw_busy.stable_code_str(), "LBR-IO-001");

        // AC3: `try_lock_exclusive_bounded` with `Duration::ZERO` and short deadline -> `LBR-IO-001`.
        let ex_zero_err = ConfigScopeLock::try_lock_exclusive_bounded(
            &scope_dir,
            DatabaseRole::GlobalConfig,
            Duration::ZERO,
        )
        .expect_err("try_lock_exclusive_bounded(0) must fail while LOCK_EX is held");
        assert!(matches!(
            ex_zero_err,
            StorageBackendError::WriteLockBusy {
                role: DatabaseRole::GlobalConfig,
                ..
            }
        ));
        assert_eq!(ex_zero_err.stable_code_str(), "LBR-IO-001");

        let t1 = Instant::now();
        let ex_short_err = ConfigScopeLock::try_lock_exclusive_bounded(
            &scope_dir,
            DatabaseRole::GlobalConfig,
            Duration::from_millis(45),
        )
        .expect_err("try_lock_exclusive_bounded(45ms) must fail while LOCK_EX is held");
        let elapsed_ex = t1.elapsed();
        assert_eq!(ex_short_err.stable_code_str(), "LBR-IO-001");
        assert!(
            elapsed_ex >= Duration::from_millis(35) && elapsed_ex < Duration::from_secs(2),
            "bounded exclusive lock wait elapsed out of expected window: {elapsed_ex:?}"
        );

        // Release external `LOCK_EX` and verify immediate acquisition succeeds again.
        fs::write(sync_dir.join("release-ex"), b"go").expect("release exclusive holder");
        for (_, child) in reaper.0.drain(..) {
            let output = child.wait_with_output().expect("join exclusive holder");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("RB03H_CHILD_OK:hold_exclusive"),
                "exclusive holder did not exit cleanly: {stdout}"
            );
        }

        let post_ex_lock = ConfigScopeLock::try_lock_exclusive_bounded(
            &scope_dir,
            DatabaseRole::GlobalConfig,
            Duration::ZERO,
        )
        .expect("LOCK_EX must succeed immediately after external holder releases");
        drop(post_ex_lock);

        // Regression (Codex R1 P1 #1): Returned `Arc<dyn ConfigStore>` handles
        // from `open_config_scope_read_only`, `open_config_scope_read_write`,
        // AND cloned `LockedConfigScope::store_arc()` (even after the guard
        // itself drops) MUST retain `LOCK_SH` for their entire lifetime,
        // keeping `try_lock_exclusive_bounded(..., Duration::ZERO)` blocked
        // with `WriteLockBusy` (`LBR-IO-001`) until the final `Arc` drops.
        {
            let ro_handle = open_config_scope_read_only(&scope_dir, DatabaseRole::GlobalConfig)
                .await
                .expect("open RO handle for lock-lifetime verification");
            let blocked_ro = ConfigScopeLock::try_lock_exclusive_bounded(
                &scope_dir,
                DatabaseRole::GlobalConfig,
                Duration::ZERO,
            )
            .expect_err("LOCK_EX must remain blocked while RO Arc<dyn ConfigStore> is alive");
            assert_eq!(blocked_ro.stable_code_str(), "LBR-IO-001");
            assert!(
                ro_handle
                    .get("scope.seed")
                    .await
                    .expect("read under held RO lock")
                    .is_some()
            );
            drop(ro_handle);
            drop(
                ConfigScopeLock::try_lock_exclusive_bounded(
                    &scope_dir,
                    DatabaseRole::GlobalConfig,
                    Duration::ZERO,
                )
                .expect("LOCK_EX must succeed immediately after RO handle drops"),
            );

            let rw_handle = open_config_scope_read_write(&scope_dir, DatabaseRole::GlobalConfig)
                .await
                .expect("open RW handle for lock-lifetime verification");
            let blocked_rw = ConfigScopeLock::try_lock_exclusive_bounded(
                &scope_dir,
                DatabaseRole::GlobalConfig,
                Duration::ZERO,
            )
            .expect_err("LOCK_EX must remain blocked while RW Arc<dyn ConfigStore> is alive");
            assert_eq!(blocked_rw.stable_code_str(), "LBR-IO-001");
            rw_handle
                .set("lock.lifetime", "held", false)
                .await
                .expect("write under held RW lock");
            drop(rw_handle);
            drop(
                ConfigScopeLock::try_lock_exclusive_bounded(
                    &scope_dir,
                    DatabaseRole::GlobalConfig,
                    Duration::ZERO,
                )
                .expect("LOCK_EX must succeed immediately after RW handle drops"),
            );

            let guard = open_locked_config_scope_read_only(
                &scope_dir,
                DatabaseRole::GlobalConfig,
                Duration::ZERO,
            )
            .await
            .expect("open locked guard for store_arc lifetime verification");
            let cloned_arc = guard.store_arc();
            drop(guard);
            let blocked_arc = ConfigScopeLock::try_lock_exclusive_bounded(
                &scope_dir,
                DatabaseRole::GlobalConfig,
                Duration::ZERO,
            )
            .expect_err("LOCK_EX must remain blocked while cloned store_arc() outlives guard");
            assert_eq!(blocked_arc.stable_code_str(), "LBR-IO-001");
            drop(cloned_arc);
            drop(
                ConfigScopeLock::try_lock_exclusive_bounded(
                    &scope_dir,
                    DatabaseRole::GlobalConfig,
                    Duration::ZERO,
                )
                .expect("LOCK_EX must succeed immediately after cloned store_arc() drops"),
            );
        }

        // Verify external `LOCK_SH` also blocks `try_lock_exclusive_bounded` (`LBR-IO-001`).
        let sh_child = Command::new(&exe)
            .arg("--exact")
            .arg("storage::config_store::tests::config_scope_bounded_lock_shared_concurrency_and_timeout_error")
            .arg("--nocapture")
            .env("LIBRA_RB03H_CHILD_MODE", "hold_shared")
            .env("LIBRA_RB03H_SCOPE_DIR", &scope_dir)
            .env("LIBRA_RB03H_SYNC_DIR", &sync_dir)
            .env("LIBRA_RB03H_ROLE", "global")
            .env("LIBRA_TEST", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn shared holder child");
        reaper.0.push(("hold_shared".to_string(), sh_child));

        let sh_marker = sync_dir.join("sh-held");
        let sh_wait = Instant::now() + CHILD_TIMEOUT;
        while !sh_marker.exists() {
            assert!(
                Instant::now() < sh_wait,
                "timed out waiting for external LOCK_SH holder"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let ex_blocked_by_sh = ConfigScopeLock::try_lock_exclusive_bounded(
            &scope_dir,
            DatabaseRole::GlobalConfig,
            Duration::ZERO,
        )
        .expect_err("LOCK_EX must fail closed with LBR-IO-001 while external LOCK_SH is held");
        assert_eq!(ex_blocked_by_sh.stable_code_str(), "LBR-IO-001");
        fs::write(sync_dir.join("release-sh"), b"go").expect("release shared holder");
        for (_, child) in reaper.0.drain(..) {
            let output = child.wait_with_output().expect("join shared holder");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("RB03H_CHILD_OK:hold_shared"),
                "shared holder did not exit cleanly: {stdout}"
            );
        }

        // Restore writable mode on tombstone before tempdir cleanup.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&sqlite_path, fs::Permissions::from_mode(0o600));
        }
        #[cfg(not(unix))]
        {
            if let Ok(meta) = fs::metadata(&sqlite_path) {
                let mut perms = meta.permissions();
                perms.set_readonly(false);
                let _ = fs::set_permissions(&sqlite_path, perms);
            }
        }
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn config_scope_readonly_absent_and_ro_system_dir_fallback() {
        use std::{fs, path::PathBuf};

        use tempfile::tempdir;

        use crate::storage::redb::RedbConfigStore;

        /// Ensures directory and file permissions are restored before `TempDir` drops.
        struct ModeRestoreGuard {
            dirs: Vec<PathBuf>,
            files: Vec<PathBuf>,
        }

        impl Drop for ModeRestoreGuard {
            fn drop(&mut self) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    for d in &self.dirs {
                        let _ = fs::set_permissions(d, fs::Permissions::from_mode(0o755));
                    }
                    for f in &self.files {
                        let _ = fs::set_permissions(f, fs::Permissions::from_mode(0o644));
                    }
                }
                #[cfg(not(unix))]
                {
                    for f in &self.files {
                        if let Ok(meta) = fs::metadata(f) {
                            let mut perms = meta.permissions();
                            perms.set_readonly(false);
                            let _ = fs::set_permissions(f, perms);
                        }
                    }
                }
            }
        }

        let root = tempdir().expect("create RB-03h RO system dir temp root");
        let mut restore = ModeRestoreGuard {
            dirs: Vec::new(),
            files: Vec::new(),
        };

        // 1. Absent and empty `SystemConfig` / `GlobalConfig` dirs return `NotFound`
        //    without creating any directory or `config-migration.lock` (`ADR-CF-06`).
        for (label, role) in [
            ("global", DatabaseRole::GlobalConfig),
            ("system", DatabaseRole::SystemConfig),
        ] {
            let missing_dir = root.path().join(format!("{label}-missing"));
            let err = open_locked_config_scope_read_only(&missing_dir, role, Duration::ZERO)
                .await
                .expect_err("missing scope dir must return NotFound");
            assert!(matches!(err, StorageBackendError::NotFound { .. }));
            assert!(!missing_dir.exists());
            assert!(!config_migration_lock_path(&missing_dir).exists());

            let empty_dir = root.path().join(format!("{label}-empty"));
            fs::create_dir_all(&empty_dir).expect("create empty scope dir");
            let err_empty = open_locked_config_scope_read_only(&empty_dir, role, Duration::ZERO)
                .await
                .expect_err("empty scope dir must return NotFound");
            assert!(matches!(err_empty, StorageBackendError::NotFound { .. }));
            assert!(!config_migration_lock_path(&empty_dir).exists());
        }

        // 2. AC4: Read-only (`0555`) `SystemConfig` directory WITHOUT
        //    `config-migration.lock` MUST degrade safely to direct read-only
        //    manifest inspection and NEVER attempt to create the lock file.
        let ro_sys_dir = root.path().join("ro-system-no-lock");
        fs::create_dir_all(&ro_sys_dir).expect("create ro system dir");
        let sys_redb = config_redb_path_in_scope_dir(&ro_sys_dir);
        let sys_sqlite = config_sqlite_path_in_scope_dir(&ro_sys_dir);

        let seed = RedbConfigStore::create(&sys_redb, DatabaseRole::SystemConfig)
            .expect("create SystemConfig config.redb");
        seed.set("core.editor", "nano-system-ro", false)
            .await
            .expect("seed SystemConfig core.editor");
        drop(seed);

        fs::write(&sys_sqlite, CONFIG_SQLITE_TOMBSTONE_SENTINEL)
            .expect("write system config.db tombstone");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &sys_sqlite,
                fs::Permissions::from_mode(SYSTEM_CONFIG_TOMBSTONE_MODE),
            )
            .expect("chmod 0444 system tombstone");
        }
        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(&sys_sqlite)
                .expect("stat tombstone")
                .permissions();
            perms.set_readonly(true);
            fs::set_permissions(&sys_sqlite, perms).expect("set readonly tombstone");
        }
        restore.files.push(sys_sqlite.clone());

        let sys_manifest =
            ConfigFormatManifest::new_redb_finalized(1, true, DatabaseRole::SystemConfig)
                .expect("build finalized system manifest");
        publish_config_format_manifest(&ro_sys_dir, DatabaseRole::SystemConfig, &sys_manifest)
            .expect("publish finalized system manifest");

        // Ensure `config-migration.lock` does NOT exist, then lock down directory to `0555`.
        let missing_lock = config_migration_lock_path(&ro_sys_dir);
        assert!(!missing_lock.exists());
        restore.dirs.push(ro_sys_dir.clone());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&ro_sys_dir, fs::Permissions::from_mode(0o555))
                .expect("chmod 0555 read-only system dir");
        }

        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(&ro_sys_dir)
                .expect("stat ro system dir")
                .permissions();
            perms.set_readonly(true);
            fs::set_permissions(&ro_sys_dir, perms).expect("set readonly on system dir");
        }

        let is_privileged_unix_root = {
            #[cfg(unix)]
            {
                // SAFETY: `geteuid` has no preconditions and always succeeds.
                unsafe { libc::geteuid() == 0 }
            }
            #[cfg(not(unix))]
            {
                false
            }
        };

        let locked_ro = open_locked_config_scope_read_only(
            &ro_sys_dir,
            DatabaseRole::SystemConfig,
            Duration::ZERO,
        )
        .await
        .expect("RO SystemConfig dir without config-migration.lock must open cleanly");
        if is_privileged_unix_root {
            assert!(
                locked_ro.is_locked(),
                "privileged root process with effective write access must still coordinate via LOCK_SH"
            );
            assert!(
                missing_lock.is_file(),
                "privileged root process must create config-migration.lock rather than bypassing coordination"
            );
        } else {
            assert!(
                !locked_ro.is_locked(),
                "non-root RO SystemConfig dir without lock file must use unlocked direct-manifest fallback"
            );
        }
        assert_eq!(locked_ro.role(), DatabaseRole::SystemConfig);
        assert_eq!(
            locked_ro
                .store()
                .get("core.editor")
                .await
                .expect("read core.editor from RO SystemConfig"),
            Some(ConfigEntry {
                key: "core.editor".to_string(),
                value: "nano-system-ro".to_string(),
                encrypted: false,
            })
        );
        drop(locked_ro);
        if !is_privileged_unix_root {
            assert!(
                !missing_lock.exists(),
                "AC4 violation: non-root RO SystemConfig lookup must NEVER create config-migration.lock"
            );
        }

        // Also verify direct `open_config_scope_read_only` works identically and creates zero lock file when non-root.
        let direct_ro = open_config_scope_read_only(&ro_sys_dir, DatabaseRole::SystemConfig)
            .await
            .expect("open_config_scope_read_only on RO SystemConfig dir must succeed");
        assert_eq!(
            direct_ro
                .get("core.editor")
                .await
                .expect("read via direct RO open")
                .map(|e| e.value),
            Some("nano-system-ro".to_string())
        );
        drop(direct_ro);
        if !is_privileged_unix_root {
            assert!(!missing_lock.exists());
        }

        // 3. When `config-migration.lock` (`0644`) DOES exist inside a `0555`
        //    read-only `SystemConfig` directory, read-only open MUST acquire
        //    `LOCK_SH` strictly via read-only fd (`is_locked() == true`), and
        //    still honor an external `LOCK_EX` with `LBR-IO-001`.
        let ro_sys_with_lock = root.path().join("ro-system-with-lock");
        fs::create_dir_all(&ro_sys_with_lock).expect("create ro system dir with lock");
        let sys2_redb = config_redb_path_in_scope_dir(&ro_sys_with_lock);
        let sys2_sqlite = config_sqlite_path_in_scope_dir(&ro_sys_with_lock);
        let seed2 = RedbConfigStore::create(&sys2_redb, DatabaseRole::SystemConfig)
            .expect("create second SystemConfig redb");
        seed2
            .set("core.pager", "cat", false)
            .await
            .expect("seed core.pager");
        drop(seed2);
        fs::write(&sys2_sqlite, CONFIG_SQLITE_TOMBSTONE_SENTINEL)
            .expect("write second system tombstone");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &sys2_sqlite,
                fs::Permissions::from_mode(SYSTEM_CONFIG_TOMBSTONE_MODE),
            )
            .expect("chmod 0444 second system tombstone");
        }
        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(&sys2_sqlite)
                .expect("stat tombstone")
                .permissions();
            perms.set_readonly(true);
            fs::set_permissions(&sys2_sqlite, perms).expect("set readonly tombstone");
        }
        restore.files.push(sys2_sqlite.clone());
        publish_config_format_manifest(
            &ro_sys_with_lock,
            DatabaseRole::SystemConfig,
            &sys_manifest,
        )
        .expect("publish second system manifest");

        // Pre-create `config-migration.lock` with SystemConfig mode `0644` before locking down dir.
        let init_lock = ConfigScopeLock::try_lock_exclusive_bounded(
            &ro_sys_with_lock,
            DatabaseRole::SystemConfig,
            Duration::ZERO,
        )
        .expect("create SystemConfig config-migration.lock");
        drop(init_lock);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let lock_mode = fs::symlink_metadata(config_migration_lock_path(&ro_sys_with_lock))
                .expect("stat system config-migration.lock")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                lock_mode, SYSTEM_CONFIG_FILE_MODE,
                "SystemConfig config-migration.lock must have exact mode 0644, got {lock_mode:04o}"
            );
        }

        restore.dirs.push(ro_sys_with_lock.clone());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&ro_sys_with_lock, fs::Permissions::from_mode(0o555))
                .expect("chmod 0555 second RO system dir");
        }

        let locked_sys2 = open_locked_config_scope_read_only(
            &ro_sys_with_lock,
            DatabaseRole::SystemConfig,
            Duration::ZERO,
        )
        .await
        .expect("RO SystemConfig dir with existing 0644 lock file must acquire LOCK_SH");
        assert!(
            locked_sys2.is_locked(),
            "RO SystemConfig dir with existing lock file must hold LOCK_SH"
        );
        assert_eq!(
            locked_sys2
                .store()
                .get("core.pager")
                .await
                .expect("read core.pager")
                .map(|e| e.value),
            Some("cat".to_string())
        );
        drop(locked_sys2);

        // 4. Regression (Codex R1 P1 #2): In a WRITABLE `SystemConfig` directory
        //    where `config-migration.lock` is initially absent, read-only open
        //    MUST NOT bypass locking — it MUST create `0644` `config-migration.lock`,
        //    acquire `LOCK_SH`, and block a concurrent `LOCK_EX` migration until
        //    the returned store handle drops.
        let writable_sys_dir = root.path().join("writable-system-no-lock");
        fs::create_dir_all(&writable_sys_dir).expect("create writable system dir");
        let wsys_redb = config_redb_path_in_scope_dir(&writable_sys_dir);
        let wsys_sqlite = config_sqlite_path_in_scope_dir(&writable_sys_dir);
        let seed_w = RedbConfigStore::create(&wsys_redb, DatabaseRole::SystemConfig)
            .expect("create writable SystemConfig redb");
        seed_w
            .set("core.autocrlf", "false", false)
            .await
            .expect("seed writable SystemConfig");
        drop(seed_w);
        fs::write(&wsys_sqlite, CONFIG_SQLITE_TOMBSTONE_SENTINEL)
            .expect("write writable system tombstone");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &wsys_sqlite,
                fs::Permissions::from_mode(SYSTEM_CONFIG_TOMBSTONE_MODE),
            )
            .expect("chmod 0444 writable system tombstone");
        }
        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(&wsys_sqlite)
                .expect("stat tombstone")
                .permissions();
            perms.set_readonly(true);
            fs::set_permissions(&wsys_sqlite, perms).expect("set readonly tombstone");
        }
        restore.files.push(wsys_sqlite);
        publish_config_format_manifest(
            &writable_sys_dir,
            DatabaseRole::SystemConfig,
            &sys_manifest,
        )
        .expect("publish writable system manifest");
        let wsys_lock_path = config_migration_lock_path(&writable_sys_dir);
        assert!(!wsys_lock_path.exists());

        let locked_writable_sys = open_locked_config_scope_read_only(
            &writable_sys_dir,
            DatabaseRole::SystemConfig,
            Duration::ZERO,
        )
        .await
        .expect("writable SystemConfig RO open must create and lock config-migration.lock");
        assert!(
            locked_writable_sys.is_locked(),
            "writable SystemConfig RO open must hold LOCK_SH rather than unlocked fallback"
        );
        assert!(wsys_lock_path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::symlink_metadata(&wsys_lock_path)
                .expect("stat created system lock")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, SYSTEM_CONFIG_FILE_MODE);
        }
        // Concurrent migration (`LOCK_EX`) must be blocked while writable system reader is live.
        let mig_blocked = ConfigScopeLock::try_lock_exclusive_bounded(
            &writable_sys_dir,
            DatabaseRole::SystemConfig,
            Duration::ZERO,
        )
        .expect_err("concurrent migration LOCK_EX must be blocked while writable SystemConfig reader holds LOCK_SH");
        assert_eq!(mig_blocked.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            locked_writable_sys
                .store()
                .get("core.autocrlf")
                .await
                .expect("read writable system key")
                .map(|e| e.value),
            Some("false".to_string())
        );
        drop(locked_writable_sys);
        drop(
            ConfigScopeLock::try_lock_exclusive_bounded(
                &writable_sys_dir,
                DatabaseRole::SystemConfig,
                Duration::ZERO,
            )
            .expect("migration LOCK_EX must succeed once writable SystemConfig reader drops"),
        );

        // Verify symlink at `config-migration.lock` fails closed (`LBR-IO-001`) and
        // never silently degrades to unlocked fallback.
        #[cfg(unix)]
        {
            let sym_dir = root.path().join("system-symlink-lock");
            fs::create_dir_all(&sym_dir).expect("create symlink lock dir");
            let sym_redb = config_redb_path_in_scope_dir(&sym_dir);
            let sym_sqlite = config_sqlite_path_in_scope_dir(&sym_dir);
            RedbConfigStore::create(&sym_redb, DatabaseRole::SystemConfig)
                .expect("create sym redb");
            fs::write(&sym_sqlite, CONFIG_SQLITE_TOMBSTONE_SENTINEL).expect("write sym tomb");
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &sym_sqlite,
                    fs::Permissions::from_mode(SYSTEM_CONFIG_TOMBSTONE_MODE),
                )
                .expect("chmod sym tomb");
            }
            restore.files.push(sym_sqlite);
            publish_config_format_manifest(&sym_dir, DatabaseRole::SystemConfig, &sys_manifest)
                .expect("publish sym manifest");
            std::os::unix::fs::symlink(&sym_redb, config_migration_lock_path(&sym_dir))
                .expect("create symlink config-migration.lock");
            let sym_err = open_locked_config_scope_read_only(
                &sym_dir,
                DatabaseRole::SystemConfig,
                Duration::ZERO,
            )
            .await
            .expect_err("symlink config-migration.lock must fail closed");
            assert_eq!(sym_err.stable_code_str(), "LBR-IO-001");

            // Regression (Codex R2 P1): an existing unreadable (`0000`) lock file
            // inside a writable directory must FAIL CLOSED (`LBR-IO-001`) instead
            // of degrading to unlocked fallback (when running as non-root).
            // SAFETY: `geteuid` has no preconditions and always succeeds.
            if unsafe { libc::geteuid() } != 0 {
                let unreadable_dir = root.path().join("system-unreadable-lock");
                fs::create_dir_all(&unreadable_dir).expect("create unreadable lock dir");
                let ur_redb = config_redb_path_in_scope_dir(&unreadable_dir);
                let ur_sqlite = config_sqlite_path_in_scope_dir(&unreadable_dir);
                RedbConfigStore::create(&ur_redb, DatabaseRole::SystemConfig)
                    .expect("create ur redb");
                fs::write(&ur_sqlite, CONFIG_SQLITE_TOMBSTONE_SENTINEL).expect("write ur tomb");
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(
                        &ur_sqlite,
                        fs::Permissions::from_mode(SYSTEM_CONFIG_TOMBSTONE_MODE),
                    )
                    .expect("chmod ur tomb");
                }
                restore.files.push(ur_sqlite);
                publish_config_format_manifest(
                    &unreadable_dir,
                    DatabaseRole::SystemConfig,
                    &sys_manifest,
                )
                .expect("publish ur manifest");
                let ur_lock = config_migration_lock_path(&unreadable_dir);
                fs::write(&ur_lock, b"").expect("create unreadable lock file");
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&ur_lock, fs::Permissions::from_mode(0o000))
                        .expect("chmod 0000 lock file");
                }
                restore.files.push(ur_lock);
                let ur_err = open_locked_config_scope_read_only(
                    &unreadable_dir,
                    DatabaseRole::SystemConfig,
                    Duration::ZERO,
                )
                .await
                .expect_err("existing unreadable lock file in writable dir must fail closed");
                assert_eq!(ur_err.stable_code_str(), "LBR-IO-001");

                // Regression (Codex R3 P1 #2): read-only `0555` `GlobalConfig`
                // directory without `config-migration.lock` MUST fail closed
                // (`LBR-IO-001`) instead of using the `SystemConfig`-only fallback.
                let ro_global_dir = root.path().join("ro-global-no-lock");
                fs::create_dir_all(&ro_global_dir).expect("create ro global dir");
                let rg_redb = config_redb_path_in_scope_dir(&ro_global_dir);
                let rg_sqlite = config_sqlite_path_in_scope_dir(&ro_global_dir);
                RedbConfigStore::create(&rg_redb, DatabaseRole::GlobalConfig)
                    .expect("create rg redb");
                fs::write(&rg_sqlite, CONFIG_SQLITE_TOMBSTONE_SENTINEL).expect("write rg tomb");
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(
                        &rg_sqlite,
                        fs::Permissions::from_mode(GLOBAL_CONFIG_TOMBSTONE_MODE),
                    )
                    .expect("chmod rg tomb");
                }
                restore.files.push(rg_sqlite);
                let global_manifest =
                    ConfigFormatManifest::new_redb_finalized(1, true, DatabaseRole::GlobalConfig)
                        .expect("build global manifest");
                publish_config_format_manifest(
                    &ro_global_dir,
                    DatabaseRole::GlobalConfig,
                    &global_manifest,
                )
                .expect("publish rg manifest");
                restore.dirs.push(ro_global_dir.clone());
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&ro_global_dir, fs::Permissions::from_mode(0o555))
                        .expect("chmod 0555 ro global dir");
                }
                let ro_global_err = open_locked_config_scope_read_only(
                    &ro_global_dir,
                    DatabaseRole::GlobalConfig,
                    Duration::ZERO,
                )
                .await
                .expect_err("RO GlobalConfig dir without lock file must fail closed");
                assert_eq!(ro_global_err.stable_code_str(), "LBR-IO-001");
                assert!(!config_migration_lock_path(&ro_global_dir).exists());
            }
        }
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn global_config_hardlink_reserve_and_unsupported_fs_reject() {
        use std::{
            fs,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };

        use tempfile::tempdir;

        let root = tempdir().expect("create RB-03d AC1/AC2 temp root");

        // -----------------------------------------------------------------
        // AC1: Fresh `GlobalConfig` `Redb` creation reserves `0400` 31-byte
        // `LIBRA_CONFIG_AUTHORITY_IS_REDB\n` at `config.db` via non-overwriting
        // `hard_link` strictly BEFORE `config.redb` is created (never 0-byte).
        // -----------------------------------------------------------------
        let global_dir = root.path().join("global-ac1");
        fs::create_dir_all(&global_dir).expect("create global-ac1 dir");
        let sqlite_path = config_sqlite_path_in_scope_dir(&global_dir);
        let redb_path = config_redb_path_in_scope_dir(&global_dir);

        let hook_verified = Arc::new(AtomicBool::new(false));
        let hook_flag = Arc::clone(&hook_verified);

        let locked = open_or_create_locked_config_scope_for_write_with_hooks(
            &global_dir,
            DatabaseRole::GlobalConfig,
            StorageBackendKind::Redb,
            Duration::from_secs(5),
            || {},
            move |dir| {
                // Inside Step F1 -> Step F2 boundary (BEFORE `config.redb` exists):
                let db_p = config_sqlite_path_in_scope_dir(dir);
                let redb_p = config_redb_path_in_scope_dir(dir);
                assert!(
                    !redb_p.exists(),
                    "AC1 violation: config.redb must NOT exist before Step F1 tombstone reservation"
                );
                let bytes = fs::read(&db_p).expect("config.db sentinel must already exist");
                assert_eq!(
                    bytes.as_slice(),
                    CONFIG_SQLITE_TOMBSTONE_SENTINEL,
                    "AC1 violation: config.db must already contain exact 31-byte sentinel (never 0 bytes)"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = fs::symlink_metadata(&db_p)
                        .expect("stat reserved config.db")
                        .permissions()
                        .mode()
                        & 0o777;
                    assert_eq!(
                        mode, GLOBAL_CONFIG_TOMBSTONE_MODE,
                        "AC1 violation: GlobalConfig config.db tombstone must be exact 0400 before config.redb creation, got {mode:04o}"
                    );
                }
                // Confirm no leftover `config.db.tombstone.*.tmp` remains after `hard_link`.
                let leftover_tmps: Vec<_> = fs::read_dir(dir)
                    .expect("read scope dir")
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with("config.db.tombstone.")
                    })
                    .collect();
                assert!(
                    leftover_tmps.is_empty(),
                    "tombstone temp alias must be unlinked immediately after hard_link: {leftover_tmps:?}"
                );
                hook_flag.store(true, Ordering::SeqCst);
            },
            || {},
        )
        .await
        .expect("fresh GlobalConfig Redb creation must succeed");

        assert!(
            hook_verified.load(Ordering::SeqCst),
            "Step F1 pre-redb verification hook must have executed"
        );
        locked
            .store()
            .set("user.name", "Global Redb First", false)
            .await
            .expect("write initial GlobalConfig key");
        drop(locked);

        // Verify final on-disk state: `0600` `config.redb`, `0400` 31B sentinel `config.db`,
        // finalized `config-format.json(backend="redb", finalized=true, legacy_sqlite_tombstone=true)`.
        assert!(redb_path.is_file());
        assert_eq!(
            fs::read(&sqlite_path).expect("read final config.db tombstone"),
            CONFIG_SQLITE_TOMBSTONE_SENTINEL
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::symlink_metadata(&redb_path)
                    .expect("stat config.redb")
                    .permissions()
                    .mode()
                    & 0o777,
                GLOBAL_CONFIG_FILE_MODE
            );
            assert_eq!(
                fs::symlink_metadata(&sqlite_path)
                    .expect("stat config.db")
                    .permissions()
                    .mode()
                    & 0o777,
                GLOBAL_CONFIG_TOMBSTONE_MODE
            );
        }
        let manifest = read_config_format_manifest(&global_dir, DatabaseRole::GlobalConfig)
            .expect("read finalized manifest")
            .expect("manifest must exist");
        assert_eq!(manifest.backend, StorageBackendKind::Redb);
        assert!(manifest.finalized);
        assert!(manifest.init_phase.is_none());
        assert!(manifest.legacy_sqlite_tombstone);
        assert!(!manifest.targets_staged_for_rename);

        let verify_ro = open_config_scope_read_only(&global_dir, DatabaseRole::GlobalConfig)
            .await
            .expect("reopen GlobalConfig read-only");
        assert_eq!(
            verify_ro
                .get("user.name")
                .await
                .expect("read user.name")
                .map(|e| e.value),
            Some("Global Redb First".to_string())
        );
        drop(verify_ro);

        // Clean up readonly bit before tempdir drop.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&sqlite_path, fs::Permissions::from_mode(0o600));
        }

        // Regression (Codex R1 P1): Cancelling/dropping the async future while
        // `spawn_blocking` is executing Step F1..F4 MUST NOT release `LOCK_EX`
        // until the blocking creation task completes.
        {
            use std::sync::mpsc;

            let cancel_dir = root.path().join("global-ac1-cancel-lock");
            fs::create_dir_all(&cancel_dir).expect("create cancel-lock dir");
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = mpsc::channel();

            let cancel_dir_task = cancel_dir.clone();
            let task = tokio::spawn(async move {
                open_or_create_locked_config_scope_for_write_with_hooks(
                    &cancel_dir_task,
                    DatabaseRole::GlobalConfig,
                    StorageBackendKind::Redb,
                    Duration::from_secs(5),
                    || {},
                    move |_| {
                        let _ = entered_tx.send(());
                        let _ = release_rx.recv_timeout(Duration::from_secs(10));
                    },
                    || {},
                )
                .await
            });

            tokio::time::timeout(Duration::from_secs(5), entered_rx)
                .await
                .expect("wait for blocking creation to pause after Step F1")
                .expect("entered channel closed unexpectedly");
            // Abort/drop the async caller while blocking creation is still in flight.
            task.abort();
            let _ = task.await;

            // `LOCK_EX` MUST STILL BE HELD by the blocking task!
            let still_locked = ConfigScopeLock::try_lock_exclusive_bounded(
                &cancel_dir,
                DatabaseRole::GlobalConfig,
                Duration::ZERO,
            )
            .expect_err("LOCK_EX must remain held after async cancellation while creation runs");
            assert_eq!(still_locked.stable_code_str(), "LBR-IO-001");

            // Release the blocking task and confirm `LOCK_EX` becomes available once it finishes.
            let _ = release_tx.send(());
            let post_cancel_lock = ConfigScopeLock::try_lock_exclusive_bounded(
                &cancel_dir,
                DatabaseRole::GlobalConfig,
                Duration::from_secs(5),
            )
            .expect("LOCK_EX must release once cancelled creation's blocking task exits");
            drop(post_cancel_lock);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(
                    config_sqlite_path_in_scope_dir(&cancel_dir),
                    fs::Permissions::from_mode(0o600),
                );
            }

            // Regression (Codex R2 P1): Cancelling the async caller during
            // fresh SQLite initialization + manifest publication MUST also keep
            // `LOCK_EX` held until BOTH `config.db` and finalized
            // `config-format.json(backend="sqlite", finalized=true)` complete.
            let sqlite_cancel_dir = root.path().join("global-ac1-cancel-sqlite-lock");
            fs::create_dir_all(&sqlite_cancel_dir).expect("create sqlite cancel dir");
            let (sq_entered_tx, sq_entered_rx) = tokio::sync::oneshot::channel();
            let (sq_release_tx, sq_release_rx) = mpsc::channel();
            let sq_dir_task = sqlite_cancel_dir.clone();
            let sq_task = tokio::spawn(async move {
                open_or_create_locked_config_scope_for_write_with_hooks(
                    &sq_dir_task,
                    DatabaseRole::GlobalConfig,
                    StorageBackendKind::Sqlite,
                    Duration::from_secs(5),
                    || {},
                    |_| {},
                    move || {
                        let _ = sq_entered_tx.send(());
                        let _ = sq_release_rx.recv_timeout(Duration::from_secs(10));
                    },
                )
                .await
            });
            tokio::time::timeout(Duration::from_secs(5), sq_entered_rx)
                .await
                .expect("wait for SQLite creation to reach pre-manifest hook")
                .expect("sqlite entered channel closed unexpectedly");
            sq_task.abort();
            let _ = sq_task.await;

            // `LOCK_EX` MUST STILL BE HELD while SQLite manifest publication finishes!
            let sq_still_locked = ConfigScopeLock::try_lock_exclusive_bounded(
                &sqlite_cancel_dir,
                DatabaseRole::GlobalConfig,
                Duration::ZERO,
            )
            .expect_err("LOCK_EX must remain held after async cancellation during SQLite creation");
            assert_eq!(sq_still_locked.stable_code_str(), "LBR-IO-001");

            let _ = sq_release_tx.send(());
            let sq_dir_post = sqlite_cancel_dir.clone();
            let post_sq_lock = tokio::task::spawn_blocking(move || {
                ConfigScopeLock::try_lock_exclusive_bounded(
                    &sq_dir_post,
                    DatabaseRole::GlobalConfig,
                    Duration::from_secs(5),
                )
            })
            .await
            .expect("join post-cancel SQLite lock check")
            .expect("LOCK_EX must release once SQLite creation finishes");
            drop(post_sq_lock);

            let sq_manifest =
                read_config_format_manifest(&sqlite_cancel_dir, DatabaseRole::GlobalConfig)
                    .expect("read SQLite manifest after cancelled caller")
                    .expect("finalized SQLite manifest must have been published under LOCK_EX");
            assert_eq!(sq_manifest.backend, StorageBackendKind::Sqlite);
            assert!(sq_manifest.finalized);
            assert!(!sq_manifest.legacy_sqlite_tombstone);
            assert!(
                config_sqlite_path_in_scope_dir(&sqlite_cancel_dir).is_file(),
                "SQLite config.db must have been fully initialized under LOCK_EX"
            );
        }

        // -----------------------------------------------------------------
        // AC2: When underlying filesystem rejects `hard_link` with
        // `Unsupported`, `ENOSYS`, or `EXDEV`, `.tmp` MUST be cleaned up,
        // `config.db` MUST NOT be created via fallback `rename`, and error
        // MUST fail closed with `LBR-IO-001`.
        // -----------------------------------------------------------------
        struct FailingHardLinkOps {
            make_err: fn() -> io::Error,
        }
        impl TombstoneHardLinkOps for FailingHardLinkOps {
            fn hard_link(&self, _src: &Path, _dst: &Path) -> io::Result<()> {
                Err((self.make_err)())
            }
        }

        type MakeIoErr = fn() -> io::Error;
        let error_cases: [(&str, MakeIoErr); 3] = [
            ("unsupported", || {
                io::Error::new(io::ErrorKind::Unsupported, "hard links unsupported")
            }),
            ("enosys", || {
                #[cfg(unix)]
                {
                    io::Error::from_raw_os_error(libc::ENOSYS)
                }
                #[cfg(not(unix))]
                {
                    io::Error::new(io::ErrorKind::Unsupported, "ENOSYS")
                }
            }),
            ("exdev", || {
                #[cfg(unix)]
                {
                    io::Error::from_raw_os_error(libc::EXDEV)
                }
                #[cfg(not(unix))]
                {
                    io::Error::new(io::ErrorKind::CrossesDevices, "EXDEV")
                }
            }),
        ];

        for (label, make_err) in error_cases {
            let unsup_dir = root.path().join(format!("global-ac2-{label}"));
            fs::create_dir_all(&unsup_dir).expect("create ac2 dir");
            let ops = FailingHardLinkOps { make_err };
            let err = reserve_config_sqlite_tombstone_hard_link_with_ops(
                &unsup_dir,
                DatabaseRole::GlobalConfig,
                &ops,
            )
            .expect_err("unsupported hard_link must fail closed");
            assert_eq!(
                err.stable_code_str(),
                "LBR-IO-001",
                "{label}: expected LBR-IO-001, got {err:?}"
            );
            let remaining: Vec<_> = fs::read_dir(&unsup_dir)
                .expect("read ac2 dir")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect ac2 dir entries");
            assert!(
                remaining.is_empty(),
                "{label}: failed hard_link must leave zero .tmp or config.db files behind (never downgrade to rename): {remaining:?}"
            );
        }
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn global_config_concurrent_first_write_serializes_authority() {
        use std::{
            env, fs,
            path::PathBuf,
            process::{Child, Command, Stdio},
        };

        use tempfile::tempdir;

        const CHILD_TIMEOUT: Duration = Duration::from_secs(20);

        struct ChildReaper(Vec<(String, Child)>);
        impl Drop for ChildReaper {
            fn drop(&mut self) {
                for (_, mut child) in self.0.drain(..) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }

        // Child worker mode: wait on strict file barrier, then race `Sqlite` vs `Redb`
        // first write against the exact same empty `LIBRA_CONFIG_GLOBAL_DB` directory.
        if let Some(backend_str) = env::var_os("LIBRA_RB03D_RACE_BACKEND") {
            let global_db_env = PathBuf::from(
                env::var_os(LIBRA_CONFIG_GLOBAL_DB_ENV).expect("LIBRA_CONFIG_GLOBAL_DB env"),
            );
            let scope_dir =
                config_scope_dir_from_db_path(&global_db_env).expect("derive global scope dir");
            let sync_dir =
                PathBuf::from(env::var_os("LIBRA_RB03D_SYNC_DIR").expect("sync dir env"));
            let backend = match backend_str.to_string_lossy().as_ref() {
                "sqlite" => StorageBackendKind::Sqlite,
                "redb" => StorageBackendKind::Redb,
                other => panic!("unexpected LIBRA_RB03D_RACE_BACKEND: {other}"),
            };

            fs::write(sync_dir.join(format!("ready-{backend}")), b"1").expect("signal ready");
            let go_file = sync_dir.join("go");
            let wait_deadline = Instant::now() + CHILD_TIMEOUT;
            while !go_file.exists() {
                assert!(
                    Instant::now() < wait_deadline,
                    "race child `{backend}` timed out waiting for go barrier"
                );
                std::thread::sleep(Duration::from_millis(5));
            }

            match open_or_create_locked_config_scope_for_write(
                &scope_dir,
                DatabaseRole::GlobalConfig,
                backend,
                Duration::from_secs(10),
            )
            .await
            {
                Ok(locked) => {
                    locked
                        .store()
                        .set("race.winner", &backend.to_string(), false)
                        .await
                        .expect("winner write must succeed");
                    drop(locked);
                    println!("RB03D_RACE_RESULT:{backend}:WINNER");
                }
                Err(err) => {
                    assert!(
                        matches!(err, StorageBackendError::BackendMismatch { .. }),
                        "loser `{backend}` must receive BackendMismatch, got {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-CLI-003");
                    println!("RB03D_RACE_RESULT:{backend}:LOSER_CLI_003");
                }
            }
            return;
        }

        let exe = env::current_exe().expect("locate current test executable");
        let root = tempdir().expect("create RB-03d AC3 race temp root");

        // Run 4 real barrier-synchronized OS-process races between `Sqlite` and `Redb`.
        for round in 0..4 {
            let scope_dir = root.path().join(format!("race-round-{round}"));
            let sync_dir = root.path().join(format!("sync-round-{round}"));
            fs::create_dir_all(&scope_dir).expect("create empty global scope dir");
            fs::create_dir_all(&sync_dir).expect("create sync dir");
            let global_db_env = config_sqlite_path_in_scope_dir(&scope_dir);

            let mut reaper = ChildReaper(Vec::new());
            // Alternate spawn order across rounds to exercise both winner polarities.
            let order = if round % 2 == 0 {
                ["sqlite", "redb"]
            } else {
                ["redb", "sqlite"]
            };
            for backend in order {
                let child = Command::new(&exe)
                    .arg("--exact")
                    .arg("storage::config_store::tests::global_config_concurrent_first_write_serializes_authority")
                    .arg("--nocapture")
                    .env("LIBRA_RB03D_RACE_BACKEND", backend)
                    .env(LIBRA_CONFIG_GLOBAL_DB_ENV, &global_db_env)
                    .env("LIBRA_RB03D_SYNC_DIR", &sync_dir)
                    .env("LIBRA_TEST", "1")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("spawn race child");
                reaper.0.push((backend.to_string(), child));
            }

            // Wait until BOTH OS processes are parked at the barrier, then release simultaneously.
            let barrier_deadline = Instant::now() + CHILD_TIMEOUT;
            for backend in ["sqlite", "redb"] {
                let ready = sync_dir.join(format!("ready-{backend}"));
                while !ready.exists() {
                    assert!(
                        Instant::now() < barrier_deadline,
                        "round {round}: timed out waiting for `{backend}` ready marker"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            fs::write(sync_dir.join("go"), b"go").expect("release race barrier");

            let mut winners = Vec::new();
            let mut losers = Vec::new();
            for (backend, child) in reaper.0.drain(..) {
                let output = child.wait_with_output().expect("join race child");
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    output.status.success() && stdout.contains("1 passed; 0 failed"),
                    "round {round} `{backend}` child failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
                );
                if stdout.contains(&format!("RB03D_RACE_RESULT:{backend}:WINNER")) {
                    winners.push(backend);
                } else if stdout.contains(&format!("RB03D_RACE_RESULT:{backend}:LOSER_CLI_003")) {
                    losers.push(backend);
                } else {
                    panic!(
                        "round {round} `{backend}` missing race result marker:\nstdout:\n{stdout}"
                    );
                }
            }

            assert_eq!(
                winners.len(),
                1,
                "round {round}: exactly ONE writer must establish single authority, winners={winners:?}"
            );
            assert_eq!(
                losers.len(),
                1,
                "round {round}: exactly ONE writer must fail closed LBR-CLI-003, losers={losers:?}"
            );

            let winner = winners[0].as_str();
            let manifest = read_config_format_manifest(&scope_dir, DatabaseRole::GlobalConfig)
                .expect("read post-race manifest")
                .expect("finalized manifest must exist after race");
            assert!(manifest.finalized);
            assert!(manifest.init_phase.is_none());

            let ro = open_config_scope_read_only(&scope_dir, DatabaseRole::GlobalConfig)
                .await
                .expect("post-race read-only open must succeed");
            assert_eq!(
                ro.get("race.winner")
                    .await
                    .expect("read race.winner")
                    .map(|e| e.value),
                Some(winner.to_string())
            );
            drop(ro);

            let sqlite_path = config_sqlite_path_in_scope_dir(&scope_dir);
            let redb_path = config_redb_path_in_scope_dir(&scope_dir);
            match winner {
                "sqlite" => {
                    assert_eq!(manifest.backend, StorageBackendKind::Sqlite);
                    assert!(!manifest.legacy_sqlite_tombstone);
                    assert!(sqlite_path.is_file());
                    assert!(
                        !redb_path.exists(),
                        "round {round}: losing redb writer must NEVER create config.redb"
                    );
                }
                "redb" => {
                    assert_eq!(manifest.backend, StorageBackendKind::Redb);
                    assert!(manifest.legacy_sqlite_tombstone);
                    assert!(redb_path.is_file());
                    assert_eq!(
                        fs::read(&sqlite_path).expect("read winner redb tombstone"),
                        CONFIG_SQLITE_TOMBSTONE_SENTINEL
                    );
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ =
                            fs::set_permissions(&sqlite_path, fs::Permissions::from_mode(0o600));
                    }
                }
                other => panic!("unexpected winner `{other}`"),
            }
        }
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn global_config_hardlink_yields_to_legacy_writer_and_blocks_post_reserve() {
        use std::fs;

        use tempfile::tempdir;

        use crate::{
            internal::{
                config::ConfigKv,
                db::schema::{create_configuration_database, open_configuration_database},
            },
            storage::sqlite::SqliteConfigStore,
        };

        let root = tempdir().expect("create RB-03d AC4/AC5 temp root");

        // -----------------------------------------------------------------
        // AC4 (Part A — mid-flight TOCTOU race): `Redb` acquires `LOCK_EX` on
        // an empty `GlobalConfig` dir, then right BEFORE Step F1 `hard_link`,
        // an un-locked legacy binary creates `config.db` via `create_new` and
        // writes a key. Step F1 `hard_link` MUST hit `AlreadyExists`, clean up
        // `.tmp`, create ZERO `config.redb` / `config-format.json`, return
        // `LBR-CLI-003`, and preserve 100% of the legacy write!
        // -----------------------------------------------------------------
        let midflight_dir = root.path().join("global-ac4-midflight");
        fs::create_dir_all(&midflight_dir).expect("create ac4 midflight dir");
        let mid_sqlite = config_sqlite_path_in_scope_dir(&midflight_dir);
        let mid_redb = config_redb_path_in_scope_dir(&midflight_dir);
        let mid_sqlite_for_hook = mid_sqlite.clone();

        let mid_err = open_or_create_locked_config_scope_for_write_with_hooks(
            &midflight_dir,
            DatabaseRole::GlobalConfig,
            StorageBackendKind::Redb,
            Duration::from_secs(5),
            move || {
                // Simulate un-locked legacy binary winning via `schema.rs:800`
                // `create_configuration_database` (`OpenOptions::create_new(true)`).
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build legacy writer runtime");
                rt.block_on(async {
                    let conn = create_configuration_database(
                        &mid_sqlite_for_hook,
                        DatabaseRole::GlobalConfig,
                    )
                    .await
                    .expect("legacy create_configuration_database must succeed");
                    ConfigKv::set_with_conn(&conn, "user.legacy", "legacy-won-midflight", false)
                        .await
                        .expect("legacy write must succeed");
                    conn.close().await.expect("close legacy connection");
                });
            },
            |_| panic!("after_tombstone hook must NOT run when hard_link hits AlreadyExists"),
            || {},
        )
        .await
        .expect_err("Redb first write must fail closed when legacy binary wins hard_link race");

        assert!(
            matches!(mid_err, StorageBackendError::BackendMismatch { .. }),
            "expected BackendMismatch on legacy win, got {mid_err:?}"
        );
        assert_eq!(mid_err.stable_code_str(), "LBR-CLI-003");
        assert!(
            !mid_redb.exists(),
            "AC4 violation: config.redb must NEVER be created when legacy writer wins"
        );
        assert!(
            !config_format_manifest_path(&midflight_dir).exists(),
            "AC4 violation: config-format.json must NOT be published when legacy writer wins"
        );
        // Zero leftover tombstone `.tmp` files.
        let leftover_tmps: Vec<_> = fs::read_dir(&midflight_dir)
            .expect("read midflight dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("config.db.tombstone.")
            })
            .collect();
        assert!(leftover_tmps.is_empty());

        // Verify legacy write is 100% preserved!
        let legacy_verify =
            SqliteConfigStore::open_read_only(&mid_sqlite, DatabaseRole::GlobalConfig)
                .await
                .expect("legacy config.db must remain 100% intact and readable");
        assert_eq!(
            legacy_verify
                .get("user.legacy")
                .await
                .expect("read legacy key")
                .map(|e| e.value),
            Some("legacy-won-midflight".to_string())
        );
        drop(legacy_verify);

        // AC4 (Part B — legacy writer already finished before Redb even starts):
        let preexist_err = open_or_create_locked_config_scope_for_write(
            &midflight_dir,
            DatabaseRole::GlobalConfig,
            StorageBackendKind::Redb,
            Duration::from_secs(5),
        )
        .await
        .expect_err("Redb write against pre-existing legacy config.db must fail closed");
        assert_eq!(preexist_err.stable_code_str(), "LBR-CLI-003");
        assert!(!mid_redb.exists());

        // -----------------------------------------------------------------
        // AC5: Post-reserve legacy binary blocked (`SQLITE_NOTADB`):
        // `Redb` creates `GlobalConfig` first (reserving `0400` 31-byte
        // `LIBRA_CONFIG_AUTHORITY_IS_REDB\n` at `config.db`), then legacy
        // `open_configuration_database` (`src/command/config.rs:262`) AND
        // `create_configuration_database` (`schema.rs:800`) both fail closed!
        // -----------------------------------------------------------------
        let post_dir = root.path().join("global-ac5-post-reserve");
        fs::create_dir_all(&post_dir).expect("create ac5 dir");
        let post_sqlite = config_sqlite_path_in_scope_dir(&post_dir);

        let locked_redb = open_or_create_locked_config_scope_for_write(
            &post_dir,
            DatabaseRole::GlobalConfig,
            StorageBackendKind::Redb,
            Duration::from_secs(5),
        )
        .await
        .expect("Redb creates GlobalConfig scope first");
        locked_redb
            .store()
            .set("user.email", "redb-owner@example.com", false)
            .await
            .expect("write Redb global key");
        drop(locked_redb);

        // Legacy `libra config get/set --global` path (`config.rs:262` -> `open_configuration_database`).
        let legacy_open_err = open_configuration_database(&post_sqlite, DatabaseRole::GlobalConfig)
            .await
            .expect_err("legacy open_configuration_database must fail on 0400 sentinel tombstone");
        let err_msg = legacy_open_err.to_string().to_ascii_lowercase();
        assert!(
            err_msg.contains("not a database")
                || err_msg.contains("sqlite_notadb")
                || err_msg.contains("code: 26"),
            "AC5 violation: expected SQLITE_NOTADB / 'file is not a database' from legacy open, got: {legacy_open_err}"
        );

        // Legacy `create_configuration_database` (`schema.rs:800` `create_new(true)`) must hit `AlreadyExists`.
        let legacy_create_err =
            create_configuration_database(&post_sqlite, DatabaseRole::GlobalConfig)
                .await
                .expect_err("legacy create_configuration_database must fail with AlreadyExists");
        assert_eq!(legacy_create_err.kind(), io::ErrorKind::AlreadyExists);

        // Sentinel bytes and Redb config values remain 100% untouched.
        assert_eq!(
            fs::read(&post_sqlite).expect("read sentinel after legacy open attempt"),
            CONFIG_SQLITE_TOMBSTONE_SENTINEL
        );
        let redb_verify = open_config_scope_read_only(&post_dir, DatabaseRole::GlobalConfig)
            .await
            .expect("reopen Redb GlobalConfig");
        assert_eq!(
            redb_verify
                .get("user.email")
                .await
                .expect("read user.email")
                .map(|e| e.value),
            Some("redb-owner@example.com".to_string())
        );
        drop(redb_verify);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&post_sqlite, fs::Permissions::from_mode(0o600));
        }
    }
}
