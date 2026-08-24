//! Streaming crash-safe atomic replacement for payloads that cannot be held in memory.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use tempfile::NamedTempFile;

use crate::utils::atomic_write::{ensure_dir_exists, fsync_parent_dir};

/// A same-filesystem temporary file finalized with atomic replacement.
pub(crate) struct StreamingAtomicFile {
    temporary: NamedTempFile,
    staging_dir: PathBuf,
    sync: bool,
}

impl StreamingAtomicFile {
    /// Create a writer in a staging directory, durably creating its ancestors
    /// when `sync` is enabled. The target may be selected after streaming.
    pub(crate) fn new_in(staging_dir: &Path, sync: bool) -> io::Result<Self> {
        Self::new_in_with_optional_permissions(staging_dir, sync, None)
    }

    /// Create a writer with an explicit creation mode. This is reserved for
    /// public repository objects whose historical contract is `0666 & umask`;
    /// private state continues to use [`Self::new_in`] and tempfile's 0600
    /// default.
    pub(crate) fn new_in_with_permissions(
        staging_dir: &Path,
        sync: bool,
        permissions: fs::Permissions,
    ) -> io::Result<Self> {
        Self::new_in_with_optional_permissions(staging_dir, sync, Some(permissions))
    }

    fn new_in_with_optional_permissions(
        staging_dir: &Path,
        sync: bool,
        permissions: Option<fs::Permissions>,
    ) -> io::Result<Self> {
        ensure_dir_exists(staging_dir, sync)?;
        let mut builder = tempfile::Builder::new();
        if let Some(permissions) = permissions {
            builder.permissions(permissions);
        }
        Ok(Self {
            temporary: builder.tempfile_in(staging_dir)?,
            staging_dir: staging_dir.to_path_buf(),
            sync,
        })
    }

    /// Flush/fsync the payload, atomically replace `target`, and sync affected
    /// directories where the platform supports it.
    pub(crate) fn persist(self, target: &Path) -> io::Result<()> {
        self.persist_with_post_replace_hook(target, || Ok(()))
    }

    /// Variant used by recovery tests that need to stop after the replacement
    /// is visible but before its directory entry is synced. Production callers
    /// use [`Self::persist`], whose hook is a no-op.
    pub(crate) fn persist_with_post_replace_hook<F>(
        mut self,
        target: &Path,
        post_replace: F,
    ) -> io::Result<()>
    where
        F: FnOnce() -> io::Result<()>,
    {
        let parent = target.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("atomic stream target has no parent: {}", target.display()),
            )
        })?;
        ensure_dir_exists(parent, self.sync)?;
        self.temporary.flush()?;
        if self.sync {
            self.temporary.as_file().sync_all()?;
        }
        persist_temporary(self.temporary, target, self.sync)?;
        post_replace()?;
        if self.sync {
            fsync_parent_dir(parent)?;
            if self.staging_dir != parent {
                fsync_parent_dir(&self.staging_dir)?;
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
fn persist_temporary(temporary: NamedTempFile, target: &Path, _sync: bool) -> io::Result<()> {
    temporary.persist(target).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(windows)]
fn persist_temporary(temporary: NamedTempFile, target: &Path, sync: bool) -> io::Result<()> {
    if !sync {
        temporary.persist(target).map_err(|error| error.error)?;
        return Ok(());
    }

    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let temporary = temporary.into_temp_path();
    let source: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl Write for StreamingAtomicFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.temporary.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.temporary.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::StreamingAtomicFile;

    #[test]
    fn streaming_replace_creates_nested_parents_and_replaces_full_contents() {
        let temp = tempfile::tempdir().expect("create atomic stream test directory");
        let staging = temp.path().join("staging");
        let target = temp.path().join("objects/ab/cd/object");
        std::fs::create_dir_all(&staging).expect("create staging directory");
        std::fs::create_dir_all(target.parent().expect("target parent"))
            .expect("create initial target parent");
        std::fs::write(&target, b"truncated old payload").expect("seed target");

        let mut writer =
            StreamingAtomicFile::new_in(&staging, true).expect("create streaming atomic writer");
        writer.write_all(b"complete").expect("stream payload");
        writer.persist(&target).expect("persist streamed payload");

        assert_eq!(std::fs::read(&target).expect("read target"), b"complete");

        let nested_target = temp.path().join("objects/ef/01/new-object");
        let mut nested_writer =
            StreamingAtomicFile::new_in(&staging, true).expect("create second atomic writer");
        nested_writer
            .write_all(b"new payload")
            .expect("stream nested payload");
        nested_writer
            .persist(&nested_target)
            .expect("persist into newly created shard parents");
        assert_eq!(
            std::fs::read(nested_target).expect("read nested target"),
            b"new payload"
        );
    }
}
