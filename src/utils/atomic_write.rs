//! Crash-safe atomic file writes for Libra's on-disk persistent state.
//!
//! lore.md §7.7: several `.libra/` writes (loose objects, the merge/revert
//! sequencer state) used a bare `fs::File::create` / `fs::write` straight to the
//! final path. A crash mid-write leaves a half-written file at the final path,
//! which corrupts a later reconcile or sequencer recovery. [`write_atomic`]
//! replaces those with the standard temp-file-then-rename dance so the final
//! path only ever contains a complete file.
//!
//! Two separate guarantees:
//! - **Atomicity** (always): write to a temp file in the same directory, then
//!   `rename` it over the destination. A crash before the rename leaves the
//!   destination untouched — only a stray temp file remains, never a truncated
//!   final file.
//! - **Durability** (opt-in via `fsync`): `sync_all` the temp file before the
//!   rename and `fsync` the parent directory after, so the write also survives a
//!   power loss. Recovery-critical state (sequencer files) always fsyncs;
//!   bulk object writes fsync only when `--sync-data` is requested (lore.md §0.5,
//!   wired through [`set_sync_data`]/[`sync_data_enabled`]).

use std::{
    fs,
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

/// Whether bulk object/index writes should also fsync for power-loss durability.
/// Recovery-critical sequencer writes fsync regardless of this flag. Toggled by
/// `--sync-data` (lore.md §0.5).
static SYNC_DATA: AtomicBool = AtomicBool::new(false);

/// Enable/disable fsync for bulk object writes (the `--sync-data` switch, §0.5).
pub fn set_sync_data(enabled: bool) {
    SYNC_DATA.store(enabled, Ordering::Relaxed);
}

/// Whether bulk object writes should fsync (see [`set_sync_data`]).
pub fn sync_data_enabled() -> bool {
    SYNC_DATA.load(Ordering::Relaxed)
}

/// Initialise the sync-data flag from the `LIBRA_SYNC_DATA` environment variable
/// (`1`/`true`/`yes`/`on` → enabled). Called once at process startup so the
/// hook is usable today; the `--sync-data` CLI flag (lore.md §0.5) will layer on
/// top by calling [`set_sync_data`] directly.
pub fn init_sync_data_from_env() {
    let enabled = std::env::var("LIBRA_SYNC_DATA")
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"));
    set_sync_data(enabled);
}

/// Atomically write `bytes` to `path`.
///
/// Writes to a uniquely-named temp file in `path`'s parent directory, flushes
/// it, optionally fsyncs it, then renames it over `path`. The rename is atomic
/// on POSIX and on Windows (`MoveFileExW` replacement), so a reader either sees
/// the old file or the fully-written new file — never a partial write. With
/// `fsync=true`, Windows also requests `MOVEFILE_WRITE_THROUGH`.
///
/// # Arguments
/// * `path` - final destination path.
/// * `bytes` - full contents to write.
/// * `fsync` - when true, fsync the temp file before replacement; Unix also
///   fsyncs affected directories, while Windows uses write-through replacement.
///
/// # Errors
/// Propagates any IO error from creating the parent directory, writing/syncing
/// the temp file, or the rename.
pub fn write_atomic(path: &Path, bytes: &[u8], fsync: bool) -> io::Result<()> {
    write_atomic_with_post_replace_hook(path, bytes, fsync, || Ok(()))
}

/// Atomically write `bytes`, invoking `post_replace` after the final path is
/// visible but before its directory entry is synced. This narrow hook exists
/// for instance-scoped durability fault injection; ordinary callers should use
/// [`write_atomic`].
pub(crate) fn write_atomic_with_post_replace_hook<F>(
    path: &Path,
    bytes: &[u8],
    fsync: bool,
    post_replace: F,
) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cannot atomically write a path with no parent: {}",
                path.display()
            ),
        )
    })?;
    // Create (and, under fsync, durably persist) any missing ancestor
    // directories BEFORE writing into them. Under fsync this matters: a bare
    // `create_dir_all` records a new directory only in its own parent's
    // directory entry, which — if never fsynced — can be lost on power loss,
    // taking the object inside it with it.
    ensure_dir_exists(parent, fsync)?;

    let mut writer = crate::utils::atomic_stream::StreamingAtomicFile::new_in(parent, fsync)?;
    writer.write_all(bytes)?;
    writer.persist_with_post_replace_hook(path, post_replace)
}

/// Create `dir` and any missing ancestors, fsyncing each newly-created
/// directory's parent (under `fsync`) so the directory entry itself is durable.
///
/// Creates the deepest-missing-first by recursing on the parent, then creating
/// this level. Race-safe: a concurrent creation surfaces as `AlreadyExists` and
/// is treated as success.
pub(crate) fn ensure_dir_exists(dir: &Path, fsync: bool) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        ensure_dir_exists(parent, fsync)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => {
            if fsync && let Some(parent) = dir.parent() {
                // Persist the new directory entry recorded in its parent.
                fsync_parent_dir(parent)?;
            }
            Ok(())
        }
        // Lost a create race with another writer — the directory now exists.
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(err) => Err(err),
    }
}

/// fsync a directory so a rename into it is durable.
#[cfg(unix)]
pub(crate) fn fsync_parent_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(windows)]
pub(crate) fn fsync_parent_dir(_dir: &Path) -> io::Result<()> {
    // Windows does not expose a reliable directory-fsync equivalent. The
    // durable atomic replacement itself uses MOVEFILE_WRITE_THROUGH in
    // `StreamingAtomicFile`, which waits for the move metadata to reach disk.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn fsync_parent_dir(_dir: &Path) -> io::Result<()> {
    // Other targets have no portable directory-sync primitive. Atomic rename
    // remains guaranteed, while power-loss durability is best effort.
    Ok(())
}

/// Unlink `path` so the removal SURVIVES a power loss.
///
/// The parent directory entry is fsynced after the unlink: a removal that has
/// not reached the disk leaves the file behind exactly as a lost write would,
/// and for recovery-critical state (sequencer sidecars, the stash log) a
/// resurrected file replays an operation the user already concluded. Both the
/// directory open and the sync PROPAGATE — swallowing them would report a
/// durable deletion that is not.
pub fn remove_durably(path: &Path) -> io::Result<()> {
    remove_durably_with_post_remove_hook(path, || Ok(()))
}

/// Durably remove `path`, invoking `post_remove` after the unlink is visible
/// but before its containing directory is synced. The hook is used only by
/// scoped recovery tests.
pub(crate) fn remove_durably_with_post_remove_hook<F>(path: &Path, post_remove: F) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cannot fsync the parent of a path with none: {}",
                path.display()
            ),
        )
    })?;
    match fs::remove_file(path) {
        Ok(()) => {
            post_remove()?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A previous unlink may have become visible before its directory
            // fsync failed. Re-sync an existing parent before acknowledging an
            // idempotent retry, otherwise the removed entry may return after a
            // crash even though this call reported success.
            return match fs::metadata(parent) {
                Ok(metadata) if metadata.is_dir() => {
                    post_remove()?;
                    fsync_parent_dir(parent)
                }
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!(
                        "cannot fsync non-directory parent '{}' after removing '{}'",
                        parent.display(),
                        path.display()
                    ),
                )),
                Err(parent_error) if parent_error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(parent_error) => Err(parent_error),
            };
        }
        Err(error) => return Err(error),
    }
    fsync_parent_dir(parent)
}

/// Re-sync a visible recovery sidecar and its containing directory before an
/// exact idempotent ACK, with a narrow hook between file and parent syncs for
/// scoped durability tests. A prior atomic replacement can be observable even
/// if its final directory sync reported failure.
pub(crate) fn sync_file_and_parent_durably_with_pre_parent_sync_hook<F>(
    path: &Path,
    pre_parent_sync: F,
) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cannot fsync the parent of a path with none: {}",
                path.display()
            ),
        )
    })?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()?;
    pre_parent_sync()?;
    fsync_parent_dir(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_full_contents_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obj");
        write_atomic(&path, b"hello world", false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello world");

        // No stray temp files remain after a successful write.
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "only the destination file should remain");
    }

    #[test]
    fn overwrites_existing_file_completely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"old much-longer contents").unwrap();
        write_atomic(&path, b"new", true).unwrap();
        // A partial overwrite of a shorter payload would leave trailing bytes;
        // the rename guarantees the file is exactly the new contents.
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn creates_and_fsyncs_missing_nested_parent_directories() {
        // fsync=true exercises the durable directory-creation path (each newly
        // created level's parent is fsynced) across multiple missing levels.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab").join("cd").join("ef01234");
        write_atomic(&path, b"loose object", true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"loose object");
    }

    #[test]
    fn sync_data_flag_round_trips() {
        // Default is off; toggling is observable. (Serial-safe: restore after.)
        let previous = sync_data_enabled();
        set_sync_data(true);
        assert!(sync_data_enabled());
        set_sync_data(false);
        assert!(!sync_data_enabled());
        set_sync_data(previous);
    }

    #[test]
    #[serial_test::serial]
    fn init_sync_data_from_env_reads_the_flag() {
        let previous = sync_data_enabled();

        {
            let _env = crate::utils::test::ScopedEnvVar::set("LIBRA_SYNC_DATA", "1");
            init_sync_data_from_env();
            assert!(sync_data_enabled(), "LIBRA_SYNC_DATA=1 should enable fsync");
        }
        {
            let _env = crate::utils::test::ScopedEnvVar::set("LIBRA_SYNC_DATA", "0");
            init_sync_data_from_env();
            assert!(
                !sync_data_enabled(),
                "LIBRA_SYNC_DATA=0 should disable fsync"
            );
        }

        set_sync_data(previous);
    }
}
