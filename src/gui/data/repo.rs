//! Repository discovery and context reads for the window.
//!
//! Discovery reuses [`RequestScope`] (never the panicking path helpers), and
//! every database read uses an injected, strictly read-only handle opened
//! without schema management or repair upgrades, so a window read cannot
//! migrate or modify the repository.

use std::{path::PathBuf, time::Duration};

#[cfg(feature = "storage-redb")]
use redb::{ReadableDatabase as _, ReadableTable as _};
#[cfg(feature = "storage-sqlite")]
use sea_orm::TransactionTrait;

use super::{
    snapshot::{DataError, RepoContext},
    state::{self, ProbeOutcome, SourceState},
};
#[cfg(feature = "storage-sqlite")]
use crate::internal::{branch::Branch, db, head::Head};
#[cfg(feature = "storage-redb")]
use crate::storage::{ConfigStore, GitStore, RefKey, RefTarget};
use crate::{
    internal::{db::DatabaseRole, worktree_scope::RequestScope},
    storage::{StorageBackendError, StorageBackendKind},
    utils::util,
};

/// Busy timeout for window reads.
pub const READ_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// Resolve the repository scope from a working directory.
///
/// Returns `Ok(None)` for a directory that is not inside a repository, and a
/// typed error when the worktree is damaged.
pub fn resolve_scope(workdir: PathBuf) -> Result<Option<RequestScope>, DataError> {
    match RequestScope::try_resolve(workdir) {
        Ok(scope) => Ok(scope),
        Err(error) => Err(DataError::new(
            SourceState::Unreadable { kind: error.kind() },
            format!(
                "the worktree layout could not be resolved ({:?})",
                error.kind()
            ),
        )),
    }
}

/// Read the repository context through a strictly read-only, non-migrating handle.
pub async fn read_context(scope: &RequestScope) -> Result<RepoContext, DataError> {
    match crate::storage::runtime::detect_repo_backend(&scope.storage).map_err(|error| {
        map_storage_error(&error, "the repository backend could not be detected")
    })? {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                read_context_redb(scope).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(DataError::new(
                    SourceState::Corrupt,
                    "this Libra binary was compiled without `storage-redb`",
                ))
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = scope.storage.join(util::DATABASE);
                let probe = state::probe_path(&db_path);
                let probe_state = state::state_for_probe(&probe);
                if !probe_state.is_available() {
                    let message = match probe {
                        ProbeOutcome::Absent => "this repository has no database yet",
                        _ => "the repository database cannot be opened for reading",
                    };
                    return Err(DataError::new(probe_state, message));
                }

                let db_path_str = db_path.to_str().ok_or_else(|| {
                    DataError::new(
                        SourceState::Corrupt,
                        "the repository database path is not valid UTF-8",
                    )
                })?;
                let conn =
                    db::open_connection_without_schema_management(db_path_str, READ_BUSY_TIMEOUT)
                        .await
                        .map_err(|error| {
                            DataError::new(
                                state::classify_open_error(&error),
                                "the repository database is unusable",
                            )
                        })?;

                let compatibility = crate::internal::db::schema::inspect_schema_for_connection(
                    &conn,
                    DatabaseRole::Repository,
                )
                .await
                .map_err(|error| {
                    DataError::new(
                        state::classify_open_error(&error),
                        "the repository schema could not be inspected",
                    )
                })?;
                let schema_state = state::classify_schema(&compatibility);
                if !schema_state.is_available() {
                    return Err(DataError::new(
                        schema_state,
                        "the repository schema is not compatible",
                    ));
                }

                // Pin the hash kind on this worker thread from the repository's recorded
                // object format before reading HEAD, so SHA-256/BLAKE3 repositories format
                // OIDs correctly.
                let object_format = match crate::internal::config::ConfigKv::get_with_conn(
                    &conn,
                    "core.objectformat",
                )
                .await
                {
                    Ok(Some(entry)) => entry.value,
                    // Only a genuinely absent key falls back to the legacy default.
                    Ok(None) => {
                        crate::internal::object_format::as_str(git_internal::hash::get_hash_kind())
                            .to_string()
                    }
                    Err(error) => {
                        return Err(DataError::new(
                            state::classify_error_chain(&error),
                            "repository configuration could not be read",
                        ));
                    }
                };
                let kind = crate::internal::object_format::parse_config_value(&object_format)
                    .map_err(|_| {
                        DataError::new(SourceState::Corrupt, "core.objectformat is invalid")
                    })?;
                git_internal::hash::set_hash_kind(kind);

                // Read HEAD and its branch tip inside one transaction, using the pinned
                // scope explicitly, so a concurrent checkout cannot pair an old branch
                // name with another branch's commit.
                let txn = conn.begin().await.map_err(|error| {
                    DataError::new(
                        state::classify_store_message(&error.to_string()),
                        "the repository database is unusable",
                    )
                })?;
                let head = Head::current_for_scope_result_with_conn(&txn, &scope.scope)
                    .await
                    .map_err(|error| {
                        DataError::new(
                            state::classify_branch_store_error(&error),
                            "HEAD could not be read",
                        )
                    })?;
                let commit = match &head {
                    Head::Branch(name) => Branch::find_branch_result_with_conn(&txn, name, None)
                        .await
                        .map_err(|error| {
                            DataError::new(
                                state::classify_branch_store_error(&error),
                                "the branch tip could not be read",
                            )
                        })?
                        .map(|branch| branch.commit),
                    Head::Detached(oid) => Some(*oid),
                };
                let _ = txn.rollback().await;

                let branch = match head {
                    Head::Branch(name) => name,
                    Head::Detached(_) => "detached".to_string(),
                };
                Ok(build_repo_context(scope, branch, commit, object_format))
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(DataError::new(
                    SourceState::Corrupt,
                    "this Libra binary was compiled without `storage-sqlite`",
                ))
            }
        }
    }
}

#[cfg(feature = "storage-redb")]
async fn read_context_redb(scope: &RequestScope) -> Result<RepoContext, DataError> {
    let git_db_path =
        crate::storage::runtime::resolve_repo_git_redb_path(&scope.storage).map_err(|error| {
            map_storage_error(&error, "the repository database path could not be resolved")
        })?;

    let probe = state::probe_path(&git_db_path);
    let probe_state = state::state_for_probe(&probe);
    if !probe_state.is_available() {
        let message = match probe {
            ProbeOutcome::Absent => "this repository has no database yet",
            _ => "the repository database cannot be opened for reading",
        };
        return Err(DataError::new(probe_state, message));
    }

    // Open strictly read-only (`open_read_only`, 16 MiB cache cap, MultiWriter,
    // never upgrades to write or runs crash recovery) and inspect `META_SCHEMA`
    // before opening store wrappers so older/newer `META_SCHEMA` versions map
    // precisely to `SchemaOld`/`SchemaNew`.
    let meta_path = git_db_path.clone();
    let meta_state = tokio::task::spawn_blocking(move || inspect_redb_meta_schema(&meta_path))
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
            _ => "the repository schema is not compatible",
        };
        return Err(DataError::new(meta_state, message));
    }

    let cfg_path = git_db_path.clone();
    let config_store = tokio::task::spawn_blocking(move || {
        crate::storage::redb::RedbConfigStore::open_read_only(&cfg_path, DatabaseRole::Repository)
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
        map_storage_error(&error, "the repository schema could not be inspected")
    })?;
    let current = receipts.iter().map(|receipt| receipt.version).max();
    let latest =
        crate::internal::db::schema::latest_schema_version_for_role(DatabaseRole::Repository)
            .map_err(|error| {
                DataError::new(
                    state::classify_open_error(&error),
                    "the repository schema could not be inspected",
                )
            })?;
    let compatibility = classify_schema_versions(current, latest);
    let schema_state = state::classify_schema(&compatibility);
    if !schema_state.is_available() {
        return Err(DataError::new(
            schema_state,
            "the repository schema is not compatible",
        ));
    }

    // Pin the hash kind on this worker thread from the repository's recorded
    // object format before parsing HEAD/branch OIDs, so SHA-256/BLAKE3
    // repositories format OIDs correctly.
    let object_format = match config_store.get("core.objectformat").await {
        Ok(Some(entry)) => entry.value,
        // Only a genuinely absent key falls back to the legacy default.
        Ok(None) => {
            crate::internal::object_format::as_str(git_internal::hash::get_hash_kind()).to_string()
        }
        Err(error) => {
            return Err(map_storage_error(
                &error,
                "repository configuration could not be read",
            ));
        }
    };
    let kind = crate::internal::object_format::parse_config_value(&object_format)
        .map_err(|_| DataError::new(SourceState::Corrupt, "core.objectformat is invalid"))?;
    git_internal::hash::set_hash_kind(kind);
    drop(config_store);

    let git_path = git_db_path;
    let git_store = tokio::task::spawn_blocking(move || {
        crate::storage::git_store::RedbGitStore::open_read_only(&git_path)
    })
    .await
    .map_err(|_| {
        DataError::new(
            SourceState::Corrupt,
            "the repository git store open task failed",
        )
    })?
    .map_err(|error| map_storage_error(&error, "the repository database is unusable"))?;

    // Read HEAD and its branch tip from a single MVCC read transaction via
    // `list_refs`, using the pinned worktree scope explicitly, so a concurrent
    // checkout cannot pair an old branch name with another branch's commit.
    let records = git_store
        .list_refs(scope.scope.worktree_id())
        .await
        .map_err(|error| map_storage_error(&error, "HEAD could not be read"))?;

    let expected_head_key = RefKey::LocalHead {
        worktree_id: scope.scope.worktree_id().map(ToOwned::to_owned),
    };
    let head_target = records
        .iter()
        .find(|record| record.key == expected_head_key)
        .map(|record| &record.target)
        .ok_or_else(|| DataError::new(SourceState::Corrupt, "HEAD could not be read"))?;

    let (branch, commit) = match head_target {
        RefTarget::Symbolic(name) => {
            if name.trim().is_empty() {
                return Err(DataError::new(
                    SourceState::Corrupt,
                    "HEAD could not be read",
                ));
            }
            let branch_key = RefKey::LocalBranch { name: name.clone() };
            let commit = match records.iter().find(|record| record.key == branch_key) {
                Some(record) => match &record.target {
                    RefTarget::Direct(hex) => {
                        let oid = crate::internal::object_format::parse_hex_for_kind(kind, hex)
                            .map_err(|_| {
                                DataError::new(
                                    SourceState::Corrupt,
                                    "the branch tip could not be read",
                                )
                            })?;
                        Some(oid)
                    }
                    RefTarget::Symbolic(_) => {
                        return Err(DataError::new(
                            SourceState::Corrupt,
                            "the branch tip could not be read",
                        ));
                    }
                },
                None => None,
            };
            (name.clone(), commit)
        }
        RefTarget::Direct(hex) => {
            let oid = crate::internal::object_format::parse_hex_for_kind(kind, hex)
                .map_err(|_| DataError::new(SourceState::Corrupt, "HEAD could not be read"))?;
            ("detached".to_string(), Some(oid))
        }
    };

    Ok(build_repo_context(scope, branch, commit, object_format))
}

#[cfg(feature = "storage-redb")]
fn inspect_redb_meta_schema(path: &std::path::Path) -> Result<SourceState, DataError> {
    let db = crate::storage::opener::open_redb_read_only(path, DatabaseRole::Repository)
        .map_err(|error| map_storage_error(&error, "the repository database is unusable"))?;
    let rtx = db.begin_read().map_err(|error| {
        let mapped_state = match &error {
            redb::TransactionError::Storage(storage_err) => {
                classify_redb_storage_error(storage_err)
            }
            redb::TransactionError::WouldBlock => SourceState::Busy,
            _ => SourceState::Corrupt,
        };
        DataError::new(mapped_state, "the repository database is unusable")
    })?;
    let table = match rtx.open_table(crate::storage::meta::META_SCHEMA_TABLE) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(SourceState::Corrupt),
        Err(redb::TableError::Storage(storage_err)) => {
            return Err(DataError::new(
                classify_redb_storage_error(&storage_err),
                "the repository schema could not be inspected",
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
                "the repository schema could not be inspected",
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
    Ok(state::classify_redb_meta_schema(
        &record,
        DatabaseRole::Repository,
    ))
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
fn classify_schema_versions(
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

fn map_storage_error(error: &StorageBackendError, default_message: &str) -> DataError {
    let mapped_state = state::classify_storage_error(error);
    let message = match error {
        StorageBackendError::RepairAborted { .. } => {
            "the repository database requires crash recovery before read-only access; run `libra fsck`"
        }
        StorageBackendError::SchemaTooNew { .. }
        | StorageBackendError::UnsupportedSchemaVersion { .. } => {
            "the repository schema is newer than this binary supports"
        }
        _ if mapped_state == SourceState::Absent => "this repository has no database yet",
        _ if mapped_state == SourceState::SchemaOld => {
            "the repository schema is older than this binary supports"
        }
        _ if matches!(mapped_state, SourceState::Unreadable { .. }) => {
            "the repository database cannot be opened for reading"
        }
        _ => default_message,
    };
    DataError::new(mapped_state, message)
}

fn build_repo_context(
    scope: &RequestScope,
    branch: String,
    commit: Option<git_internal::hash::ObjectHash>,
    object_format: String,
) -> RepoContext {
    let head_label = commit
        .map(|oid| {
            let text = oid.to_string();
            text.chars().take(12).collect::<String>()
        })
        .unwrap_or_else(|| "no commits".to_string());

    RepoContext {
        worktree_label: scope
            .worktree_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repository".to_string()),
        storage_label: util::ROOT_DIR.to_string(),
        branch,
        head: head_label,
        object_format,
        head_oid: commit.map(|oid| oid.to_string()),
    }
}
