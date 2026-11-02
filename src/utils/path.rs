//! Path builders for repository storage: index, objects, database, hooks, and attributes locations relative to the working directory.

use std::{future::Future, io, path::PathBuf};

use crate::utils::util;

tokio::task_local! {
    static INDEX_OVERRIDE: PathBuf;
}

/// Run `future` with a task-local index path used by all nested index readers.
///
/// This is intentionally crate-private and scoped to one async task, so dry-run
/// consumers can use an isolated index without environment variables or
/// cross-command/global state.
pub(crate) async fn with_index_override<T>(
    index_path: PathBuf,
    future: impl Future<Output = T>,
) -> T {
    INDEX_OVERRIDE.scope(index_path, future).await
}

pub fn index() -> PathBuf {
    if let Ok(index_path) = INDEX_OVERRIDE.try_with(Clone::clone) {
        return index_path;
    }
    // lore.md 2.1: the index is PER-WORKTREE — it lives in the local gitdir,
    // not the shared/common storage (db/objects stay shared).
    //
    // §C.4.2: from the gitdir the INVOCATION pinned, not the cwd. The database
    // half of this pairing is already pinned; leaving the index ambient would
    // let a cwd that moved mid-command read scope A's rows and then rewrite
    // worktree B's index — the two halves disagreeing is worse than either
    // being wrong alone. `request_worktree_gitdir_strict` falls back to the cwd
    // only for a caller that never pinned.
    util::request_worktree_gitdir_strict().join("index")
}

pub fn try_index() -> io::Result<PathBuf> {
    if let Ok(index_path) = INDEX_OVERRIDE.try_with(Clone::clone) {
        return Ok(index_path);
    }
    Ok(util::try_get_worktree_gitdir(None)?.join("index"))
}

pub fn objects() -> PathBuf {
    util::request_storage_path().join("objects")
}

pub fn try_objects() -> io::Result<PathBuf> {
    Ok(util::try_request_storage_path()?.join("objects"))
}

/// Shared/common repository storage that owns the commit-preview quota.
pub(crate) fn try_preview_scratch_storage() -> io::Result<PathBuf> {
    util::try_get_storage_path(None)
}

/// Algorithm namespace for the `fastcdc-v2020-32k` media cache (C-08).
/// Legacy `.libra/media/{chunks,manifests}` from `fastcdc-v1` is not this path.
pub const MEDIA_CACHE_NAMESPACE: &str = "fastcdc-v2020-32k";

/// Root of the current media cache: `.libra/media/fastcdc-v2020-32k/`.
#[cfg(feature = "fastcdc")]
pub fn media_root() -> PathBuf {
    util::storage_path()
        .join("media")
        .join(MEDIA_CACHE_NAMESPACE)
}

/// FastCDC media chunk store (lore.md §6): a physical sibling of `objects/`,
/// wholly outside the Git object graph. Chunk files live under
/// `media/<namespace>/chunks/<ab>/<chunk_hash>` and are never walked as loose
/// objects. Gated behind the `fastcdc` feature at the call sites.
#[cfg(feature = "fastcdc")]
pub fn media_chunks() -> PathBuf {
    media_root().join("chunks")
}

/// Paged FastCDC manifest root: `media/<namespace>/manifests/<oid>/summary.json`
/// plus immutable pages. Sibling of `objects/`, outside the Git object graph.
#[cfg(feature = "fastcdc")]
pub fn media_manifests() -> PathBuf {
    media_root().join("manifests")
}

pub fn database() -> PathBuf {
    util::storage_path().join(util::DATABASE)
}

pub fn hooks() -> PathBuf {
    util::storage_path().join("hooks")
}

pub fn attributes() -> PathBuf {
    util::working_dir().join(util::ATTRIBUTES)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    #[serial_test::serial(cwd)]
    fn preview_scratch_uses_common_storage_for_linked_worktree() {
        let root = tempfile::tempdir().expect("create linked-worktree fixture");
        let common = root.path().join("main/.libra");
        let linked = root.path().join("linked");
        let linked_gitdir = linked.join(".libra");
        fs::create_dir_all(common.join("objects")).expect("create shared object store");
        // The fail-closed pointer validation (`is_terminal_common_storage`)
        // requires the target to look like real common storage — carry the
        // repository database the way an actual main worktree does.
        fs::write(common.join(util::DATABASE), b"").expect("create shared repository database");
        fs::create_dir_all(&linked_gitdir).expect("create linked worktree gitdir");
        fs::write(
            linked_gitdir.join("commondir"),
            common.to_string_lossy().as_bytes(),
        )
        .expect("write commondir");
        fs::write(linked_gitdir.join("worktree_id"), b"linked-test\n").expect("write worktree id");
        let _cwd = crate::utils::test::ChangeDirGuard::new(&linked);

        assert_eq!(
            try_preview_scratch_storage().expect("resolve shared preview scratch"),
            common.canonicalize().expect("canonicalize shared storage")
        );
    }
}
