//! Repository discovery and context reads for the window.
//!
//! Discovery reuses [`RequestScope`] (never the panicking path helpers), and
//! every database read uses an injected connection opened without schema
//! management, so a window read cannot migrate the repository.

use std::{path::PathBuf, time::Duration};

use sea_orm::TransactionTrait;

use super::{
    snapshot::{DataError, RepoContext},
    state::{self, ProbeOutcome, SourceState},
};
use crate::{
    internal::{
        branch::Branch,
        db::{self, DatabaseRole},
        head::Head,
        worktree_scope::RequestScope,
    },
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

/// Read the repository context through an injected, non-migrating connection.
pub async fn read_context(scope: &RequestScope) -> Result<RepoContext, DataError> {
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
    let conn = db::open_connection_without_schema_management(db_path_str, READ_BUSY_TIMEOUT)
        .await
        .map_err(|error| {
            DataError::new(
                state::classify_open_error(&error),
                "the repository database is unusable",
            )
        })?;

    let compatibility =
        crate::internal::db::schema::inspect_schema_for_connection(&conn, DatabaseRole::Repository)
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
    let object_format = object_format(&conn).await?;
    let kind = crate::internal::object_format::parse_config_value(&object_format)
        .map_err(|_| DataError::new(SourceState::Corrupt, "core.objectformat is invalid"))?;
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
    let head_label = commit
        .map(|oid| {
            let text = oid.to_string();
            text.chars().take(12).collect::<String>()
        })
        .unwrap_or_else(|| "no commits".to_string());

    Ok(RepoContext {
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
    })
}

/// Object format recorded for this repository, read through the injected
/// connection; falls back to the process default when unset.
async fn object_format(conn: &impl sea_orm::ConnectionTrait) -> Result<String, DataError> {
    match crate::internal::config::ConfigKv::get_with_conn(conn, "core.objectformat").await {
        Ok(Some(entry)) => Ok(entry.value),
        // Only a genuinely absent key falls back to the legacy default.
        Ok(None) => Ok(
            crate::internal::object_format::as_str(git_internal::hash::get_hash_kind()).to_string(),
        ),
        Err(error) => Err(DataError::new(
            state::classify_error_chain(&error),
            "repository configuration could not be read",
        )),
    }
}
