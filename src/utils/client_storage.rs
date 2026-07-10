//! Client-side object storage gateway.
//!
//! This module is the synchronous facade that the rest of the codebase uses to read,
//! write, and search Git objects. It hides three orthogonal concerns:
//!
//! 1. **Storage backend selection** — local-only, or local cache plus a remote
//!    object_store-backed bucket (S3/R2). Backend is chosen at construction time from
//!    `LIBRA_STORAGE_*` environment variables and `vault.env.*` config entries.
//! 2. **Sync/async bridging** — most of the codebase is synchronous CLI logic, while
//!    every storage backend is async. A dedicated multi-thread Tokio runtime owned by
//!    this module runs the async work and the CLI thread blocks on a `mpsc::channel`,
//!    avoiding nested-runtime panics that would occur if we drove the storage from the
//!    main runtime.
//! 3. **Background object indexing** — every successful `put` enqueues an index-update
//!    message for the cloud-backup object index. The consumer runs serially on the
//!    background runtime so concurrent writers cannot deadlock on the SQLite database.
//!
//! Search supports Git's revision navigation suffixes (`HEAD`, `~`, `^`).

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use async_trait::async_trait;
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use futures::FutureExt; // Import for catch_unwind
use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::object::{commit::Commit, types::ObjectType},
};
use once_cell::sync::Lazy;
use regex::Regex;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Statement,
    Value,
};
use tokio::{
    runtime::Runtime,
    sync::mpsc::{Sender, channel, error::TrySendError},
};
use uuid::Uuid;

use crate::{
    command::load_object,
    internal::{
        branch::Branch,
        config::{ConfigKv, decrypt_value},
        db,
        db::establish_connection_with_busy_timeout,
        head::Head,
        model::object_index,
    },
    utils::{
        storage::{Storage, local::LocalStorage, remote::RemoteStorage, tiered::TieredStorage},
        util::{DATABASE, try_get_storage_path},
    },
};

// Dedicated runtime for storage operations to avoid blocking/deadlocks in the main runtime.
// We never `await` storage from the calling tokio runtime; instead we hand the work to
// this private runtime and block on an mpsc receiver. This avoids `block_on within
// runtime` panics and decouples storage IO from the caller's executor.
static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    // INVARIANT: `Builder::build()` only fails on platform resource
    // exhaustion (cannot spawn the I/O reactor or worker threads). If
    // that happens the process cannot make progress regardless, so
    // surfacing the panic immediately is the right behavior. The
    // panic message identifies that this is the storage runtime so
    // the failure is distinguishable from caller-runtime issues.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build dedicated tokio runtime for ClientStorage IO")
});

// Message describing a single object_index update queued by `ClientStorage::put`.
// Carries enough state for the consumer to run independently of the calling thread.
struct IndexUpdateMsg {
    hash: String,
    obj_type: String,
    size: i64,
    db_path: PathBuf,
}

// RAII guard that decrements PENDING_TASKS exactly once even if the consumer task panics.
// Drop runs on both the success path and during unwinding, so the pending counter
// observed by `wait_for_background_tasks` cannot drift on errors.
struct TaskGuard;
impl Drop for TaskGuard {
    fn drop(&mut self) {
        PENDING_TASKS.fetch_sub(1, Ordering::Relaxed);
    }
}

// Global channel for index updates.
// Bounded (1000) so a runaway producer cannot exhaust memory; producers fall back to
// the runtime-spawned `send` path when `try_send` reports `Full`. The consumer runs
// serially on RUNTIME to avoid SQLite write contention on `.libra/libra.db`.
static INDEX_UPDATE_CHANNEL: Lazy<Sender<IndexUpdateMsg>> = Lazy::new(|| {
    let (tx, mut rx) = channel::<IndexUpdateMsg>(1000);

    RUNTIME.spawn(async move {
        while let Some(msg) = rx.recv().await {
            // Guard ensures decrement happens on drop (scope exit or panic)
            let _guard = TaskGuard;

            // Wrap in AssertUnwindSafe to catch panics from DB operations.
            // This prevents the consumer loop from dying if one update fails hard —
            // a panic here would otherwise stall every subsequent index update for the
            // process lifetime.
            let future = async {
                if let Err(e) =
                    update_object_index(&msg.db_path, &msg.hash, &msg.obj_type, msg.size).await
                {
                    tracing::warn!("Failed to update object index for {}: {}", msg.hash, e);
                }
            };
            let result = std::panic::AssertUnwindSafe(future).catch_unwind().await;

            if let Err(payload) = result {
                tracing::error!("Panic in background index update task: {:?}", payload);
            }
        }
    });

    tx
});

// Counter for active background tasks. Read by `wait_for_background_tasks` so the CLI
// can drain pending index updates before exiting.
static PENDING_TASKS: AtomicUsize = AtomicUsize::new(0);

// Object-index updates run behind foreground repository writes. SQLite can keep
// the repository database locked for longer than a single short busy timeout, so
// cloud backup correctness depends on retrying instead of silently dropping rows.
const INDEX_UPDATE_MAX_ATTEMPTS: usize = 12;

/// Synchronous facade for the configured object backend.
///
/// Wraps a `dyn Storage` (local, remote, or tiered) and adapts every operation to a
/// blocking call by routing through the dedicated [`RUNTIME`]. Cheap to clone —
/// internally it is an `Arc` plus a `PathBuf`.
#[derive(Clone)]
pub struct ClientStorage {
    storage: Arc<dyn Storage>,
    base_path: PathBuf, // Keep base_path for legacy access if needed
}

/// Default tiered-storage small/large object threshold (1 MiB): objects at or
/// above this size are LRU-cached rather than stored permanently locally.
pub const DEFAULT_STORAGE_THRESHOLD_BYTES: usize = 1024 * 1024;
/// Default local LRU disk budget for large cached objects (200 MiB).
pub const DEFAULT_CACHE_SIZE_BYTES: usize = 200 * 1024 * 1024;
/// Operator command shown when an old binary sees a newer global config schema.
pub const INSTALL_NEWER_LIBRA_COMMAND: &str =
    "curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh";

static GLOBAL_CONFIG_SCHEMA_FUTURE_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);
const REMOTE_STORAGE_ENV_KEYS_AFTER_TYPE: &[&str] = &[
    "LIBRA_STORAGE_BUCKET",
    "LIBRA_STORAGE_ENDPOINT",
    "LIBRA_STORAGE_REGION",
    "LIBRA_STORAGE_ACCESS_KEY",
    "LIBRA_STORAGE_SECRET_KEY",
    "LIBRA_STORAGE_ALLOW_HTTP",
    "LIBRA_STORAGE_THRESHOLD",
    "LIBRA_STORAGE_CACHE_SIZE",
];

/// Typed description of a global config DB that this binary cannot safely read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfigSchemaFuture {
    pub db_path: PathBuf,
    pub current_version: i64,
    pub latest_version: Option<i64>,
}

impl GlobalConfigSchemaFuture {
    pub fn latest_supported_display(&self) -> String {
        self.latest_version
            .map(|version| version.to_string())
            .unwrap_or_else(|| "none".to_string())
    }

    pub fn binary_path_display() -> String {
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|err| format!("unknown ({err})"))
    }

    pub fn diagnostic_message(&self, action: &str) -> String {
        format!(
            "global config database schema is newer than this Libra binary supports; binary: {}; version: {}; config database: {}; config schema version: {}; latest supported schema version: {}; {action}; update with: {INSTALL_NEWER_LIBRA_COMMAND}",
            Self::binary_path_display(),
            env!("CARGO_PKG_VERSION"),
            self.db_path.display(),
            self.current_version,
            self.latest_supported_display(),
        )
    }
}

impl std::fmt::Display for GlobalConfigSchemaFuture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "global config database '{}' schema version {} is newer than this Libra binary supports (latest supported: {})",
            self.db_path.display(),
            self.current_version,
            self.latest_supported_display()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StorageConfigResolutionError {
    GlobalSchemaFuture(GlobalConfigSchemaFuture),
    Other(String),
}

impl std::fmt::Display for StorageConfigResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GlobalSchemaFuture(future) => future.fmt(f),
            Self::Other(message) => f.write_str(message),
        }
    }
}

/// Warn once when the global config DB is too new but the current command may
/// continue in an explicit local/offline or config-irrelevant mode.
pub fn emit_global_config_schema_future_warning(future: &GlobalConfigSchemaFuture, action: &str) {
    if GLOBAL_CONFIG_SCHEMA_FUTURE_WARNING_EMITTED
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        crate::utils::error::emit_warning(future.diagnostic_message(action));
    }
}

pub(crate) fn reset_global_config_schema_future_warning_for_invocation() {
    GLOBAL_CONFIG_SCHEMA_FUTURE_WARNING_EMITTED.store(false, Ordering::Relaxed);
}

pub async fn storage_config_resolution_may_read_global_config() -> bool {
    let storage_type = match resolve_env_for_storage_init_without_global("LIBRA_STORAGE_TYPE").await
    {
        Ok(Some(storage_type)) => storage_type,
        Ok(None) => return true,
        Err(_) => return false,
    };
    match storage_type.as_str() {
        "s3" | "r2" => {
            for name in REMOTE_STORAGE_ENV_KEYS_AFTER_TYPE {
                match resolve_env_for_storage_init_without_global(name).await {
                    Ok(Some(_)) => {}
                    Ok(None) => return true,
                    Err(_) => return false,
                }
            }
            false
        }
        _ => false,
    }
}

pub async fn env_resolution_may_read_global_config(names: &[&str]) -> bool {
    for name in names {
        match resolve_env_for_storage_init_without_global(name).await {
            Ok(Some(_)) => {}
            Ok(None) => return true,
            Err(_) => return false,
        }
    }
    false
}

/// The resolved tiered-storage / LRU-cache tunables (lore.md §0.10). Exposes the
/// existing `LIBRA_STORAGE_*` knobs for inspection via `libra cache info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheConfig {
    /// The RAW `LIBRA_STORAGE_TYPE` value (`local` only when unset), e.g.
    /// `s3`/`r2`. Not normalized — a wrong-case `R2` is reported verbatim (and
    /// `tiered` is false), matching how the backend interprets it.
    pub storage_type: String,
    /// Whether the static config selects a durable tier: a case-sensitive
    /// `s3`/`r2` `storage_type` that also passes every static fallback check the
    /// backend applies before connecting (non-empty bucket, parseable endpoint
    /// URL, non-empty access/secret key). The cache tunables only take effect
    /// when tiered. NB: an actual connection additionally requires valid
    /// credentials, which this static report does not validate.
    pub tiered: bool,
    /// Small/large object threshold in bytes (`LIBRA_STORAGE_THRESHOLD`).
    pub threshold_bytes: usize,
    /// Local LRU disk budget in bytes (`LIBRA_STORAGE_CACHE_SIZE`).
    pub cache_size_bytes: usize,
}

/// Resolve the cache/storage tunables the way [`ClientStorage::create_storage_backend`]
/// does (env first, then the global config DB via `resolve_env_sync`), mirroring
/// its lenient parse — an unparseable numeric value falls back to the default,
/// exactly as the storage backend would use it. Used by `libra cache info` so
/// the reported values match what the running backend applies.
///
/// # Errors
/// Propagates a config-resolution failure (e.g. an unreadable global config DB).
/// Whether the S3/R2 static pre-connection checks pass, resolved in the SAME
/// order as [`ClientStorage::create_storage_backend`] and short-circuiting to
/// `false` at the first static fallback (empty bucket / unparseable endpoint /
/// empty access or secret key). Each var is resolved in order so a
/// config-resolution error only surfaces for a var the backend would actually
/// have reached — `tiered` is thus never over-reported. `REGION`/`ALLOW_HTTP`
/// values do not gate tiering (the backend accepts any), but a resolution error
/// on either still degrades the backend to local, so they are resolved too.
fn tiered_static_checks_pass() -> Result<bool, String> {
    if resolve_env_sync("LIBRA_STORAGE_BUCKET")?.is_some_and(|bucket| bucket.is_empty()) {
        return Ok(false);
    }
    if let Some(endpoint) = resolve_env_sync("LIBRA_STORAGE_ENDPOINT")?
        && url::Url::parse(&endpoint).is_err()
    {
        return Ok(false);
    }
    let _region = resolve_env_sync("LIBRA_STORAGE_REGION")?;
    if resolve_env_sync("LIBRA_STORAGE_ACCESS_KEY")?.is_some_and(|key| key.is_empty()) {
        return Ok(false);
    }
    if resolve_env_sync("LIBRA_STORAGE_SECRET_KEY")?.is_some_and(|secret| secret.is_empty()) {
        return Ok(false);
    }
    let _allow_http = resolve_env_sync("LIBRA_STORAGE_ALLOW_HTTP")?;
    Ok(true)
}

pub fn resolve_cache_config() -> Result<CacheConfig, String> {
    let (storage_type, mut tiered) = match resolve_env_sync("LIBRA_STORAGE_TYPE")? {
        // Raw, case-sensitive match — identical to create_storage_backend, so a
        // value the backend rejects (e.g. `R2`, `" r2 "`) reports non-tiered
        // rather than misleading the user into thinking tiering is active.
        Some(raw) => {
            let tiered = matches!(raw.as_str(), "s3" | "r2");
            (raw, tiered)
        }
        None => ("local".to_string(), false),
    };
    // Mirror every static pre-connection fallback the backend applies, in the
    // SAME order, so `tiered` is never over-reported. (An actual connection
    // additionally needs valid credentials, which a static report cannot verify.)
    if tiered {
        tiered = tiered_static_checks_pass()?;
    }

    // Raw `.parse()` (no trim) mirrors the backend exactly: an unparseable value
    // like `" 2048 "` falls back to the default, just as the backend applies it.
    let threshold_bytes = match resolve_env_sync("LIBRA_STORAGE_THRESHOLD")? {
        Some(raw) => raw.parse().unwrap_or(DEFAULT_STORAGE_THRESHOLD_BYTES),
        None => DEFAULT_STORAGE_THRESHOLD_BYTES,
    };
    let cache_size_bytes = match resolve_env_sync("LIBRA_STORAGE_CACHE_SIZE")? {
        Some(raw) => raw.parse().unwrap_or(DEFAULT_CACHE_SIZE_BYTES),
        None => DEFAULT_CACHE_SIZE_BYTES,
    };

    Ok(CacheConfig {
        storage_type,
        tiered,
        threshold_bytes,
        cache_size_bytes,
    })
}

impl ClientStorage {
    /// Evict verified-durable large objects until under budget (lore.md
    /// 2.9). `Ok(None)` when the backing store is not tiered.
    pub async fn evict_local(
        &self,
        request: crate::utils::storage::EvictRequest,
    ) -> Result<Option<crate::utils::storage::EvictReport>, git_internal::errors::GitError> {
        self.storage.evict_local(request).await
    }

    pub fn base_path(&self) -> &PathBuf {
        &self.base_path
    }

    /// Construct a `ClientStorage` rooted at `base_path` (typically `.libra/objects`).
    ///
    /// Functional scope:
    /// - Picks the storage backend based on env / vault config (see
    ///   [`Self::create_storage_backend`]). Local-only when `LIBRA_STORAGE_TYPE` is
    ///   absent.
    ///
    /// Boundary conditions:
    /// - Never panics on misconfiguration: any unrecoverable env error degrades to
    ///   `LocalStorage` with a one-line error written to stderr. This means a broken
    ///   `LIBRA_STORAGE_*` setting silently disables remote backup instead of stopping
    ///   the CLI.
    pub fn init(base_path: PathBuf) -> ClientStorage {
        let storage = Self::create_storage_backend(base_path.clone());
        ClientStorage { storage, base_path }
    }

    /// Construct a strictly **local** `ClientStorage` rooted at `base_path`,
    /// ignoring `LIBRA_STORAGE_TYPE` and any cloud configuration.
    ///
    /// Use this when reading a *foreign* object store (for example another
    /// repository's `objects` directory): the tiered backend would otherwise
    /// fall back to the configured remote on a miss and could write fetched
    /// objects back into that foreign directory using cloud credentials.
    pub fn init_local(base_path: PathBuf) -> ClientStorage {
        let storage = Arc::new(LocalStorage::new(base_path.clone()));
        ClientStorage { storage, base_path }
    }

    /// Create a storage backend.
    ///
    /// # Remote Storage
    /// If `LIBRA_STORAGE_TYPE` is set to "s3" or "r2", it configures a tiered storage
    /// with local cache and remote persistence.
    ///
    /// ## Repo ID Isolation
    /// When remote storage is enabled, it attempts to read `libra.repoid` from the configuration.
    /// If found, it uses `repo_id` as a key prefix (`<repo_id>/objects/...`) for isolation.
    /// If not found (e.g., during init before config exists), it defaults to no prefix (root of bucket),
    /// which might be risky for multi-tenant buckets but acceptable for single-repo buckets.
    ///
    /// Boundary conditions:
    /// - Any env-var resolution error degrades to `LocalStorage` (see
    ///   [`Self::storage_config_resolution_fallback`]); the user sees the failure on
    ///   stderr but the CLI continues.
    /// - An empty bucket / access key / secret key, or a non-URL endpoint, also
    ///   triggers a degrade-to-local with an error message.
    /// - Unknown `LIBRA_STORAGE_TYPE` values (anything other than `s3`/`r2`) print
    ///   "Unsupported storage type" and degrade to local.
    /// - `LIBRA_STORAGE_THRESHOLD` and `LIBRA_STORAGE_CACHE_SIZE` accept any
    ///   parseable usize and silently fall back to defaults (1 MiB, 200 MiB) when the
    ///   value is not a valid number.
    /// - The `expect("Failed to build S3 storage")` is the one panicking path: it
    ///   only fires if the partial AWS builder is missing a required field, which
    ///   should be impossible given the explicit checks above.
    fn create_storage_backend(base_path: PathBuf) -> Arc<dyn Storage> {
        // Check for object storage configuration.
        // Uses the typed resolver so vault-stored secrets are picked up without
        // turning a too-new global config schema into a silent local fallback.
        let storage_type = match resolve_env_sync_typed("LIBRA_STORAGE_TYPE") {
            Ok(Some(storage_type)) => storage_type,
            Ok(None) => {
                return Arc::new(LocalStorage::new_with_alternates(base_path));
            }
            Err(err) => {
                return Self::storage_config_resolution_fallback(
                    &base_path,
                    "LIBRA_STORAGE_TYPE",
                    &err,
                );
            }
        };

        let bucket = match resolve_env_sync_typed("LIBRA_STORAGE_BUCKET") {
            Ok(Some(bucket)) => bucket,
            Ok(None) => "libra".to_string(),
            Err(err) => {
                return Self::storage_config_resolution_fallback(
                    &base_path,
                    "LIBRA_STORAGE_BUCKET",
                    &err,
                );
            }
        };
        if bucket.is_empty() {
            eprintln!(
                "Warning: LIBRA_STORAGE_BUCKET cannot be empty. Falling back to local storage."
            );
            return Arc::new(LocalStorage::new_with_alternates(base_path));
        }

        // Build ObjectStore
        let object_store: Arc<dyn object_store::ObjectStore> = match storage_type.as_str() {
            "s3" | "r2" => {
                let mut builder =
                    object_store::aws::AmazonS3Builder::new().with_bucket_name(&bucket);

                // Bound object_store's built-in retry (which already backs off on
                // 429/`SlowDown`/5xx and honours `Retry-After`) to the same caps
                // as `utils::backoff::RetryPolicy`, so no remote path can hammer
                // the backend or hang unbounded. See `docs/development/gap/lore.md`
                // §0.2 / §7.6.
                builder = builder.with_retry(object_store::RetryConfig {
                    backoff: object_store::BackoffConfig {
                        init_backoff: Duration::from_millis(200),
                        max_backoff: Duration::from_secs(10),
                        base: 2.0,
                    },
                    max_retries: 5,
                    retry_timeout: Duration::from_secs(60),
                });

                let endpoint = match resolve_env_sync_typed("LIBRA_STORAGE_ENDPOINT") {
                    Ok(endpoint) => endpoint,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_ENDPOINT",
                            &err,
                        );
                    }
                };
                if let Some(endpoint) = endpoint {
                    if url::Url::parse(&endpoint).is_err() {
                        eprintln!(
                            "Warning: Invalid LIBRA_STORAGE_ENDPOINT URL: {}. Falling back to local storage.",
                            endpoint
                        );
                        return Arc::new(LocalStorage::new_with_alternates(base_path));
                    }
                    builder = builder.with_endpoint(endpoint);
                }
                let region = match resolve_env_sync_typed("LIBRA_STORAGE_REGION") {
                    Ok(region) => region,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_REGION",
                            &err,
                        );
                    }
                };
                if let Some(region) = region {
                    builder = builder.with_region(region);
                }
                let key = match resolve_env_sync_typed("LIBRA_STORAGE_ACCESS_KEY") {
                    Ok(key) => key,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_ACCESS_KEY",
                            &err,
                        );
                    }
                };
                if let Some(key) = key {
                    if key.is_empty() {
                        eprintln!(
                            "Warning: LIBRA_STORAGE_ACCESS_KEY cannot be empty. Falling back to local storage."
                        );
                        return Arc::new(LocalStorage::new_with_alternates(base_path));
                    }
                    builder = builder.with_access_key_id(key);
                }
                let secret = match resolve_env_sync_typed("LIBRA_STORAGE_SECRET_KEY") {
                    Ok(secret) => secret,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_SECRET_KEY",
                            &err,
                        );
                    }
                };
                if let Some(secret) = secret {
                    if secret.is_empty() {
                        eprintln!(
                            "Warning: LIBRA_STORAGE_SECRET_KEY cannot be empty. Falling back to local storage."
                        );
                        return Arc::new(LocalStorage::new_with_alternates(base_path));
                    }
                    builder = builder.with_secret_access_key(secret);
                }

                let allow_http = match resolve_env_sync_typed("LIBRA_STORAGE_ALLOW_HTTP") {
                    Ok(allow_http) => allow_http,
                    Err(err) => {
                        return Self::storage_config_resolution_fallback(
                            &base_path,
                            "LIBRA_STORAGE_ALLOW_HTTP",
                            &err,
                        );
                    }
                };
                if allow_http.as_deref() == Some("true") {
                    builder = builder.with_allow_http(true);
                }

                Arc::new(builder.build().unwrap_or_else(|err| {
                    panic!(
                        "ClientStorage::with_remote: failed to build S3 storage with endpoint/\
                         bucket/region/credentials from LIBRA_STORAGE_* env: {err}"
                    )
                }))
            }
            _ => {
                eprintln!(
                    "Warning: Unsupported storage type: {}. Falling back to local storage.",
                    storage_type
                );
                return Arc::new(LocalStorage::new_with_alternates(base_path));
            }
        };

        let remote = match get_or_create_repo_id_for_prefix() {
            Some(repo_id) => RemoteStorage::new_with_prefix(object_store, repo_id),
            None => RemoteStorage::new(object_store),
        };
        let local = LocalStorage::new_with_alternates(base_path.clone());

        let threshold = match resolve_env_sync_typed("LIBRA_STORAGE_THRESHOLD") {
            Ok(Some(raw_threshold)) => raw_threshold
                .parse()
                .unwrap_or(DEFAULT_STORAGE_THRESHOLD_BYTES),
            Ok(None) => DEFAULT_STORAGE_THRESHOLD_BYTES,
            Err(err) => {
                return Self::storage_config_resolution_fallback(
                    &base_path,
                    "LIBRA_STORAGE_THRESHOLD",
                    &err,
                );
            }
        };

        // Parse cache size (previously hardcoded/magic number)
        let disk_cache_limit_bytes = match resolve_env_sync_typed("LIBRA_STORAGE_CACHE_SIZE") {
            Ok(Some(raw_size)) => raw_size.parse().unwrap_or(DEFAULT_CACHE_SIZE_BYTES),
            Ok(None) => DEFAULT_CACHE_SIZE_BYTES,
            Err(err) => {
                return Self::storage_config_resolution_fallback(
                    &base_path,
                    "LIBRA_STORAGE_CACHE_SIZE",
                    &err,
                );
            }
        };

        Arc::new(TieredStorage::new(
            local,
            remote,
            threshold,
            disk_cache_limit_bytes,
        ))
    }

    /// Emit a stderr warning and degrade to `LocalStorage` when a storage env
    /// var cannot be resolved. Centralised so every fallback prints the same
    /// message shape and ensures CLI commands keep working when remote storage
    /// is broken — the `Warning:` prefix mirrors the recovered, non-fatal
    /// nature of the degrade path so users do not mistake it for a fatal
    /// command failure (e.g. a `~/.libra/config.db` whose schema is newer than
    /// this binary supports still surfaces a chain like "Repository database
    /// schema version ... is newer than this Libra binary supports", but the
    /// clone/init operation itself still succeeds via `LocalStorage`).
    fn storage_config_resolution_fallback(
        base_path: &Path,
        name: &str,
        error: &StorageConfigResolutionError,
    ) -> Arc<dyn Storage> {
        if let StorageConfigResolutionError::GlobalSchemaFuture(future) = error {
            emit_global_config_schema_future_warning(
                future,
                "ignoring global storage config and falling back to local storage",
            );
            return Arc::new(LocalStorage::new_with_alternates(base_path.to_path_buf()));
        }
        eprintln!(
            "Warning: failed to resolve {}: {}. Falling back to local storage.",
            name, error
        );
        Arc::new(LocalStorage::new_with_alternates(base_path.to_path_buf()))
    }

    /// Helper to execute async task on dedicated runtime and block waiting for result.
    ///
    /// Functional scope:
    /// - Spawns `future` on the private [`RUNTIME`] and blocks the calling thread on
    ///   an `mpsc::channel` until the result is delivered.
    ///
    /// Boundary conditions:
    /// - Panics if the runtime drops the future before sending a result (e.g. runtime
    ///   shutdown) — this indicates a programmer error since RUNTIME is a `Lazy`
    ///   static and should outlive the process.
    /// - Safe to call from inside another tokio runtime: the work runs on RUNTIME, not
    ///   the caller's runtime, so nested-runtime panics are avoided.
    fn block_on_storage<F, T>(&self, future: F) -> T
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        RUNTIME.spawn(async move {
            let res = future.await;
            let _ = tx.send(res);
        });
        // INVARIANT: the spawned task above always either returns or panics
        // before exiting. recv() therefore only returns Err if the spawned
        // task panicked (sender dropped before sending). The function's doc
        // comment already documents this as a programmer error since
        // RUNTIME is a `Lazy` static that outlives the process.
        rx.recv()
            .expect("ClientStorage storage-runtime task panicked before sending result")
    }

    /// Wait for all background tasks (e.g. indexing) to complete.
    ///
    /// Functional scope:
    /// - Polls [`PENDING_TASKS`] every 100 ms until it reaches zero; logs a progress
    ///   line every 5 s so a stuck index update is visible to the user.
    ///
    /// Boundary conditions:
    /// - Has no upper time bound. If the consumer is wedged the call blocks forever;
    ///   in practice the only path that can wedge is a SQLite lock contention bug,
    ///   which the consumer's panic catcher and short busy timeouts already mitigate.
    /// - Called by the top-level CLI dispatcher just before process exit so queued
    ///   index updates are not killed mid-write.
    pub fn wait_for_background_tasks() {
        // Wait until all tasks finish
        let mut waited = 0;
        loop {
            let pending = PENDING_TASKS.load(Ordering::Relaxed);
            if pending == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            waited += 100;
            if waited >= 5000 {
                tracing::info!("Waiting for {} background tasks to complete...", pending);
                waited = 0;
            }
        }
    }

    /// Read a Git object's *raw payload* by its hash.
    ///
    /// Functional scope:
    /// - Returns the object content only; the `ObjectType` is dropped here. Use
    ///   [`Self::get_object_type`] when the type is needed.
    ///
    /// Boundary conditions:
    /// - Returns `GitError::ObjectNotFound` when neither local cache nor remote
    ///   bucket holds the object.
    /// - Blocks the calling thread on the storage runtime; safe to call from sync or
    ///   async contexts.
    pub fn get(&self, object_id: &ObjectHash) -> Result<Vec<u8>, GitError> {
        let storage = self.storage.clone();
        let hash = *object_id;
        self.block_on_storage(async move { storage.get(&hash).await.map(|(data, _)| data) })
    }

    /// Attempt to repair a missing or corrupted object from the durable tier
    /// (`libra fsck --heal`, lore.md §0.4).
    ///
    /// Returns `Ok(true)` when the object was fetched, verified, and written
    /// locally; `Ok(false)` when there is no durable tier (local-only backend)
    /// or the object is absent from it. Never fabricates an object: only a
    /// payload that verifies against `object_id` is persisted. See
    /// [`crate::utils::storage::Storage::heal`].
    pub fn heal(&self, object_id: &ObjectHash) -> Result<bool, GitError> {
        let storage = self.storage.clone();
        let hash = *object_id;
        self.block_on_storage(async move { storage.heal(&hash).await })
    }

    /// Persist a Git object and queue a background index update.
    ///
    /// Functional scope:
    /// - Writes the object via the configured backend (synchronously, on the storage
    ///   runtime), then enqueues an [`IndexUpdateMsg`] so the cloud-backup object
    ///   index reflects the new entry.
    ///
    /// Boundary conditions:
    /// - The index update is best-effort: if the bounded channel is full, the message
    ///   is forwarded to a runtime task that performs the blocking `send`. If the
    ///   channel is closed (RUNTIME tearing down), the index update is dropped with a
    ///   `tracing::warn` and the put still succeeds.
    /// - Returns `io::Error` (instead of `GitError`) so callers using `std::io`
    ///   abstractions can propagate the error directly.
    /// - The index update is skipped silently when the database path cannot be
    ///   resolved (e.g. base_path has no parent), since some test harnesses use
    ///   non-standard layouts.
    /// - See: `test_content_store`, `background_index_update_uses_storage_database_instead_of_cwd`.
    pub fn put(
        &self,
        obj_id: &ObjectHash,
        content: &[u8],
        obj_type: ObjectType,
    ) -> Result<String, io::Error> {
        let storage = self.storage.clone();
        let hash = *obj_id;
        let data = content.to_vec();
        let data_len = data.len();
        let hash_str = hash.to_string();
        let type_str = obj_type.to_string();

        // First, store the object
        let result = self.block_on_storage(async move {
            storage
                .put(&hash, &data, obj_type)
                .await
                .map_err(|e| io::Error::other(e.to_string()))
        })?;

        // Update object index asynchronously (via sequential queue)
        // This ensures CLI commands don't block on indexing, and avoids DB lock contention.
        if let Some(db_path) = Self::index_db_path_from_base(&self.base_path)
            && db_path.exists()
        {
            let hash_str = hash_str.clone();
            let type_str = type_str.clone();

            PENDING_TASKS.fetch_add(1, Ordering::Relaxed);

            // Send to global channel
            // If channel is closed (runtime shutting down), we can't do much, but that's unlikely in normal CLI flow.
            let msg = IndexUpdateMsg {
                hash: hash_str,
                obj_type: type_str,
                size: data_len as i64,
                db_path,
            };

            match INDEX_UPDATE_CHANNEL.try_send(msg) {
                Ok(_) => {}
                Err(TrySendError::Full(msg)) => {
                    // Avoid blocking the caller thread if the bounded queue is
                    // full; wait for capacity on the dedicated storage runtime.
                    RUNTIME.spawn(async move {
                        if INDEX_UPDATE_CHANNEL.send(msg).await.is_err() {
                            PENDING_TASKS.fetch_sub(1, Ordering::Relaxed);
                            tracing::warn!("Failed to queue object index update: channel closed");
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    PENDING_TASKS.fetch_sub(1, Ordering::Relaxed);
                    tracing::warn!("Failed to queue object index update: channel closed");
                }
            }
        }

        Ok(result)
    }

    /// Physically delete an object's payload (lore.md 2.5) from the durable
    /// tier and the in-memory cache; a no-op for a local-only store.
    pub async fn delete_payload(&self, hash: &ObjectHash) -> Result<(), GitError> {
        self.storage.delete_payload(hash).await
    }

    /// Check whether an object exists in the configured backend.
    ///
    /// Boundary conditions:
    /// - For tiered storage, returns `true` if the object lives in either tier; does
    ///   not promote the object to the local cache.
    pub fn exist(&self, obj_id: &ObjectHash) -> bool {
        let storage = self.storage.clone();
        let hash = *obj_id;
        self.block_on_storage(async move { storage.exist(&hash).await })
    }

    /// Read just the `ObjectType` for `obj_id`.
    ///
    /// Boundary conditions:
    /// - For backends that store the object body inline with its type header, this
    ///   may decode the entire body and discard the payload. Prefer
    ///   [`Self::is_object_type`] when only checking a single type.
    pub fn get_object_type(&self, obj_id: &ObjectHash) -> Result<ObjectType, GitError> {
        let storage = self.storage.clone();
        let hash = *obj_id;
        self.block_on_storage(async move { storage.get(&hash).await.map(|(_, t)| t) })
    }

    /// Convenience wrapper: returns whether `obj_id` resolves to an object of the
    /// requested type. Returns `false` on any read error (rather than propagating)
    /// because callers typically use this in match arms where missing-or-wrong-type
    /// have the same effect.
    pub fn is_object_type(&self, obj_id: &ObjectHash, obj_type: ObjectType) -> bool {
        match self.get_object_type(obj_id) {
            Ok(t) => t == obj_type,
            Err(_) => false,
        }
    }

    /// Search for objects matching the provided revision-ish identifier.
    ///
    /// Functional scope:
    /// - Wraps [`Self::search_result`]; logs and swallows errors to keep the simple
    ///   "list of hashes" return shape that callers expect.
    ///
    /// Boundary conditions:
    /// - On any error, returns an empty vector and logs an `error!`. Use
    ///   [`Self::search_result`] when the caller needs to react to the error.
    pub async fn search(&self, obj_id: &str) -> Vec<ObjectHash> {
        match self.search_result(obj_id).await {
            Ok(matches) => matches,
            Err(error) => {
                tracing::error!("failed to search objects for '{obj_id}': {error}");
                Vec::new()
            }
        }
    }

    /// Search for objects matching `obj_id`, surfacing errors to the caller.
    ///
    /// Functional scope:
    /// - Recognises `HEAD`, branch names, and Git navigation suffixes (`~`, `^`).
    /// - For navigation forms (`HEAD~3`, `main^^`) resolves the base ref then walks
    ///   parent commits via [`Self::navigate_commit_path`].
    /// - For prefix matches (e.g. an abbreviated SHA) delegates to the underlying
    ///   storage's `search`.
    ///
    /// Boundary conditions:
    /// - Returns `Ok(vec![])` when an empty base ref is supplied (e.g. `~1`, `^2`)
    ///   to avoid degenerating into a prefix search of all objects.
    /// - Returns `Ok(vec![])` when the base ref is ambiguous (multiple matching
    ///   commit objects). The caller decides whether ambiguity is an error.
    /// - Returns `Err` when an underlying database/branch read fails (e.g. corrupt
    ///   `reference` row), so users see the actionable error instead of silent empty.
    /// - See: `test_search_result_surfaces_corrupt_branch_storage`,
    ///   `test_search_result_rejects_empty_base_ref_navigation`.
    pub async fn search_result(&self, obj_id: &str) -> Result<Vec<ObjectHash>, GitError> {
        if obj_id == "HEAD" {
            return Ok(Head::current_commit_result()
                .await
                .map_err(|error| GitError::CustomError(format!("failed to resolve HEAD: {error}")))?
                .into_iter()
                .collect());
        }

        if obj_id.contains('~') || obj_id.contains('^') {
            // Complex navigation relies on sync object loads. This stays on the
            // current runtime thread and delegates object reads through `self.get()`,
            // which already uses the dedicated background runtime.
            let mut split_pos = 0;
            let mut found_special = false;
            for (i, c) in obj_id.char_indices() {
                if c == '~' || c == '^' {
                    found_special = true;
                    split_pos = i;
                    break;
                }
            }

            if found_special {
                let base_ref = &obj_id[..split_pos];
                let path_part = &obj_id[split_pos..];

                // Reject empty base_ref (e.g. user passes "~1" or "^2") to avoid
                // a degenerate prefix search for "" which would list all objects.
                if base_ref.is_empty() {
                    return Ok(Vec::new());
                }

                let base_commit =
                    match base_ref {
                        "HEAD" => match Head::current_commit_result().await.map_err(|error| {
                            GitError::CustomError(format!("failed to resolve HEAD: {error}"))
                        })? {
                            Some(commit) => commit,
                            None => return Ok(Vec::new()),
                        },
                        _ => match Branch::find_branch_result(base_ref, None).await.map_err(
                            |error| {
                                GitError::CustomError(format!(
                                    "failed to resolve branch '{base_ref}': {error}"
                                ))
                            },
                        )? {
                            Some(branch) => branch.commit,
                            None => {
                                if Branch::exists_result(base_ref, None)
                                    .await
                                    .map_err(|error| {
                                        GitError::CustomError(format!(
                                            "failed to resolve branch '{base_ref}': {error}"
                                        ))
                                    })?
                                {
                                    return Ok(Vec::new());
                                }

                                let matches = self.storage.search(base_ref).await;
                                let commits: Vec<ObjectHash> = matches
                                    .into_iter()
                                    .filter(|x| self.is_object_type(x, ObjectType::Commit))
                                    .collect();

                                if commits.len() == 1 {
                                    commits[0]
                                } else {
                                    return Ok(Vec::new());
                                }
                            }
                        },
                    };

                let target_commit = match self.navigate_commit_path(base_commit, path_part) {
                    Ok(commit) => commit,
                    Err(_) => return Ok(Vec::new()),
                };

                return Ok(vec![target_commit]);
            }
        }

        Ok(self.storage.search(obj_id).await)
    }

    /// Walk parent commits according to a Git revision suffix.
    ///
    /// Functional scope:
    /// - Parses every `~N` and `^N` token in `path` and walks accordingly:
    ///   `^N` selects the Nth parent of the current commit; `~N` walks N first-parent
    ///   steps.
    ///
    /// Boundary conditions:
    /// - Returns `GitError::InvalidArgument` when `path` does not match the expected
    ///   shape at all (defensive: callers already pre-filter on `~` / `^`).
    /// - Returns `GitError::ObjectNotFound` when a requested parent index does not
    ///   exist (e.g. `~5` on a commit whose history is shorter, or `^2` on a non-merge
    ///   commit).
    /// - When the count is missing (`~` rather than `~1`) it defaults to 1, matching
    ///   Git's convention.
    fn navigate_commit_path(
        &self,
        base_commit: ObjectHash,
        path: &str,
    ) -> Result<ObjectHash, GitError> {
        let mut current = base_commit;
        // INVARIANT: compile-time literal regex with two capture groups;
        // Regex::new only fails on syntactically invalid patterns, which
        // is caught by the surrounding parent-traversal tests.
        let re =
            Regex::new(r"(\^|~)(\d*)").expect("revision-suffix regex is a valid hardcoded pattern");

        if !re.is_match(path) {
            return Err(GitError::InvalidArgument(format!(
                "Invalid reference path: {path}"
            )));
        }
        for cap in re.captures_iter(path) {
            // INVARIANT: capture group 1 is non-optional (`(\^|~)`), so any
            // match produced by `captures_iter` is guaranteed to populate it.
            let symbol = cap
                .get(1)
                .expect("regex capture group 1 is non-optional")
                .as_str();
            let num_str = cap.get(2).map_or("1", |m| m.as_str());
            let num: usize = num_str.parse().unwrap_or(1);

            match symbol {
                "^" => {
                    current = self.get_parent_commit(&current, num)?;
                }
                "~" => {
                    for _ in 0..num {
                        current = self.get_parent_commit(&current, 1)?;
                    }
                }
                // INVARIANT: regex `(\^|~)(\d*)` only captures "^" or "~" in
                // group 1, so `symbol` cannot hold any other value here.
                _ => unreachable!("regex capture group 1 is restricted to \"^\" or \"~\""),
            }
        }
        Ok(current)
    }

    /// Return the Nth parent (1-indexed) of `commit_id`.
    ///
    /// Boundary conditions:
    /// - Returns `GitError::ObjectNotFound` when `n == 0` or `n` exceeds the parent
    ///   count. Callers using `^` semantics never pass 0; the explicit check is for
    ///   safety against future callers.
    fn get_parent_commit(&self, commit_id: &ObjectHash, n: usize) -> Result<ObjectHash, GitError> {
        let commit: Commit = load_object(commit_id)?;
        if n == 0 || n > commit.parent_commit_ids.len() {
            return Err(GitError::ObjectNotFound(format!(
                "Parent {n} does not exist"
            )));
        }
        Ok(commit.parent_commit_ids[n - 1])
    }

    /// Compress `data` with zlib using the default compression level — exposed for
    /// tests and other utilities that produce loose-object byte streams.
    pub fn compress_zlib(data: &[u8]) -> io::Result<Vec<u8>> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        let compressed_data = encoder.finish()?;
        Ok(compressed_data)
    }

    /// Inverse of [`Self::compress_zlib`] — decompress a previously-zlib-compressed
    /// byte slice. Used by tests and pack inspection paths.
    pub fn decompress_zlib(data: &[u8]) -> io::Result<Vec<u8>> {
        let mut decoder = ZlibDecoder::new(data);
        let mut decompressed_data = Vec::new();
        decoder.read_to_end(&mut decompressed_data)?;
        Ok(decompressed_data)
    }

    /// Map `<storage>/objects` back to `<storage>/<DATABASE>` so background index
    /// updates write to the database that owns this objects directory rather than
    /// to whichever database happens to be discoverable from the process CWD.
    fn index_db_path_from_base(base_path: &Path) -> Option<PathBuf> {
        base_path
            .parent()
            .map(|storage_path| storage_path.join(DATABASE))
    }
}

/// Enqueue an `object_index` row for an object that was written outside the
/// usual `ClientStorage::put` path — currently agent capture transcript and
/// metadata blobs, which `HistoryManager::append_checkpoint_commit` writes
/// directly via [`crate::utils::object::write_git_object`] for the orphan
/// `refs/libra/traces` history.
///
/// Why this exists: cloud sync uploads only the rows it finds in
/// `object_index` — anything that bypasses `object_index` is invisible to
/// `libra cloud sync`. Without this hook, agent transcripts written by the
/// hook runtime would never reach R2, and the Phase 3.5b `cloud restore`
/// catalogue would resolve commit OIDs that pointed at missing blobs on a
/// fresh clone (entire.md §14.3 phase-3 item 3 — "走正常 R2 同步").
///
/// The function takes the `.libra` directory rather than the storage objects
/// path because agent capture callers already hold a `repo_path` shaped that
/// way; the db lives at `<libra_dir>/<DATABASE>`. Returns immediately when
/// the database file is absent so legacy bootstrap and tempdir tests stay
/// quiet.
///
/// `pub(crate)` — there is no validation that the (`o_id`, `o_type`,
/// `o_size`) triple matches an actual on-disk Git object, so this is an
/// internal escape hatch for callers that already hold the truth (only
/// `HistoryManager` today). External crates / users must go through
/// `ClientStorage::put`, which both writes the object and indexes it.
pub(crate) fn enqueue_agent_blob_object_index_update(
    libra_dir: &Path,
    o_id: &str,
    o_type: &str,
    o_size: i64,
) {
    let db_path = libra_dir.join(DATABASE);
    if !db_path.exists() {
        return;
    }
    let msg = IndexUpdateMsg {
        hash: o_id.to_string(),
        obj_type: o_type.to_string(),
        size: o_size,
        db_path,
    };
    PENDING_TASKS.fetch_add(1, Ordering::Relaxed);
    match INDEX_UPDATE_CHANNEL.try_send(msg) {
        Ok(_) => {}
        Err(TrySendError::Full(msg)) => {
            // Bounded queue is full. Don't block the caller — spawn the
            // send onto the storage runtime so the agent capture hook
            // path keeps moving even when the foreground commit pipeline
            // is producing index updates faster than the consumer can
            // drain them. Mirrors the policy used by `ClientStorage::put`.
            RUNTIME.spawn(async move {
                if INDEX_UPDATE_CHANNEL.send(msg).await.is_err() {
                    PENDING_TASKS.fetch_sub(1, Ordering::Relaxed);
                    tracing::warn!(
                        "Failed to queue agent blob object index update: channel closed"
                    );
                }
            });
        }
        Err(TrySendError::Closed(_)) => {
            PENDING_TASKS.fetch_sub(1, Ordering::Relaxed);
            tracing::warn!("Failed to queue agent blob object index update: channel closed");
        }
    }
}

/// Delete `object_index` rows for the given OIDs in the current repo
/// (AG-20 prune-side counterpart of
/// [`enqueue_agent_blob_object_index_update`]).
///
/// Functional scope:
/// - Resolves the repo id the same way the indexing writer does
///   (`libra.repoid` config, falling back to `unknown-repo`) so the delete
///   predicate matches the rows the writer created.
/// - Deletes in bounded `IN (...)` chunks and returns the total number of
///   rows removed. Idempotent: OIDs without a row simply delete nothing.
///
/// Boundary conditions:
/// - Returns `Ok(0)` without touching anything when `oids` is empty or the
///   `object_index` table does not exist (minimal test databases).
/// - `pub(crate)` — callers must already have proven the OIDs unreachable
///   (only `HistoryManager::commit_checkpoint_prune` today); there is no
///   reachability validation here.
pub(crate) async fn remove_object_index_rows_with_conn<C: ConnectionTrait>(
    conn: &C,
    oids: &[String],
) -> Result<u64, DbErr> {
    if oids.is_empty() {
        return Ok(0);
    }
    let backend = conn.get_database_backend();
    let table_exists = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'object_index' LIMIT 1"
                .to_string(),
        ))
        .await?
        .is_some();
    if !table_exists {
        return Ok(0);
    }

    // Resolve the repo id exactly like the indexing writer
    // (`resolve_repo_id_for_index`): `libra.repoid` config with an
    // `unknown-repo` fallback — including on lookup failure, mirroring the
    // writer's tolerance so the delete predicate matches the rows it wrote
    // (a wrong fallback merely deletes nothing, which is safe).
    let repo_id = match conn
        .query_one(Statement::from_string(
            backend,
            "SELECT value FROM config_kv WHERE key = 'libra.repoid' ORDER BY id DESC LIMIT 1"
                .to_string(),
        ))
        .await
    {
        Ok(Some(row)) => match row.try_get_by::<String, _>("value") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => "unknown-repo".to_string(),
        },
        Ok(None) => "unknown-repo".to_string(),
        Err(err) => {
            tracing::debug!(
                "failed to resolve repo id for object index cleanup, using fallback: {err}"
            );
            "unknown-repo".to_string()
        }
    };

    // SQLite's default host-parameter limit is generous (32k), but keep the
    // chunks small so a huge prune cannot produce pathological statements.
    const DELETE_CHUNK: usize = 200;
    let mut deleted = 0_u64;
    for chunk in oids.chunks(DELETE_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql =
            format!("DELETE FROM object_index WHERE repo_id = ? AND o_id IN ({placeholders})");
        let mut values: Vec<Value> = Vec::with_capacity(chunk.len() + 1);
        values.push(Value::from(repo_id.clone()));
        values.extend(chunk.iter().map(|oid| Value::from(oid.clone())));
        let result = conn
            .execute(Statement::from_sql_and_values(backend, sql, values))
            .await?;
        deleted += result.rows_affected();
    }
    Ok(deleted)
}

#[async_trait]
impl Storage for ClientStorage {
    async fn get(&self, hash: &ObjectHash) -> Result<(Vec<u8>, ObjectType), GitError> {
        let storage = self.storage.clone();
        let hash = *hash;
        self.block_on_storage(async move { storage.get(&hash).await })
    }

    async fn put(
        &self,
        hash: &ObjectHash,
        data: &[u8],
        obj_type: ObjectType,
    ) -> Result<String, GitError> {
        ClientStorage::put(self, hash, data, obj_type).map_err(GitError::IOError)
    }

    async fn exist(&self, hash: &ObjectHash) -> bool {
        ClientStorage::exist(self, hash)
    }

    async fn search(&self, prefix: &str) -> Vec<ObjectHash> {
        ClientStorage::search(self, prefix).await
    }
}

/// Resolve an environment variable, checking both system env and vault config.
///
/// First checks `std::env::var` (fast, sync). If the system env var is absent,
/// it reuses the async `resolve_env()` path on a dedicated thread so local/global
/// config and vault-backed values share exactly the same semantics.
///
/// This avoids deadlocks from nested tokio runtimes during storage init, which
/// runs synchronously and may be called from within async test contexts.
///
/// Boundary conditions:
/// - Returns `Ok(None)` only when neither the system env nor any config scope
///   contains the value.
/// - Returns `Err(String)` when the worker thread crashes before sending or when
///   the underlying config lookup raises an error (e.g. corrupt SQLite, unreadable
///   permissions). Callers convert this into a hard storage configuration failure
///   rather than silently degrading.
fn resolve_env_sync(name: &str) -> Result<Option<String>, String> {
    resolve_env_sync_typed(name).map_err(|err| err.to_string())
}

fn resolve_env_sync_typed(name: &str) -> Result<Option<String>, StorageConfigResolutionError> {
    // Always check system environment first.
    if let Ok(val) = std::env::var(name) {
        return Ok(Some(val));
    }

    let owned = name.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = resolve_env_sync_worker(&owned);
        let _ = tx.send(result);
    });
    match rx.recv() {
        Ok(result) => result,
        Err(_) => Err(StorageConfigResolutionError::Other(format!(
            "env resolution worker for '{name}' exited before returning a result"
        ))),
    }
}

/// Worker side of [`resolve_env_sync`]: builds a single-purpose tokio runtime in a
/// dedicated thread so we can drive the async config lookup without colliding with
/// any runtime the caller already owns.
fn resolve_env_sync_worker(name: &str) -> Result<Option<String>, StorageConfigResolutionError> {
    let runtime = tokio::runtime::Runtime::new().map_err(|err| {
        StorageConfigResolutionError::Other(format!(
            "failed to create tokio runtime for env resolution of '{name}': {err}"
        ))
    })?;
    runtime.block_on(resolve_env_for_storage_init_typed(name))
}

/// Look up `name` in the local repo's config first, then in the global config.
///
/// Functional scope:
/// - Reads `vault.env.<name>` from `<repo>/.libra/<DATABASE>` if it exists, then from
///   the global config (overridable via `LIBRA_CONFIG_GLOBAL_DB`).
///
/// Boundary conditions:
/// - Returns `Ok(None)` when neither database holds the key.
/// - Returns `Err` when a database file exists but cannot be opened or queried — the
///   caller surfaces this so the user sees actionable errors rather than silently
///   degrading to local-only storage on a typo'd schema.
async fn resolve_env_for_storage_init_typed(
    name: &str,
) -> Result<Option<String>, StorageConfigResolutionError> {
    if let Some(value) = resolve_env_for_storage_init_without_global(name).await? {
        return Ok(Some(value));
    }

    let vault_key = format!("vault.env.{name}");

    if let Some(global_db_path) = storage_global_config_path()
        && global_db_path.exists()
    {
        if let Some(future) = inspect_global_config_schema_future_at_path(&global_db_path).await {
            return Err(StorageConfigResolutionError::GlobalSchemaFuture(future));
        }
        match read_config_env_value(name, &vault_key, &global_db_path, "global")
            .await
            .map_err(StorageConfigResolutionError::Other)
        {
            Ok(Some(value)) => return Ok(Some(value)),
            Ok(None) => {}
            Err(err) => return Err(err),
        }
    }

    Ok(None)
}

async fn resolve_env_for_storage_init_without_global(
    name: &str,
) -> Result<Option<String>, StorageConfigResolutionError> {
    if let Ok(val) = std::env::var(name) {
        return Ok(Some(val));
    }

    let vault_key = format!("vault.env.{name}");

    if let Ok(storage_path) = try_get_storage_path(None) {
        let local_db_path = storage_path.join(DATABASE);
        if local_db_path.exists()
            && let Some(value) = read_config_env_value(name, &vault_key, &local_db_path, "local")
                .await
                .map_err(StorageConfigResolutionError::Other)?
        {
            return Ok(Some(value));
        }
    }

    Ok(None)
}

/// Inspect the configured global config DB and return only the too-new-schema
/// case. Other config errors are left to the normal resolver path so commands
/// that never touch global storage config keep their historical behavior.
pub async fn inspect_global_config_schema_future() -> Option<GlobalConfigSchemaFuture> {
    let global_db_path = storage_global_config_path()?;
    if !global_db_path.exists() {
        return None;
    }
    inspect_global_config_schema_future_at_path(&global_db_path).await
}

async fn inspect_global_config_schema_future_at_path(
    global_db_path: &Path,
) -> Option<GlobalConfigSchemaFuture> {
    match db::inspect_database_schema(global_db_path).await {
        Ok(db::SchemaCompatibility::UnsupportedFuture {
            current_version,
            latest_version,
        }) => Some(GlobalConfigSchemaFuture {
            db_path: global_db_path.to_path_buf(),
            current_version,
            latest_version,
        }),
        Ok(_) | Err(_) => None,
    }
}

/// Read a single `vault.env.*` entry from a config database, decrypting if needed.
///
/// Functional scope:
/// - Connects with a 200 ms busy timeout so background storage init cannot block on
///   foreground writers.
/// - When the entry is encrypted, decrypts using the per-scope key (local repo key
///   or global key).
///
/// Boundary conditions:
/// - Returns `Err` when the database path is not valid UTF-8 (sea-orm needs a
///   string-typed URL).
/// - Returns `Err` when decryption fails — the user sees the raw vault error, not a
///   silent fall-back to plaintext.
async fn read_config_env_value(
    env_name: &str,
    vault_key: &str,
    db_path: &Path,
    scope: &str,
) -> Result<Option<String>, String> {
    let db_path_str = db_path.to_str().ok_or_else(|| {
        format!(
            "database path is not valid UTF-8 for {scope} config: {}",
            db_path.display()
        )
    })?;
    let conn = establish_connection_with_busy_timeout(db_path_str, Duration::from_millis(200))
        .await
        .map_err(|err| match scope {
            "global" => format!(
                "failed to connect to global config '{}': {}",
                db_path.display(),
                err
            ),
            _ => format!(
                "failed to connect to local config '{}': {}",
                db_path.display(),
                err
            ),
        })?;

    let entry = ConfigKv::get_with_conn(&conn, vault_key)
        .await
        .map_err(|err| format!("failed to read '{env_name}' from {scope} config: {err}"))?;

    match entry {
        Some(entry) if entry.encrypted => decrypt_value(&entry.value, scope)
            .await
            .map(Some)
            .map_err(|err| {
                if scope == "global" {
                    format!("failed to decrypt vault.env.{env_name} from global config: {err}")
                } else {
                    format!("failed to decrypt vault.env.{env_name}: {err}")
                }
            }),
        Some(entry) => Ok(Some(entry.value)),
        None => Ok(None),
    }
}

/// Locate the global config database.
///
/// Boundary conditions:
/// - Honours `LIBRA_CONFIG_GLOBAL_DB` first so tests can redirect to a temp path.
/// - Returns `None` when no home directory is discoverable; on those platforms global
///   config is unavailable.
fn storage_global_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LIBRA_CONFIG_GLOBAL_DB") {
        return Some(PathBuf::from(path));
    }
    dirs::home_dir().map(|home| home.join(".libra").join("config.db"))
}

/// Resolve (and lazily create) the per-repo `libra.repoid` used as a key prefix in
/// shared S3/R2 buckets.
///
/// Functional scope:
/// - Reads `libra.repoid` from the local config; if missing or set to the legacy
///   placeholder `"unknown-repo"`, generates a fresh UUID and persists it so future
///   invocations stay aligned with the same prefix.
///
/// Boundary conditions:
/// - Returns `None` if there is no resolvable storage path or no database file yet
///   (`libra init` has not run); the caller falls back to no prefix in that case.
/// - The whole computation runs on RUNTIME via mpsc, mirroring the rest of this
///   module's blocking-into-async pattern.
fn get_or_create_repo_id_for_prefix() -> Option<String> {
    let storage_path = try_get_storage_path(None).ok()?;
    let db_path = storage_path.join(DATABASE);
    if !db_path.exists() {
        return None;
    }

    let (tx, rx) = mpsc::channel();
    RUNTIME.spawn(async move {
        let mut repo_id = ConfigKv::get("libra.repoid")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        let needs_init = repo_id
            .as_deref()
            .map(|s| s.is_empty() || s == "unknown-repo")
            .unwrap_or(true);
        if needs_init {
            let new_id = Uuid::new_v4().to_string();
            let _ = ConfigKv::set("libra.repoid", &new_id, false).await;
            repo_id = Some(new_id);
        }
        let _ = tx.send(repo_id);
    });

    rx.recv().ok().flatten()
}

/// Resolve repository ID for object-index rows.
///
/// Best effort only: if config cannot be read (e.g. temp repo already removed),
/// use a stable fallback to avoid panicking background tasks.
///
/// Boundary conditions:
/// - Returns the literal string `"unknown-repo"` when the entry is missing, blank,
///   or unreadable. This sentinel is also recognised by `get_or_create_repo_id_for_prefix`
///   as a placeholder that should be re-rolled on first use.
async fn resolve_repo_id_for_index(db_conn: &DatabaseConnection) -> String {
    match ConfigKv::get_with_conn(db_conn, "libra.repoid").await {
        Ok(Some(entry)) if !entry.value.trim().is_empty() => entry.value,
        Ok(_) => "unknown-repo".to_string(),
        Err(err) => {
            tracing::debug!("Failed to resolve repo id for object index update: {}", err);
            "unknown-repo".to_string()
        }
    }
}

/// Insert (or no-op) an entry in the `object_index` table with bounded retries on
/// transient failures.
///
/// Functional scope:
/// - Calls [`update_object_index_once`] up to [`INDEX_UPDATE_MAX_ATTEMPTS`] times.
///   SQLite locking errors are normally transient because object writes race with
///   foreground commit/reference updates; dropping the row would make `cloud sync`
///   upload an incomplete object graph.
///
/// Boundary conditions:
/// - Returns `Ok(())` (without retry) when the database file disappears between
///   attempts, which happens in test cleanup.
async fn update_object_index(
    db_path: &Path,
    o_id: &str,
    o_type: &str,
    o_size: i64,
) -> Result<(), String> {
    let mut last_err = None;

    for attempt in 1..=INDEX_UPDATE_MAX_ATTEMPTS {
        match update_object_index_once(db_path, o_id, o_type, o_size).await {
            Ok(()) => return Ok(()),
            Err(_err) if !db_path.exists() => return Ok(()),
            Err(err) => {
                if attempt == INDEX_UPDATE_MAX_ATTEMPTS {
                    last_err = Some(err);
                    break;
                }

                tracing::debug!(
                    db_path = %db_path.display(),
                    object_id = o_id,
                    attempt,
                    max_attempts = INDEX_UPDATE_MAX_ATTEMPTS,
                    error = %err,
                    "Retrying object index update after transient failure"
                );
                let delay_ms = 100 * attempt as u64;
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| "object index update failed".to_string()))
}

/// Update `object_index` for cloud backup tracking — single attempt.
///
/// Functional scope:
/// - Skips entirely when the database file is absent (e.g. temp repo torn down).
/// - Looks up the existing `(o_id, repo_id)` row; inserts only when missing.
/// - Uses a short 200 ms busy timeout so foreground commit/reflog/etc. operations
///   are not blocked by indexing. The outer retry loop gives longer lock windows
///   time to clear without holding contention for a full second at a time.
///
/// Boundary conditions:
/// - Returns `Ok(())` for any error encountered after the database file disappears
///   (test teardown), preventing spurious failures from racing tempdirs.
/// - Returns `Err` when the database path is not valid UTF-8 — sea-orm requires a
///   string URL.
/// - See: `update_object_index_skips_missing_database_without_error`.
async fn update_object_index_once(
    db_path: &Path,
    o_id: &str,
    o_type: &str,
    o_size: i64,
) -> Result<(), String> {
    if !db_path.exists() {
        return Ok(());
    }

    let db_path_str = db_path.to_str().ok_or_else(|| {
        format!(
            "database path is not valid UTF-8 for object index update: {}",
            db_path.display()
        )
    })?;

    // Background indexing is best-effort but must not lose rows during ordinary
    // commit-time SQLite lock windows; the outer retry loop handles longer locks.
    let db_conn =
        match db::establish_connection_with_busy_timeout(db_path_str, Duration::from_millis(200))
            .await
        {
            Ok(conn) => conn,
            Err(err) if err.kind() == io::ErrorKind::NotFound || !db_path.exists() => return Ok(()),
            Err(err) => {
                return Err(format!(
                    "Failed to connect to object index database {}: {}",
                    db_path.display(),
                    err
                ));
            }
        };

    let repo_id = resolve_repo_id_for_index(&db_conn).await;
    let created_at = chrono::Utc::now().timestamp();

    // Check if object already exists
    // With multi-repo support, we must check (o_id, repo_id)
    use sea_orm::{ActiveModelTrait, Set};
    let existing = object_index::Entity::find()
        .filter(object_index::Column::OId.eq(o_id))
        .filter(object_index::Column::RepoId.eq(&repo_id))
        .one(&db_conn)
        .await;

    let existing = match existing {
        Ok(existing) => existing,
        Err(err) => {
            if !db_path.exists() {
                return Ok(());
            }
            return Err(format!("Database query failed: {}", err));
        }
    };

    if let Some(existing_row) = existing {
        // Phase 3.5c codex review: a row may already exist with the
        // generic `blob` tag (written by the standard storage path)
        // before the agent capture runtime calls back with a more
        // specific `agent_transcript` tag for the same content-addressed
        // OID. Without this upgrade the `agent_transcript` tag would be
        // silently dropped — first-writer-wins — and downstream tooling
        // that filters by o_type would never see the captured
        // transcripts. We promote a generic tag to the agent-specific
        // one but never demote in the other direction (a row already
        // tagged `agent_transcript` is left alone).
        if existing_row.o_type != o_type
            && o_type.starts_with("agent_")
            && !existing_row.o_type.starts_with("agent_")
        {
            let mut active: object_index::ActiveModel = existing_row.into();
            active.o_type = Set(o_type.to_string());
            if let Err(err) = active.update(&db_conn).await {
                if !db_path.exists() {
                    return Ok(());
                }
                return Err(format!("Failed to upgrade object_index o_type: {}", err));
            }
        }
        return Ok(());
    }

    // Insert new object index entry
    let entry = object_index::ActiveModel {
        o_id: Set(o_id.to_string()),
        o_type: Set(o_type.to_string()),
        o_size: Set(o_size),
        repo_id: Set(repo_id),
        created_at: Set(created_at),
        is_synced: Set(0), // Not synced to cloud yet
        ..Default::default()
    };

    if let Err(err) = entry.insert(&db_conn).await {
        if !db_path.exists() {
            return Ok(());
        }
        return Err(format!("Failed to insert object index: {}", err));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use git_internal::{
        errors::GitError,
        hash::{HashKind, get_hash_kind, set_hash_kind, set_hash_kind_for_test},
        internal::{
            metadata::{EntryMeta, MetaAttached},
            object::{ObjectTrait, blob::Blob},
            pack::{encode::PackEncoder, entry::Entry},
        },
    };
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use serial_test::serial;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::{ClientStorage, resolve_env_sync, update_object_index, update_object_index_once};
    use crate::{
        internal::{
            config::ConfigKv,
            db,
            model::{object_index, reference},
        },
        utils::test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in},
    };

    /// Test helper that clears an env var on construction and restores it on drop.
    /// Combined with `#[serial]`, this lets tests assert behaviour when a specific
    /// env var is unset without leaking state into sibling tests.
    struct ClearedEnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl ClearedEnvVarGuard {
        fn new(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: these tests are `#[serial]`, so process env mutation is isolated.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for ClearedEnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: this restores the exact previous value for the same process env key.
            unsafe {
                if let Some(value) = &self.previous {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    // Helper to build packs (copied from previous version for tests)
    async fn encode_entries_to_pack_bytes(entries: Vec<Entry>) -> Result<Vec<u8>, GitError> {
        assert!(!entries.is_empty(), "encode requires at least one entry");
        let (pack_tx, mut pack_rx) = mpsc::channel::<Vec<u8>>(128);
        let (entry_tx, entry_rx) = mpsc::channel::<MetaAttached<Entry, EntryMeta>>(entries.len());
        let mut encoder = PackEncoder::new(entries.len(), 0, pack_tx);
        let kind = get_hash_kind();
        let encode_handle = tokio::spawn(async move {
            set_hash_kind(kind);
            encoder.encode(entry_rx).await
        });

        for entry in entries {
            entry_tx
                .send(MetaAttached {
                    inner: entry,
                    meta: EntryMeta::new(),
                })
                .await
                .map_err(|e| GitError::PackEncodeError(format!("send entry failed: {e}")))?;
        }
        drop(entry_tx);

        let mut pack_bytes = Vec::new();
        while let Some(chunk) = pack_rx.recv().await {
            pack_bytes.extend_from_slice(&chunk);
        }

        let encode_result = encode_handle
            .await
            .map_err(|e| GitError::PackEncodeError(format!("pack encoder task join error: {e}")))?;
        encode_result?;
        Ok(pack_bytes)
    }

    fn build_pack_bytes(entries: Vec<Entry>) -> Result<Vec<u8>, GitError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(encode_entries_to_pack_bytes(entries))
    }

    fn write_pack_to_objects(
        pack_bytes: &[u8],
        label: &str,
    ) -> Result<(tempfile::TempDir, PathBuf, PathBuf), GitError> {
        let dir = tempdir()?;
        let objects_dir = dir.path().join("objects");
        let pack_dir = objects_dir.join("pack");
        fs::create_dir_all(&pack_dir)?;
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pack_path = pack_dir.join(format!("client-storage-{label}-{unique}.pack"));
        fs::write(&pack_path, pack_bytes)?;
        Ok((dir, objects_dir, pack_path))
    }

    /// Scenario: a freshly-built SHA-1 pack must be readable through `ClientStorage`
    /// without the caller having touched any database. Guards the pack-reading code
    /// path that `clone`/`fetch` rely on so they can fall back to packs when the
    /// loose-object directory is absent.
    #[test]
    #[serial]
    fn client_storage_reads_pack_sha1() -> Result<(), GitError> {
        let _guard = set_hash_kind_for_test(HashKind::Sha1);
        let blob = Blob::from_content("client-storage-sha1");
        let pack_bytes = build_pack_bytes(vec![Entry::from(blob.clone())])?;
        let (_tmp, objects_dir, _) = write_pack_to_objects(&pack_bytes, "sha1")?;

        let storage = ClientStorage::init(objects_dir);
        let data = storage.get(&blob.id)?;
        assert_eq!(data, blob.data);
        Ok(())
    }

    /// Scenario: parallel test for SHA-256 pack reading. SHA-256 has a different
    /// header layout and crc table; this test pins backwards/forwards compatibility
    /// for repositories created with `core.objectformat=sha256`.
    #[test]
    #[serial]
    fn client_storage_reads_pack_sha256() -> Result<(), GitError> {
        let _guard = set_hash_kind_for_test(HashKind::Sha256);
        let blob = Blob::from_content("client-storage-sha256");
        let pack_bytes = build_pack_bytes(vec![Entry::from(blob.clone())])?;
        let (_tmp, objects_dir, _) = write_pack_to_objects(&pack_bytes, "sha256")?;

        let storage = ClientStorage::init(objects_dir);
        let data = storage.get(&blob.id)?;
        assert_eq!(data, blob.data);
        Ok(())
    }

    /// Scenario: round-trip a blob through `put`/`exist`/`get`. This is the smallest
    /// possible regression check that the synchronous facade and its blocking-on-
    /// runtime bridge are wired up correctly.
    #[test]
    #[serial]
    fn test_content_store() {
        let content = "Hello, world!";
        let blob = Blob::from_content(content);

        let _tmp = tempdir().unwrap();
        let source = _tmp.path().join("objects");

        let client_storage = ClientStorage::init(source.clone());
        assert!(
            client_storage
                .put(&blob.id, &blob.data, blob.get_type())
                .is_ok()
        );
        assert!(client_storage.exist(&blob.id));

        let data = client_storage.get(&blob.id).unwrap();
        assert_eq!(data, blob.data);
        assert_eq!(String::from_utf8(data).unwrap(), content);
    }

    /// Scenario: searching for a freshly-stored object by its full hash must return a
    /// non-empty result. This guards the storage-search wiring that downstream
    /// commands (`cat-file`, `rev-parse`) rely on for hash resolution.
    #[tokio::test]
    async fn test_search() {
        let blob = Blob::from_content("Hello, world!");

        let _tmp = tempdir().unwrap();
        let source = _tmp.path().join("objects");

        let client_storage = ClientStorage::init(source.clone());
        assert!(
            client_storage
                .put(&blob.id, &blob.data, blob.get_type())
                .is_ok()
        );

        // Search by full hash should return it
        let objs = client_storage.search(&blob.id.to_string()).await;
        assert!(!objs.is_empty());
    }

    /// Scenario: when a branch row exists but its `commit` column is not a valid
    /// hash, navigation like `main~1` must surface a fatal error rather than
    /// silently returning an empty match list. This protects users from acting on
    /// stale or corrupt references without realising it.
    #[tokio::test]
    #[serial]
    async fn test_search_result_surfaces_corrupt_branch_storage() {
        let repo = tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let db_conn = db::get_db_conn_instance().await;
        reference::ActiveModel {
            name: Set(Some("main".to_string())),
            kind: Set(reference::ConfigKind::Branch),
            commit: Set(Some("not-a-valid-hash".to_string())),
            remote: Set(None),
            ..Default::default()
        }
        .insert(&db_conn)
        .await
        .unwrap();

        let storage = ClientStorage::init(crate::utils::path::objects());
        let error = storage
            .search_result("main~1")
            .await
            .expect_err("corrupt branch storage should be surfaced");
        assert!(
            error
                .to_string()
                .contains("stored branch reference 'main' is corrupt"),
            "unexpected error: {error}"
        );
    }

    /// Scenario: input like `~1` or `^2` has no base ref. Without a guard, those
    /// would degenerate into a prefix search of the empty string, returning every
    /// object in the repository. The test verifies that we instead return an empty
    /// vector — the safe behaviour for invalid navigation requests.
    #[tokio::test]
    #[serial]
    async fn test_search_result_rejects_empty_base_ref_navigation() {
        let repo = tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let storage = ClientStorage::init(crate::utils::path::objects());
        assert!(
            storage
                .search_result("~1")
                .await
                .expect("empty-base ~ navigation should not error")
                .is_empty()
        );
        assert!(
            storage
                .search_result("^2")
                .await
                .expect("empty-base ^ navigation should not error")
                .is_empty()
        );
    }

    /// Scenario: zlib compress then decompress must yield the original bytes
    /// verbatim. Pins the compression helpers used by the loose-object writer so a
    /// crate upgrade cannot silently change the round-trip.
    #[test]
    fn test_decompress() {
        let data = b"blob 13\0Hello, world!";
        let compressed_data = ClientStorage::compress_zlib(data).unwrap();
        let decompressed_data = ClientStorage::decompress_zlib(&compressed_data).unwrap();
        assert_eq!(decompressed_data, data);
    }

    /// Scenario: `put` should write its index update to the database that owns the
    /// objects directory it just wrote into, *not* whichever database is reachable
    /// from the process CWD. Regression guard for a bug where two repositories sharing
    /// a CWD could cross-pollinate their object indexes.
    #[tokio::test]
    #[serial]
    async fn background_index_update_uses_storage_database_instead_of_cwd() {
        let workspace = tempdir().unwrap();
        let storage_path = workspace.path().join(".libra");
        fs::create_dir_all(&storage_path).unwrap();
        let objects_dir = storage_path.join("objects");
        fs::create_dir_all(&objects_dir).unwrap();

        let db_path = storage_path.join(crate::utils::util::DATABASE);
        let db_conn = db::create_database(db_path.to_str().unwrap())
            .await
            .unwrap();
        let _ = ConfigKv::set_with_conn(&db_conn, "libra.repoid", "repo-from-storage", false).await;

        // CWD must be the workspace so `try_get_storage_path` can find `.libra/`.
        let _guard = ChangeDirGuard::new(workspace.path());

        let blob = Blob::from_content("index from explicit storage db");
        let storage = ClientStorage::init(objects_dir);
        storage.put(&blob.id, &blob.data, blob.get_type()).unwrap();
        ClientStorage::wait_for_background_tasks();

        let row = object_index::Entity::find()
            .filter(object_index::Column::OId.eq(blob.id.to_string()))
            .filter(object_index::Column::RepoId.eq("repo-from-storage"))
            .one(&db_conn)
            .await
            .unwrap();
        assert!(row.is_some());
    }

    /// Scenario: index updates must tolerate a missing database file rather than
    /// returning an error and triggering retry storms. This is common during test
    /// teardown when a temp repo is removed before its background index task drains.
    #[tokio::test]
    #[serial]
    async fn update_object_index_skips_missing_database_without_error() {
        let missing_root = tempdir().unwrap();
        let missing_db = missing_root.path().join(crate::utils::util::DATABASE);

        let result = update_object_index(&missing_db, "deadbeef", "blob", 12).await;
        assert!(result.is_ok());
    }

    /// Phase 3.5c codex round-2 follow-up: a row written first by the
    /// standard storage path with `o_type='blob'` must be UPGRADED to
    /// the agent-specific tag (`agent_transcript`) when the agent
    /// capture call back arrives for the same content-addressed OID.
    /// This is the regression case the round-1 review flagged: a naive
    /// "skip if exists" silently kept the generic tag and downstream
    /// tooling that filtered by o_type lost visibility on captured
    /// transcripts. We exercise the upgrade branch directly here.
    #[tokio::test]
    #[serial]
    async fn update_object_index_upgrades_generic_blob_to_agent_specific_o_type() {
        use sea_orm::{ConnectionTrait, Statement};

        let tmp = tempdir().unwrap();
        let db_path = tmp.path().join(crate::utils::util::DATABASE);
        let conn = db::create_database(db_path.to_str().expect("test database path is UTF-8"))
            .await
            .expect("create database");

        // Seed a generic-blob row that mimics the standard storage path.
        // The repo_id matches the sentinel returned by
        // `resolve_repo_id_for_index` when `libra.repoid` is absent. That
        // keeps the seeded row aligned with what `update_object_index_once`
        // queries while preserving the full current schema contract.
        const OID: &str = "abcdef1234567890abcdef1234567890abcdef12";
        let backend = conn.get_database_backend();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) \
             VALUES (?, 'blob', 42, 'unknown-repo', 0, 0)",
            [OID.into()],
        ))
        .await
        .unwrap();

        // First call: agent-specific tag arrives. The row must be
        // promoted in place; o_id stays unique.
        update_object_index_once(&db_path, OID, "agent_transcript", 42)
            .await
            .expect("upgrade ok");

        let row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT o_type FROM object_index WHERE o_id = ? LIMIT 1",
                [OID.into()],
            ))
            .await
            .unwrap()
            .expect("row exists");
        assert_eq!(
            row.try_get_by::<String, _>("o_type").unwrap(),
            "agent_transcript",
            "blob row must upgrade to agent_transcript"
        );

        // Second call: a *generic* tag arrives for an OID that is
        // already agent-specific. The row must NOT demote — that would
        // strip the spec-mandated tag from the catalogue.
        update_object_index_once(&db_path, OID, "blob", 42)
            .await
            .expect("no-op ok");
        let row_again = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT o_type FROM object_index WHERE o_id = ? LIMIT 1",
                [OID.into()],
            ))
            .await
            .unwrap()
            .expect("row still exists");
        assert_eq!(
            row_again.try_get_by::<String, _>("o_type").unwrap(),
            "agent_transcript",
            "no demotion: agent_transcript stays sticky"
        );

        // Third call: same agent tag, same OID — idempotent no-op.
        update_object_index_once(&db_path, OID, "agent_transcript", 42)
            .await
            .expect("idempotent ok");

        let count_row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT COUNT(*) AS n FROM object_index WHERE o_id = ?",
                [OID.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        let count: i64 = count_row.try_get_by("n").unwrap();
        assert_eq!(count, 1, "single row preserved through upgrade + no-op");
    }

    /// Scenario: when the system environment variable is unset, `resolve_env_sync`
    /// must consult the repository's `vault.env.*` config entries. This is the
    /// primary mechanism users rely on to keep storage credentials inside the
    /// repository config rather than in their shell rc.
    #[test]
    #[serial]
    fn resolve_env_sync_reads_non_allowlisted_local_config_values() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _guard = ChangeDirGuard::new(repo.path());
        let _endpoint = ClearedEnvVarGuard::new("LIBRA_STORAGE_ENDPOINT");

        rt.block_on(async {
            ConfigKv::set(
                "vault.env.LIBRA_STORAGE_ENDPOINT",
                "https://storage.example.com",
                false,
            )
            .await
            .unwrap();
        });

        let value = resolve_env_sync("LIBRA_STORAGE_ENDPOINT").unwrap();
        assert_eq!(value.as_deref(), Some("https://storage.example.com"));
    }

    /// Scenario: a corrupt global config file must propagate a fatal error rather
    /// than silently ignoring the global-scope value. Without this guard, an
    /// invalid global config would silently degrade remote storage to local-only
    /// without telling the user anything is wrong.
    #[test]
    #[serial]
    fn resolve_env_sync_surfaces_global_config_connection_errors() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _guard = ChangeDirGuard::new(repo.path());
        let _threshold = ClearedEnvVarGuard::new("LIBRA_STORAGE_THRESHOLD");

        let bad_global_dir = tempdir().unwrap();
        let bad_global_db = bad_global_dir.path().join("bad-global.db");
        fs::write(&bad_global_db, "not sqlite").unwrap();
        let _global_db = ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &bad_global_db);

        let err = resolve_env_sync("LIBRA_STORAGE_THRESHOLD")
            .expect_err("global config connection failure should surface");
        assert!(
            err.contains("failed to connect to global config"),
            "unexpected error: {err}"
        );
    }
}
