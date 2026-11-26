//! W4-13: ApprovalStore runtime cache ownership keyed by canonical `repo_id`.
//!
//! Persistent Always approvals live in [`ApprovedRulesetStore`] (W4-07). This
//! module owns the in-process cache key and lease-takeover invalidation:
//! session/TTL/allow-all memos are scoped to `repo:{libra.repoid}` (never a
//! global `None` prefix) and are dropped when a controller lease is taken
//! over. Always rows remain visible across linked worktrees via the database.

use std::{path::Path, sync::Arc};

use chrono::Utc;
use tokio::sync::Mutex;

use super::{ApprovedRuleset, ApprovedRulesetStore, ApprovedStoreError};
use crate::internal::{ai::sandbox::ApprovalStore, worktree_scope::RequestScope};

/// Fail-closed errors while resolving the runtime approval cache.
#[derive(Debug, thiserror::Error)]
pub enum ApprovalRuntimeCacheError {
    #[error(
        "working directory `{0}` is not a Libra repository; approval cache cannot use a global scope"
    )]
    NotARepository(String),
    #[error("cannot resolve approval cache for `{path}`: {detail}")]
    DamagedWorktree { path: String, detail: String },
    #[error("{0}")]
    Database(String),
    #[error(transparent)]
    Approved(#[from] ApprovedStoreError),
}

/// Repository-keyed Always snapshot plus the explicit ApprovalStore scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRuntimeCache {
    pub repo_id: String,
    pub scope_key: String,
    pub approved_ruleset: ApprovedRuleset,
}

/// Explicit ApprovalStore prefix for a canonical `libra.repoid`.
///
/// Never returns an empty string (sandbox would treat that as the global
/// `interactive` scope).
pub fn approval_cache_scope_key(repo_id: &str) -> Result<String, ApprovalRuntimeCacheError> {
    if repo_id.is_empty() || repo_id != repo_id.trim() {
        return Err(ApprovalRuntimeCacheError::Database(
            "cannot key the approval cache: repository identity is empty or padded".to_string(),
        ));
    }
    Ok(format!("repo:{repo_id}"))
}

/// Isolated non-repository scope so tests and accidental non-repo launches
/// never share the global `interactive` cache.
pub fn unbound_approval_cache_scope(working_dir: &Path) -> String {
    let path = working_dir
        .canonicalize()
        .unwrap_or_else(|_| working_dir.to_path_buf());
    format!("unbound:{}", path.display())
}

/// Load Always approvals and the repo-id cache key for `working_dir`.
///
/// Linked worktrees share common storage, so they resolve the same
/// `repo_id` / Always ruleset (W4-07 provenance stays on the rows).
/// Missing identity, damaged worktree, or database errors fail closed.
pub async fn resolve_approval_runtime_cache(
    working_dir: &Path,
) -> Result<ApprovalRuntimeCache, ApprovalRuntimeCacheError> {
    let request = match RequestScope::try_resolve(working_dir.to_path_buf()) {
        Ok(Some(request)) => request,
        Ok(None) => {
            return Err(ApprovalRuntimeCacheError::NotARepository(
                working_dir.display().to_string(),
            ));
        }
        Err(error) => {
            return Err(ApprovalRuntimeCacheError::DamagedWorktree {
                path: working_dir.display().to_string(),
                detail: error.to_string(),
            });
        }
    };
    let ruleset = match crate::storage::runtime::detect_repo_backend(&request.storage).map_err(
        |error| {
            ApprovalRuntimeCacheError::Database(format!(
                "cannot detect storage backend for `{}` to load Always approvals: {error}",
                request.storage.display()
            ))
        },
    )? {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let ro_config =
                    crate::storage::runtime::open_repo_config_store_read_only(&request.storage)
                        .await
                        .map_err(|error| {
                            ApprovalRuntimeCacheError::Database(format!(
                                "cannot open repository config store `{}` to load Always approvals: {error}",
                                request.storage.display()
                            ))
                        })?;
                let identity = match crate::internal::workspace::RepoIdentity::resolve_with_store(
                    ro_config.as_ref(),
                )
                .await
                {
                    Ok(identity) => identity,
                    Err(crate::internal::workspace::WorkspaceError::Corrupt(message))
                        if message.contains("repository identity (libra.repoid) is missing") =>
                    {
                        drop(ro_config);
                        let rw_config = crate::storage::runtime::open_repo_config_store_read_write(
                            &request.storage,
                        )
                        .await
                        .map_err(|error| {
                            ApprovalRuntimeCacheError::Database(format!(
                                "cannot open repository config store `{}` to initialize repository identity: {error}",
                                request.storage.display()
                            ))
                        })?;
                        ApprovedRulesetStore::ensure_repo_identity_with_store(rw_config.as_ref())
                            .await?
                    }
                    Err(error) => return Err(ApprovedStoreError::Workspace(error).into()),
                };
                let state_store = crate::storage::runtime::open_git_state_store_read_only(
                    &request.storage,
                )
                .await
                .map_err(|error| {
                    ApprovalRuntimeCacheError::Database(format!(
                        "cannot open repository state store `{}` to load Always approvals: {error}",
                        request.storage.display()
                    ))
                })?;
                ApprovedRulesetStore::load_for_project_id_with_store(
                    state_store.as_ref(),
                    identity.as_str(),
                )
                .await?
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                return Err(ApprovalRuntimeCacheError::Database(
                    "this Libra binary was compiled without `storage-redb`".to_string(),
                ));
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = request.storage.join(crate::utils::util::DATABASE);
                let conn = crate::internal::db::get_db_conn_instance_for_path(&db_path)
                    .await
                    .map_err(|error| {
                        ApprovalRuntimeCacheError::Database(format!(
                            "cannot open repository database `{}` to load Always approvals: {error}",
                            db_path.display()
                        ))
                    })?;
                ApprovedRulesetStore::load(&conn).await?
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                return Err(ApprovalRuntimeCacheError::Database(
                    "this Libra binary was compiled without `storage-sqlite`".to_string(),
                ));
            }
        }
    };
    let scope_key = approval_cache_scope_key(&ruleset.project_id)?;
    Ok(ApprovalRuntimeCache {
        repo_id: ruleset.project_id.clone(),
        scope_key,
        approved_ruleset: ruleset,
    })
}

/// Drop in-memory session/TTL/allow-all memos after a controller lease
/// takeover. Persistent Always rows are untouched.
pub async fn revoke_session_approval_memos(store: &Arc<Mutex<ApprovalStore>>) {
    let mut store = store.lock().await;
    let keys: Vec<String> = store
        .active_memos_at(Utc::now())
        .into_iter()
        .map(|memo| memo.key)
        .collect();
    for key in keys {
        store.revoke(&key);
    }
    for scope in store.active_allow_all_scopes() {
        store.revoke_allow_all_for_scope(&scope);
    }
}
