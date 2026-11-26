//! Availability projection for status surfaces (Splash/Welcome/About/
//! Capability map/Search). Derived from the window snapshot; the early probe
//! performs only bounded metadata/database checks.

#[cfg(feature = "storage-redb")]
use redb::{ReadableDatabase as _, ReadableTable as _};
#[cfg(feature = "storage-sqlite")]
use sea_orm::ConnectionTrait;

use super::{
    snapshot::{DataError, Snapshot},
    state::{self, SourceState},
};
#[cfg(feature = "storage-redb")]
use crate::storage::{CaptureStore, ConfigStore};
use crate::storage::{StorageBackendError, StorageBackendKind};

/// One data-surface availability row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAvailability {
    pub name: &'static str,
    pub state: SourceState,
    pub label: &'static str,
}

/// Repository and data-surface availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    pub repo: SourceState,
    pub repo_label: String,
    pub branch: String,
    pub head: String,
    pub object_format: String,
    pub sources: Vec<SourceAvailability>,
}

impl Availability {
    /// The placeholder state before the first snapshot arrives.
    pub fn unchecked() -> Self {
        Self {
            repo: SourceState::Unchecked,
            repo_label: "未检查".to_string(),
            branch: "—".to_string(),
            head: "—".to_string(),
            object_format: "—".to_string(),
            sources: SOURCE_NAMES
                .iter()
                .map(|name| SourceAvailability {
                    name,
                    state: SourceState::Unchecked,
                    label: "未检查",
                })
                .collect(),
        }
    }
}

const SOURCE_NAMES: [&str; 5] = ["capture", "operation", "memory", "runs", "history"];

/// A bounded availability probe: metadata plus short database/schema checks
/// (200 ms busy timeout each) so status surfaces get a real verdict quickly.
pub async fn probe_availability(
    scope: Option<&crate::internal::worktree_scope::RequestScope>,
) -> Availability {
    let Some(scope) = scope else {
        // No repository: every surface is absent (never 未检查).
        let mut availability = Availability::unchecked();
        availability.repo = SourceState::Absent;
        availability.repo_label = "no repository".to_string();
        for source in &mut availability.sources {
            source.state = SourceState::Absent;
            source.label = "absent";
        }
        return availability;
    };
    let runs = scope.storage.join("sessions").join("agent-runs");
    let budget = std::time::Duration::from_millis(200);

    let repo_state = probe_repository_database(&scope.storage, budget).await;
    let capture_state = probe_capture_database(&scope.storage, budget).await;
    let runs_state = probe_directory(&runs);
    let sources = vec![
        source("capture", capture_state),
        source("operation", repo_state.clone()),
        source("memory", repo_state.clone()),
        source("runs", runs_state),
        source("history", repo_state.clone()),
    ];
    Availability {
        repo: repo_state,
        repo_label: scope
            .worktree_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repository".to_string()),
        branch: "—".to_string(),
        head: "—".to_string(),
        object_format: "—".to_string(),
        sources,
    }
}

/// Probe a repository database with bounded open/schema checks.
async fn probe_repository_database(
    storage: &std::path::Path,
    budget: std::time::Duration,
) -> SourceState {
    let backend = match crate::storage::runtime::detect_repo_backend(storage) {
        Ok(backend) => backend,
        Err(error) => return state::classify_storage_error(&error),
    };
    match backend {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                probe_repository_database_redb(storage, budget).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                let _ = budget;
                SourceState::Corrupt
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let path = storage.join(crate::utils::util::DATABASE);
                match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_file() => {}
                    Ok(_) => {
                        return SourceState::Unreadable {
                            kind: std::io::ErrorKind::Other,
                        };
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return SourceState::Absent;
                    }
                    Err(error) => {
                        return SourceState::Unreadable { kind: error.kind() };
                    }
                }
                if let Err(error) = std::fs::File::open(&path) {
                    return SourceState::Unreadable { kind: error.kind() };
                }
                let Some(path_str) = path.to_str() else {
                    return SourceState::Corrupt;
                };
                let conn = match tokio::time::timeout(
                    budget,
                    crate::internal::db::open_connection_without_schema_management(
                        path_str, budget,
                    ),
                )
                .await
                {
                    Ok(Ok(conn)) => conn,
                    Ok(Err(error)) => return state::classify_open_error(&error),
                    Err(_) => return SourceState::Busy,
                };
                let schema_state = match crate::internal::db::schema::inspect_schema_for_connection(
                    &conn,
                    crate::internal::db::schema::DatabaseRole::Repository,
                )
                .await
                {
                    Ok(compatibility) => state::classify_schema(&compatibility),
                    Err(error) => state::classify_store_message(&error.to_string()),
                };
                conn.close().await.ok();
                schema_state
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                let _ = budget;
                SourceState::Corrupt
            }
        }
    }
}

/// Probe the capture catalog (open_readonly expects the STORAGE directory).
async fn probe_capture_database(
    storage: &std::path::Path,
    budget: std::time::Duration,
) -> SourceState {
    let backend = match crate::storage::runtime::detect_repo_backend(storage) {
        Ok(backend) => backend,
        Err(error) => return state::classify_storage_error(&error),
    };
    match backend {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                probe_capture_database_redb(storage, budget).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                let _ = budget;
                SourceState::Corrupt
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let path = crate::internal::ai::capture::store::capture_database_path(storage);
                match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_file() => {}
                    Ok(_) => {
                        return SourceState::Unreadable {
                            kind: std::io::ErrorKind::Other,
                        };
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return SourceState::Absent;
                    }
                    Err(error) => {
                        return SourceState::Unreadable { kind: error.kind() };
                    }
                }
                if let Err(error) = std::fs::File::open(&path) {
                    return SourceState::Unreadable { kind: error.kind() };
                }
                // Bound connection establishment too (the busy timeout alone does not).
                let conn = match tokio::time::timeout(
                    budget,
                    crate::internal::ai::capture::store::open_readonly(storage, budget),
                )
                .await
                {
                    Ok(Ok(conn)) => conn,
                    Ok(Err(error)) => return state::classify_open_error(&error),
                    Err(_) => return SourceState::Busy,
                };
                let schema_state = match conn
                    .query_one_raw(sea_orm::Statement::from_string(
                        conn.get_database_backend(),
                        "SELECT MAX(version) AS version FROM schema_versions".to_string(),
                    ))
                    .await
                {
                    Ok(Some(row)) => match row.try_get::<i64>("", "version").ok() {
                        Some(version)
                            if version < crate::internal::db::schema::CAPTURE_SCHEMA_VERSION =>
                        {
                            SourceState::SchemaOld
                        }
                        Some(version)
                            if version > crate::internal::db::schema::CAPTURE_SCHEMA_VERSION =>
                        {
                            SourceState::SchemaNew
                        }
                        Some(_) => SourceState::Available,
                        None => SourceState::Corrupt,
                    },
                    Ok(None) => SourceState::Corrupt,
                    Err(error) => state::classify_store_message(&error.to_string()),
                };
                conn.close().await.ok();
                schema_state
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                let _ = budget;
                SourceState::Corrupt
            }
        }
    }
}

#[cfg(feature = "storage-redb")]
async fn probe_repository_database_redb(
    storage: &std::path::Path,
    budget: std::time::Duration,
) -> SourceState {
    let path = match crate::storage::runtime::resolve_repo_git_redb_path(storage) {
        Ok(path) => path,
        Err(error) => return state::classify_storage_error(&error),
    };
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return SourceState::Unreadable {
                kind: std::io::ErrorKind::Other,
            };
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SourceState::Absent,
        Err(error) => return SourceState::Unreadable { kind: error.kind() },
    }
    if let Err(error) = std::fs::File::open(&path) {
        return SourceState::Unreadable { kind: error.kind() };
    }
    match tokio::time::timeout(budget, async move {
        let meta_path = path.clone();
        let meta_state = match tokio::task::spawn_blocking(move || {
            inspect_redb_meta_schema(
                &meta_path,
                crate::internal::db::schema::DatabaseRole::Repository,
            )
        })
        .await
        {
            Ok(Ok(meta_state)) => meta_state,
            Ok(Err(error)) => return error.state,
            Err(_) => return SourceState::Corrupt,
        };
        if !meta_state.is_available() {
            return meta_state;
        }
        let cfg_path = path;
        let config_store = match tokio::task::spawn_blocking(move || {
            crate::storage::redb::RedbConfigStore::open_read_only(
                &cfg_path,
                crate::internal::db::schema::DatabaseRole::Repository,
            )
        })
        .await
        {
            Ok(Ok(store)) => store,
            Ok(Err(error)) => return state::classify_storage_error(&error),
            Err(_) => return SourceState::Corrupt,
        };
        let receipts = match config_store.list_schema_receipts().await {
            Ok(receipts) => receipts,
            Err(error) => return state::classify_storage_error(&error),
        };
        let current = receipts.iter().map(|receipt| receipt.version).max();
        let latest = match crate::internal::db::schema::latest_schema_version_for_role(
            crate::internal::db::schema::DatabaseRole::Repository,
        ) {
            Ok(latest) => latest,
            Err(error) => return state::classify_open_error(&error),
        };
        state::classify_schema(&classify_schema_versions(current, latest))
    })
    .await
    {
        Ok(result) => result,
        Err(_) => SourceState::Busy,
    }
}

#[cfg(feature = "storage-redb")]
async fn probe_capture_database_redb(
    storage: &std::path::Path,
    budget: std::time::Duration,
) -> SourceState {
    let path = match crate::storage::runtime::resolve_repo_capture_redb_path(storage) {
        Ok(path) => path,
        Err(error) => return state::classify_storage_error(&error),
    };
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return SourceState::Unreadable {
                kind: std::io::ErrorKind::Other,
            };
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SourceState::Absent,
        Err(error) => return SourceState::Unreadable { kind: error.kind() },
    }
    if let Err(error) = std::fs::File::open(&path) {
        return SourceState::Unreadable { kind: error.kind() };
    }
    match tokio::time::timeout(budget, async move {
        let meta_path = path.clone();
        let meta_state = match tokio::task::spawn_blocking(move || {
            inspect_redb_meta_schema(
                &meta_path,
                crate::internal::db::schema::DatabaseRole::Capture,
            )
        })
        .await
        {
            Ok(Ok(meta_state)) => meta_state,
            Ok(Err(error)) => return error.state,
            Err(_) => return SourceState::Corrupt,
        };
        if !meta_state.is_available() {
            return meta_state;
        }
        let capture_store = match crate::storage::RedbCaptureStore::open_read_only(&path).await {
            Ok(store) => store,
            Err(error) => return state::classify_storage_error(&error),
        };
        let versions = match capture_store.list_schema_versions().await {
            Ok(versions) => versions,
            Err(error) => return state::classify_storage_error(&error),
        };
        match versions.iter().map(|record| record.version).max() {
            Some(version) if version < crate::internal::db::schema::CAPTURE_SCHEMA_VERSION => {
                SourceState::SchemaOld
            }
            Some(version) if version > crate::internal::db::schema::CAPTURE_SCHEMA_VERSION => {
                SourceState::SchemaNew
            }
            Some(_) => SourceState::Available,
            None => SourceState::Corrupt,
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => SourceState::Busy,
    }
}

#[cfg(feature = "storage-redb")]
pub(super) fn inspect_redb_meta_schema(
    path: &std::path::Path,
    role: crate::internal::db::schema::DatabaseRole,
) -> Result<SourceState, DataError> {
    let db = crate::storage::opener::open_redb_read_only(path, role)
        .map_err(|error| map_storage_error(&error, "the database is unusable"))?;
    let rtx = db.begin_read().map_err(|error| {
        let mapped_state = match &error {
            redb::TransactionError::Storage(storage_err) => {
                classify_redb_storage_error(storage_err)
            }
            redb::TransactionError::WouldBlock => SourceState::Busy,
            _ => SourceState::Corrupt,
        };
        DataError::new(mapped_state, "the database is unusable")
    })?;
    let table = match rtx.open_table(crate::storage::meta::META_SCHEMA_TABLE) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(SourceState::Corrupt),
        Err(redb::TableError::Storage(storage_err)) => {
            return Err(DataError::new(
                classify_redb_storage_error(&storage_err),
                "the schema could not be inspected",
            ));
        }
        Err(_) => return Ok(SourceState::Corrupt),
    };
    let raw = match table.get(crate::storage::meta::META_SCHEMA_SINGLETON_KEY) {
        Ok(Some(raw)) => raw,
        Ok(None) => return Ok(SourceState::Corrupt),
        Err(storage_err) => {
            return Err(DataError::new(
                classify_redb_storage_error(&storage_err),
                "the schema could not be inspected",
            ));
        }
    };
    let record: crate::storage::meta::MetaSchemaRecord =
        match crate::storage::codec::decode_versioned(
            raw.value(),
            crate::storage::meta::META_SCHEMA_CODEC_VERSION,
        ) {
            Ok(record) => record,
            Err(error) => return Ok(state::classify_storage_error(&error)),
        };
    Ok(state::classify_redb_meta_schema(&record, role))
}

#[cfg(feature = "storage-redb")]
fn classify_redb_storage_error(error: &redb::StorageError) -> SourceState {
    match error {
        redb::StorageError::Io(io_err) => match io_err.kind() {
            std::io::ErrorKind::NotFound => SourceState::Absent,
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => SourceState::Busy,
            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::InvalidData => {
                SourceState::Corrupt
            }
            kind => SourceState::Unreadable { kind },
        },
        _ => SourceState::Corrupt,
    }
}

#[cfg(feature = "storage-redb")]
pub(super) fn classify_schema_versions(
    current: Option<i64>,
    latest: Option<i64>,
) -> crate::internal::db::SchemaCompatibility {
    match (current, latest) {
        (Some(current), latest) if latest.is_none_or(|latest| current > latest) => {
            crate::internal::db::SchemaCompatibility::UnsupportedFuture {
                current_version: current,
                latest_version: latest,
            }
        }
        (current, Some(latest)) if current != Some(latest) => {
            crate::internal::db::SchemaCompatibility::UpgradeRequired {
                current_version: current,
                latest_version: latest,
            }
        }
        (current, latest) => crate::internal::db::SchemaCompatibility::Compatible {
            current_version: current,
            latest_version: latest,
        },
    }
}

/// Open the repository `redb` config store strictly read-only after validating
/// filesystem readability, `META_SCHEMA`, and `schema_versions` receipts.
///
/// Returns `Ok(None)` when the repository database is absent.
#[cfg(feature = "storage-redb")]
pub(super) async fn open_redb_repo_config_checked(
    storage: &std::path::Path,
) -> Result<Option<(std::path::PathBuf, crate::storage::RedbConfigStore)>, DataError> {
    let git_db_path = match crate::storage::runtime::resolve_repo_git_redb_path(storage) {
        Ok(path) => path,
        Err(StorageBackendError::NotFound { .. }) => return Ok(None),
        Err(error) => {
            return Err(map_storage_error(
                &error,
                "the repository database path could not be resolved",
            ));
        }
    };
    match state::probe_path(&git_db_path) {
        state::ProbeOutcome::Absent => return Ok(None),
        state::ProbeOutcome::Unreadable { kind } => {
            return Err(DataError::new(
                SourceState::Unreadable { kind },
                "the repository database cannot be opened for reading",
            ));
        }
        state::ProbeOutcome::Present => {}
    }

    let meta_path = git_db_path.clone();
    let meta_state = tokio::task::spawn_blocking(move || {
        inspect_redb_meta_schema(
            &meta_path,
            crate::internal::db::schema::DatabaseRole::Repository,
        )
    })
    .await
    .map_err(|_| {
        DataError::new(
            SourceState::Corrupt,
            "the repository metadata schema inspection task failed",
        )
    })??;
    if !meta_state.is_available() {
        let message = match meta_state {
            SourceState::SchemaNew => "the repository schema is newer than this binary supports",
            SourceState::SchemaOld => "the repository schema is older than this binary supports",
            _ => "the repository schema does not match this binary",
        };
        return Err(DataError::new(meta_state, message));
    }

    let cfg_path = git_db_path.clone();
    let config_store = tokio::task::spawn_blocking(move || {
        crate::storage::redb::RedbConfigStore::open_read_only(
            &cfg_path,
            crate::internal::db::schema::DatabaseRole::Repository,
        )
    })
    .await
    .map_err(|_| {
        DataError::new(
            SourceState::Corrupt,
            "the repository configuration store open task failed",
        )
    })?
    .map_err(|error| map_storage_error(&error, "the repository database is unusable"))?;

    let receipts = config_store.list_schema_receipts().await.map_err(|error| {
        map_storage_error(&error, "the repository schema ledger could not be read")
    })?;
    let current = receipts.iter().map(|receipt| receipt.version).max();
    let latest = crate::internal::db::schema::latest_schema_version_for_role(
        crate::internal::db::schema::DatabaseRole::Repository,
    )
    .map_err(|error| {
        DataError::new(
            state::classify_open_error(&error),
            "the repository schema ledger could not be read",
        )
    })?;
    match state::classify_schema(&classify_schema_versions(current, latest)) {
        SourceState::Available => Ok(Some((git_db_path, config_store))),
        other => Err(DataError::new(
            other,
            "the repository schema does not match this binary",
        )),
    }
}

pub(super) fn map_storage_error(error: &StorageBackendError, default_message: &str) -> DataError {
    let mapped_state = state::classify_storage_error(error);
    let message = match error {
        StorageBackendError::RepairAborted {
            role: crate::internal::db::schema::DatabaseRole::Capture,
            ..
        } => {
            "the capture database requires crash recovery before read-only access; run `libra agent doctor --repair`"
        }
        StorageBackendError::RepairAborted { .. } => {
            "the repository database requires crash recovery before read-only access; run `libra fsck`"
        }
        StorageBackendError::SchemaTooNew { .. }
        | StorageBackendError::UnsupportedSchemaVersion { .. } => {
            "the database schema is newer than this binary supports"
        }
        _ if mapped_state == SourceState::Absent => "this repository has no database yet",
        _ if mapped_state == SourceState::SchemaOld => {
            "the database schema is older than this binary supports"
        }
        _ if matches!(mapped_state, SourceState::Unreadable { .. }) => {
            "the database cannot be opened for reading"
        }
        _ => default_message,
    };
    DataError::new(mapped_state, message)
}

/// Directory existence/type check WITHOUT opening the path (FIFO-safe).
fn probe_directory(path: &std::path::Path) -> SourceState {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => SourceState::Unchecked,
        Ok(_) => SourceState::Unreadable {
            kind: std::io::ErrorKind::NotADirectory,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SourceState::Absent,
        Err(error) => SourceState::Unreadable { kind: error.kind() },
    }
}

/// Build availability from a delivered snapshot (or the unchecked placeholder).
pub fn availability(snapshot: Option<&Snapshot>) -> Availability {
    let Some(snapshot) = snapshot else {
        return Availability::unchecked();
    };
    let context = snapshot.context.as_ref();
    let repo = snapshot.state.clone();
    let sources = vec![
        source("capture", snapshot.sessions_state.clone()),
        source("operation", snapshot.operations.state.clone()),
        source("memory", snapshot.memory.state.clone()),
        source("runs", snapshot.runs.state.clone()),
        source("history", snapshot.history_state.clone()),
    ];
    Availability {
        repo,
        repo_label: context
            .map(|context| context.worktree_label.clone())
            .unwrap_or_else(|| "no repository".to_string()),
        branch: context
            .map(|context| context.branch.clone())
            .unwrap_or_else(|| "—".to_string()),
        head: context
            .map(|context| context.head.clone())
            .unwrap_or_else(|| "—".to_string()),
        object_format: context
            .map(|context| context.object_format.clone())
            .unwrap_or_else(|| "—".to_string()),
        sources,
    }
}

fn label_for(state: &SourceState) -> &'static str {
    match state {
        SourceState::Available => "available",
        SourceState::Absent => "absent",
        SourceState::Unchecked => "未检查",
        SourceState::Stale => "stale",
        SourceState::Busy => "busy",
        SourceState::SchemaOld => "schema old",
        SourceState::SchemaNew => "schema new",
        SourceState::Unreadable { .. } => "unreadable",
        SourceState::Corrupt => "corrupt",
        SourceState::Worker => "worker failed",
    }
}

fn source(name: &'static str, state: SourceState) -> SourceAvailability {
    let label = label_for(&state);
    SourceAvailability { name, state, label }
}

/// Availability when the worktree scope could not be resolved at all: every
/// surface stays Unchecked (never absent).
pub fn unresolved_availability() -> Availability {
    let mut availability = Availability::unchecked();
    availability.repo = SourceState::Unchecked;
    availability.repo_label = "unresolved".to_string();
    availability
}

/// Capability rows for a given availability projection (shared by the dialog
/// and the status pages).
pub fn availability_capabilities(
    availability: &Availability,
) -> Vec<super::super::model::Capability> {
    capabilities(availability)
}

/// Merge a terminal repository failure with independently probed sources:
/// the repository error wins, but probe verdicts for unaffected surfaces stay.
pub fn merge_availability(probe: &Availability, snapshot: &Snapshot) -> Availability {
    let mut merged = probe.clone();
    merged.repo = if snapshot.state.is_available() {
        probe.repo.clone()
    } else {
        snapshot.state.clone()
    };
    for source in &mut merged.sources {
        let final_state = match source.name {
            "capture" => snapshot.sessions_state.clone(),
            "operation" => snapshot.operations.state.clone(),
            "memory" => snapshot.memory.state.clone(),
            "runs" => snapshot.runs.state.clone(),
            _ => snapshot.history_state.clone(),
        };
        if final_state != SourceState::Unchecked {
            source.state = final_state.clone();
            source.label = label_for(&final_state);
        }
    }
    merged
}

/// Capability-map rows for the Sessions surface, backed by real states.
pub fn capabilities(availability: &Availability) -> Vec<super::super::model::Capability> {
    availability
        .sources
        .iter()
        .map(|source| super::super::model::Capability {
            capability: source.name.to_string(),
            basis: "window read".to_string(),
            data: source.label.to_string(),
            missing: if source.state.is_available() {
                "—".to_string()
            } else {
                source.label.to_string()
            },
        })
        .collect()
}
