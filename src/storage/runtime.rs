//! Runtime backend dispatch and store-opener helpers for Track B integration
//! (`RB-13`..`RB-30`).
//!
//! Centralizes repository backend detection, read-only vs read-write store
//! opening (`ADR-RB-03`), and atomic fresh `redb` repository initialization
//! (`git.gen-00000001.redb` + `capture.gen-00000001.redb` + dual-lane queue +
//! `0400` spool tombstones + finalized `storage-format.json`).

#[cfg(feature = "storage-redb")]
use std::{fs, io};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(feature = "storage-redb")]
use crate::storage::{
    capture_store::RedbCaptureStore,
    git_state_store::RedbGitStateStore,
    git_store::RedbGitStore,
    manifest::{
        INITIAL_STORAGE_GENERATION, RedbGenerationPaths, StorageFormatManifest,
        canonical_capture_redb_rel_path, canonical_git_redb_rel_path, publish_storage_manifest,
        resolve_redb_generation_paths,
    },
    meta::init_meta_schema,
    opener::{RedbRepairContext, begin_write_bounded_blocking, create_redb_writable},
    queue::{init_capture_queue, init_redb_capture_spool_tombstones},
    redb::RedbConfigStore,
};
use crate::{
    internal::db::schema::DatabaseRole,
    storage::{
        backend::StorageBackendKind,
        capture_store::CaptureStore,
        config_store::{ConfigStore, MetadataStore},
        error::{StorageBackendError, StorageResult},
        git_state_store::GitStateStore,
        git_store::GitStore,
        manifest::{
            PRIVATE_SUBDIR, REDB_SUBDIR, STORAGE_FORMAT_MANIFEST_FILE, StorageAuthority,
            detect_storage_authority,
        },
    },
};

/// Returns `true` when `storage_dir` contains finalized/staged `redb` repository
/// markers (`storage-format.json` or a `redb/` directory).
#[must_use]
pub fn has_redb_storage_markers(storage_dir: &Path) -> bool {
    storage_dir.join(STORAGE_FORMAT_MANIFEST_FILE).exists()
        || storage_dir.join(REDB_SUBDIR).is_dir()
}

/// Detect the active storage backend for a repository `<common_storage>` directory
/// from its authoritative on-disk layout (`storage-format.json` or `libra.db`).
///
/// Fails closed with [`StorageBackendError::ManifestNotFinalized`] when an
/// unfinalized manifest exists. Does not apply the pre-release CLI lock so
/// internal facades and Track B tests can exercise an already-initialized `redb`
/// repository directly; CLI entry points enforce [`StorageBackendKind::resolve`].
pub fn detect_repo_backend(common_storage: &Path) -> StorageResult<StorageBackendKind> {
    match detect_storage_authority(common_storage)? {
        StorageAuthority::Manifest(layout) => Ok(layout.backend),
        StorageAuthority::LegacySqlite { .. } => Ok(StorageBackendKind::Sqlite),
        StorageAuthority::FreshEmpty => Ok(StorageBackendKind::compiled_default()),
    }
}

/// Select the storage backend for fresh repository initialization (`init`/`clone`).
///
/// Once `REDB_RUNTIME_UNLOCKED` is enabled (`RB-29a`), honors `LIBRA_STORAGE_BACKEND`
/// via [`StorageBackendKind::resolve`]. Prior to unlock, uses [`detect_repo_backend`]
/// so pure-`redb` tests can initialize `redb` repos directly while production CLI
/// entry points continue to enforce the pre-release gate in `src/cli.rs`.
pub fn select_init_backend(common_storage: &Path) -> StorageResult<StorageBackendKind> {
    if crate::storage::backend::REDB_RUNTIME_UNLOCKED {
        StorageBackendKind::resolve(common_storage)
    } else {
        detect_repo_backend(common_storage)
    }
}

/// Resolve the pinned or current repository `<common_storage>` directory.
pub fn current_repo_storage_dir() -> StorageResult<PathBuf> {
    crate::internal::worktree_scope::request_storage_dir()
        .map_err(|err| StorageBackendError::io("resolve repository storage directory", err))
}

/// Detect the active backend for the pinned/current repository.
pub fn detect_current_repo_backend() -> StorageResult<StorageBackendKind> {
    let storage = current_repo_storage_dir()?;
    detect_repo_backend(&storage)
}

/// Resolve the active `git.gen-<id>.redb` path for a `redb` repository.
#[cfg(feature = "storage-redb")]
pub fn resolve_repo_git_redb_path(common_storage: &Path) -> StorageResult<PathBuf> {
    let paths = resolve_redb_generation_paths(common_storage)?.ok_or_else(|| {
        StorageBackendError::NotFound {
            resource: "finalized redb repository manifest (storage-format.json)".to_string(),
        }
    })?;
    Ok(paths.git_db_path)
}

/// Resolve the active `capture.gen-<id>.redb` path for a `redb` repository.
#[cfg(feature = "storage-redb")]
pub fn resolve_repo_capture_redb_path(common_storage: &Path) -> StorageResult<PathBuf> {
    let paths = resolve_redb_generation_paths(common_storage)?.ok_or_else(|| {
        StorageBackendError::NotFound {
            resource: "finalized redb repository manifest (storage-format.json)".to_string(),
        }
    })?;
    Ok(paths.capture_db_path)
}

/// Open a read-only [`GitStore`] for `common_storage` according to its backend.
pub async fn open_git_store_read_only(common_storage: &Path) -> StorageResult<Arc<dyn GitStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(crate::storage::manifest::LEGACY_SQLITE_DB_FILE);
                let store = crate::storage::SqliteGitStore::open_read_only(&db_path).await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let path = resolve_repo_git_redb_path(common_storage)?;
                let store = RedbGitStore::open_read_only(&path)?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                })
            }
        }
    }
}

/// Open a read-write [`GitStore`] for `common_storage` according to its backend.
pub async fn open_git_store_read_write(common_storage: &Path) -> StorageResult<Arc<dyn GitStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(crate::storage::manifest::LEGACY_SQLITE_DB_FILE);
                let store = crate::storage::SqliteGitStore::open(&db_path).await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let path = resolve_repo_git_redb_path(common_storage)?;
                let store = RedbGitStore::open_read_write(&path)?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                })
            }
        }
    }
}

/// Open a read-only [`GitStateStore`] for `common_storage`.
pub async fn open_git_state_store_read_only(
    common_storage: &Path,
) -> StorageResult<Arc<dyn GitStateStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(crate::storage::manifest::LEGACY_SQLITE_DB_FILE);
                let store = crate::storage::SqliteGitStateStore::open_read_only(&db_path).await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let path = resolve_repo_git_redb_path(common_storage)?;
                let store = RedbGitStateStore::open_read_only(&path)?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                })
            }
        }
    }
}

/// Open a read-write [`GitStateStore`] for `common_storage`.
pub async fn open_git_state_store_read_write(
    common_storage: &Path,
) -> StorageResult<Arc<dyn GitStateStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(crate::storage::manifest::LEGACY_SQLITE_DB_FILE);
                let store = crate::storage::SqliteGitStateStore::open(&db_path).await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let path = resolve_repo_git_redb_path(common_storage)?;
                let store = RedbGitStateStore::open_read_write(&path)?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                })
            }
        }
    }
}

/// Open a read-only repository [`ConfigStore`] for `common_storage`.
pub async fn open_repo_config_store_read_only(
    common_storage: &Path,
) -> StorageResult<Arc<dyn ConfigStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(crate::storage::manifest::LEGACY_SQLITE_DB_FILE);
                let store = crate::storage::SqliteConfigStore::open_read_only(
                    &db_path,
                    DatabaseRole::Repository,
                )
                .await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let path = resolve_repo_git_redb_path(common_storage)?;
                let store = RedbConfigStore::open_read_only(&path, DatabaseRole::Repository)?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                })
            }
        }
    }
}

/// Open a read-write repository [`ConfigStore`] for `common_storage`.
pub async fn open_repo_config_store_read_write(
    common_storage: &Path,
) -> StorageResult<Arc<dyn ConfigStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(crate::storage::manifest::LEGACY_SQLITE_DB_FILE);
                let store =
                    crate::storage::SqliteConfigStore::open(&db_path, DatabaseRole::Repository)
                        .await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let path = resolve_repo_git_redb_path(common_storage)?;
                let store = RedbConfigStore::open_read_write(&path, DatabaseRole::Repository)?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                })
            }
        }
    }
}

/// Open a read-only repository [`MetadataStore`] for `common_storage`.
pub async fn open_repo_metadata_store_read_only(
    common_storage: &Path,
) -> StorageResult<Arc<dyn MetadataStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(crate::storage::manifest::LEGACY_SQLITE_DB_FILE);
                let store = crate::storage::SqliteConfigStore::open_read_only(
                    &db_path,
                    DatabaseRole::Repository,
                )
                .await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let path = resolve_repo_git_redb_path(common_storage)?;
                let store = RedbConfigStore::open_read_only(&path, DatabaseRole::Repository)?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                })
            }
        }
    }
}

/// Open a read-write repository [`MetadataStore`] for `common_storage`.
pub async fn open_repo_metadata_store_read_write(
    common_storage: &Path,
) -> StorageResult<Arc<dyn MetadataStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(crate::storage::manifest::LEGACY_SQLITE_DB_FILE);
                let store =
                    crate::storage::SqliteConfigStore::open(&db_path, DatabaseRole::Repository)
                        .await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let path = resolve_repo_git_redb_path(common_storage)?;
                let store = RedbConfigStore::open_read_write(&path, DatabaseRole::Repository)?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                })
            }
        }
    }
}

/// Open a read-only [`CaptureStore`] for `common_storage`.
pub async fn open_capture_store_read_only(
    common_storage: &Path,
) -> StorageResult<Arc<dyn CaptureStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(PRIVATE_SUBDIR).join("capture.db");
                let store = crate::storage::SqliteCaptureStore::open_read_only(&db_path).await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let path = resolve_repo_capture_redb_path(common_storage)?;
                let store = RedbCaptureStore::open_read_only(&path).await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                })
            }
        }
    }
}

/// Open a read-write [`CaptureStore`] for `common_storage`.
#[cfg(feature = "storage-redb")]
pub async fn open_capture_store_read_write(
    common_storage: &Path,
    repair_context: RedbRepairContext,
) -> StorageResult<Arc<dyn CaptureStore>> {
    match detect_repo_backend(common_storage)? {
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = common_storage.join(PRIVATE_SUBDIR).join("capture.db");
                let store = crate::storage::SqliteCaptureStore::open_read_write(&db_path).await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`".to_string(),
                })
            }
        }
        StorageBackendKind::Redb => {
            let path = resolve_repo_capture_redb_path(common_storage)?;
            let store = RedbCaptureStore::open_read_write(&path, repair_context).await?;
            Ok(Arc::new(store))
        }
    }
}

/// Convenience wrappers bound to the pinned/current repository storage dir.
pub async fn open_current_git_store_read_only() -> StorageResult<Arc<dyn GitStore>> {
    let storage = current_repo_storage_dir()?;
    open_git_store_read_only(&storage).await
}

pub async fn open_current_git_store_read_write() -> StorageResult<Arc<dyn GitStore>> {
    let storage = current_repo_storage_dir()?;
    open_git_store_read_write(&storage).await
}

pub async fn open_current_git_state_store_read_only() -> StorageResult<Arc<dyn GitStateStore>> {
    let storage = current_repo_storage_dir()?;
    open_git_state_store_read_only(&storage).await
}

pub async fn open_current_git_state_store_read_write() -> StorageResult<Arc<dyn GitStateStore>> {
    let storage = current_repo_storage_dir()?;
    open_git_state_store_read_write(&storage).await
}

pub async fn open_current_repo_config_store_read_only() -> StorageResult<Arc<dyn ConfigStore>> {
    let storage = current_repo_storage_dir()?;
    open_repo_config_store_read_only(&storage).await
}

pub async fn open_current_repo_config_store_read_write() -> StorageResult<Arc<dyn ConfigStore>> {
    let storage = current_repo_storage_dir()?;
    open_repo_config_store_read_write(&storage).await
}

pub async fn open_current_repo_metadata_store_read_only() -> StorageResult<Arc<dyn MetadataStore>> {
    let storage = current_repo_storage_dir()?;
    open_repo_metadata_store_read_only(&storage).await
}

pub async fn open_current_repo_metadata_store_read_write() -> StorageResult<Arc<dyn MetadataStore>>
{
    let storage = current_repo_storage_dir()?;
    open_repo_metadata_store_read_write(&storage).await
}

/// Atomically initialize a fresh `redb` repository storage layout under
/// `common_storage` (`.libra/redb/git.gen-00000001.redb`,
/// `.libra/private/capture.gen-00000001.redb`, dual-lane capture queue,
/// `0400` `capture-spool/{tmp,ready,quarantine}` tombstones, and finalized
/// `.libra/storage-format.json`). Never creates `.libra/libra.db`.
#[cfg(feature = "storage-redb")]
pub async fn init_redb_repository_storage(
    common_storage: &Path,
) -> StorageResult<RedbGenerationPaths> {
    let redb_dir = common_storage.join(REDB_SUBDIR);
    let private_dir = common_storage.join(PRIVATE_SUBDIR);
    fs::create_dir_all(&redb_dir)
        .map_err(|err| StorageBackendError::io("create .libra/redb directory", err))?;
    fs::create_dir_all(&private_dir)
        .map_err(|err| StorageBackendError::io("create .libra/private directory", err))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&private_dir, fs::Permissions::from_mode(0o700))
            .map_err(|err| StorageBackendError::io("chmod 0700 .libra/private", err))?;
    }

    let generation = INITIAL_STORAGE_GENERATION;
    let git_db_path = common_storage.join(canonical_git_redb_rel_path(generation));
    let capture_db_path = common_storage.join(canonical_capture_redb_rel_path(generation));

    if !git_db_path.exists() {
        create_unified_git_redb(&git_db_path)?;
    }
    if !capture_db_path.exists() {
        let cap = RedbCaptureStore::create(&capture_db_path).await?;
        drop(cap);
    }

    let _ = init_capture_queue(common_storage)?;
    let _ = init_redb_capture_spool_tombstones(common_storage)?;

    let manifest = StorageFormatManifest::new_redb_finalized(generation)?;
    publish_storage_manifest(common_storage, &manifest)?;

    Ok(RedbGenerationPaths {
        generation,
        git_db_path,
        capture_db_path,
    })
}

#[cfg(feature = "storage-redb")]
fn create_unified_git_redb(path: &Path) -> StorageResult<()> {
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tmp_path = parent.join(format!("git.init.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp_path);

    {
        let mut opts = fs::OpenOptions::new();
        opts.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            opts.mode(crate::storage::config_store::REPOSITORY_CONFIG_FILE_MODE)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            let file = opts
                .open(&tmp_path)
                .map_err(|err| StorageBackendError::io("stage unified git.redb temp file", err))?;
            file.set_permissions(fs::Permissions::from_mode(
                crate::storage::config_store::REPOSITORY_CONFIG_FILE_MODE,
            ))
            .map_err(|err| StorageBackendError::io("chmod 0600 unified git.redb temp", err))?;
            drop(file);
        }
        #[cfg(not(unix))]
        {
            let file = opts
                .open(&tmp_path)
                .map_err(|err| StorageBackendError::io("stage unified git.redb temp file", err))?;
            drop(file);
        }

        let db = create_redb_writable(
            &tmp_path,
            DatabaseRole::Repository,
            RedbRepairContext::WriteCommand,
        )?;
        let wtx = begin_write_bounded_blocking(
            &db,
            DatabaseRole::Repository,
            std::time::Duration::from_secs(5),
        )?;
        init_meta_schema(&wtx, DatabaseRole::Repository)?;
        let _ = crate::storage::redb::config_store::initialize_config_tables(
            &wtx,
            DatabaseRole::Repository,
        )?;
        crate::storage::git_store::redb::initialize_core_git_tables(&wtx)?;
        crate::storage::git_state_store::redb::initialize_rb05_state_tables(&wtx)?;
        crate::storage::git_state_store::redb::initialize_rb05b_state_tables(&wtx)?;
        wtx.commit()
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("commit initial unified git.redb schema: {err}"),
            })?;
    }

    match fs::hard_link(&tmp_path, path) {
        Ok(()) => {
            let _ = fs::remove_file(&tmp_path);
            crate::storage::manifest::fsync_dir(parent)?;
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp_path);
            Ok(())
        }
        Err(err) => {
            let _ = fs::remove_file(&tmp_path);
            Err(StorageBackendError::io(
                "publish unified git.redb via hard_link",
                err,
            ))
        }
    }
}
