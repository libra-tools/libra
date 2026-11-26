//! Bounded internal workspace port for promisor lazy-fetch (`CoreLazyFetchPort`).
//!
//! Root `libra` registers its `command::try_lazy_fetch_object` implementation
//! via [`register_lazy_fetch_hook`]. When no hook is registered and the active
//! repository configures `extensions.partialClone`, [`try_lazy_fetch_object`]
//! fails closed with [`LazyFetchUnregisteredError`] instead of silently
//! degrading to an ordinary missing-object error.

use std::sync::OnceLock;

use git_internal::hash::ObjectHash;
use thiserror::Error;

/// Function signature for root-provided promisor lazy fetch.
#[doc(hidden)]
pub type LazyFetchFn = fn(&ObjectHash) -> bool;

static LAZY_FETCH_HOOK: OnceLock<LazyFetchFn> = OnceLock::new();

/// Returned when promisor lazy fetch cannot be safely skipped because no root
/// hook is registered in this process.
#[doc(hidden)]
#[derive(Debug, Clone, Error)]
pub enum LazyFetchUnregisteredError {
    #[error(
        "promisor lazy-fetch hook is not registered for partial clone repository (remote '{remote}'); \
         invoke through the root `libra` entrypoint or register the runtime hook first"
    )]
    PartialCloneUnregistered { remote: String },
    #[error(
        "promisor lazy-fetch hook is not registered and repository partial-clone inspection failed: \
         {detail}; refusing to fail open"
    )]
    InspectionFailed { detail: String },
}

/// Register the process-wide promisor lazy-fetch hook (idempotent).
#[doc(hidden)]
pub fn register_lazy_fetch_hook(hook: LazyFetchFn) {
    let _ = LAZY_FETCH_HOOK.set(hook);
}

/// Attempt to lazy-fetch a missing promisor object.
///
/// - When a hook is registered, delegates directly to it (preserving the
///   existing `bool` semantics when fetch returns `false`).
/// - When no hook is registered:
///   - Under [`crate::utils::read_policy::ReadPolicy::LocalOnly`], returns `Ok(false)`.
///   - Otherwise inspects the active repository for `extensions.partialClone`:
///     - If set, returns `Err(LazyFetchUnregisteredError::PartialCloneUnregistered)` (fail-closed).
///     - If repository/database/config inspection fails, returns
///       `Err(LazyFetchUnregisteredError::InspectionFailed)` (fail-closed).
///     - If no repository is active (`NotFound`) or `extensions.partialClone` is
///       absent in an ordinary repository, returns `Ok(false)`.
#[doc(hidden)]
pub fn try_lazy_fetch_object(hash: &ObjectHash) -> Result<bool, LazyFetchUnregisteredError> {
    if let Some(hook) = LAZY_FETCH_HOOK.get() {
        return Ok(hook(hash));
    }
    if matches!(
        crate::utils::read_policy::read_policy(),
        crate::utils::read_policy::ReadPolicy::LocalOnly
    ) {
        return Ok(false);
    }
    match current_repo_partial_clone_remote() {
        Ok(Some(remote)) => Err(LazyFetchUnregisteredError::PartialCloneUnregistered { remote }),
        Ok(None) => Ok(false),
        Err(detail) => Err(LazyFetchUnregisteredError::InspectionFailed { detail }),
    }
}

fn current_repo_partial_clone_remote() -> Result<Option<String>, String> {
    let storage = match crate::utils::util::try_get_storage_path(None) {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("failed to locate repository storage: {err}")),
    };
    let db_path = storage.join(crate::utils::util::DATABASE);
    if !db_path.is_file() {
        return Err(format!(
            "repository database '{}' is missing or not a regular file",
            db_path.display()
        ));
    }
    let handle = std::thread::spawn(move || -> Result<Option<String>, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| format!("failed to build inspection runtime: {err}"))?;
        rt.block_on(async move {
            let conn = crate::internal::db::open_database_without_migrations(&db_path)
                .await
                .map_err(|err| {
                    format!(
                        "failed to open repository database '{}': {err}",
                        db_path.display()
                    )
                })?;
            crate::internal::config::ConfigKv::get_with_conn(&conn, "extensions.partialClone")
                .await
                .map(|entry| entry.map(|e| e.value))
                .map_err(|err| format!("failed to query extensions.partialClone: {err}"))
        })
    });
    handle
        .join()
        .map_err(|_| "partial-clone inspection thread panicked".to_string())?
}

#[cfg(test)]
mod tests {
    use git_internal::hash::ObjectHash;
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{internal::config::ConfigKv, utils::test};

    #[tokio::test]
    #[serial(cwd, env)]
    async fn unregistered_hook_fails_closed_for_partial_clone() {
        let dir = tempdir().expect("create temp repo");
        test::setup_with_new_libra_in(dir.path()).await;
        let _cwd = test::ChangeDirGuard::new(dir.path());
        let missing = ObjectHash::from_type_and_data(
            git_internal::internal::object::types::ObjectType::Blob,
            b"missing-promisor-object",
        );

        // Ordinary repo without extensions.partialClone: returns Ok(false)
        // when no hook is registered (preserves ordinary ObjectNotFound).
        if LAZY_FETCH_HOOK.get().is_none() {
            let res = try_lazy_fetch_object(&missing);
            assert!(
                matches!(res, Ok(false)),
                "ordinary repo should return Ok(false), got {res:?}"
            );

            // Partial-clone repo with extensions.partialClone set: must fail
            // closed instead of silently degrading to Ok(false).
            ConfigKv::set("extensions.partialClone", "origin", false)
                .await
                .expect("set extensions.partialClone");
            let err = try_lazy_fetch_object(&missing)
                .expect_err("partial-clone repo must fail closed when hook is unregistered");
            assert!(
                matches!(
                    err,
                    LazyFetchUnregisteredError::PartialCloneUnregistered { ref remote }
                        if remote == "origin"
                ),
                "expected PartialCloneUnregistered(origin), got {err:?}"
            );

            // Corrupt/unreadable repository DB must also fail closed rather than
            // returning Ok(false).
            let db_path = dir
                .path()
                .join(crate::utils::util::ROOT_DIR)
                .join(crate::utils::util::DATABASE);
            std::fs::write(&db_path, b"not-a-valid-sqlite-database")
                .expect("corrupt test libra.db");
            let inspect_err = try_lazy_fetch_object(&missing)
                .expect_err("corrupt repo DB must fail closed when hook is unregistered");
            assert!(
                matches!(
                    inspect_err,
                    LazyFetchUnregisteredError::InspectionFailed { .. }
                ),
                "expected InspectionFailed, got {inspect_err:?}"
            );
        }
    }
}
