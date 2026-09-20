//! Shared worktree blob materialization primitive (plan issues/470 FM-01,
//! ADR-FM-02/03).
//!
//! Tracked blob writes must:
//! - derive the on-disk permission bits from the entry mode (`100755` -> `0o777`,
//!   `100644` -> `0o666`) and let the kernel apply the process `umask` at
//!   creation time, exactly like Git's `create_file`;
//! - replace existing files through a same-directory temp file plus rename, so
//!   content and permissions become visible together and a stale execute bit is
//!   cleared instead of silently preserved;
//! - keep symbolic-link semantics (replacing an existing entry, never writing
//!   through a directory).
//!
//! The temp-file/rename mechanics are the shared
//! [`StreamingAtomicFile`](crate::utils::atomic_stream::StreamingAtomicFile)
//! implementation; `util::write_file` stays in use for non-blob files (config,
//! sidecars).

use std::{
    fs::{self, Permissions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::utils::atomic_stream::StreamingAtomicFile;

/// Platform-appropriate creation permissions for one entry mode.
fn worktree_permissions(executable: bool) -> Option<Permissions> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(Permissions::from_mode(if executable {
            0o777
        } else {
            0o666
        }))
    }
    #[cfg(not(unix))]
    {
        // No POSIX permission bits on this platform; tempfile's defaults apply.
        let _ = executable;
        None
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique same-directory sibling used as the rename source for symlinks.
fn temp_sibling_path(dest: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "blob".to_string());
    dest.with_file_name(format!(
        ".{name}.libra-tmp-{}-{counter}",
        std::process::id()
    ))
}

/// A same-directory staging file that becomes `dest` on
/// [`WorktreeFileWriter::finish`].
///
/// The staging file is created with the mode-derived permissions so the rename
/// publishes content and permissions in one step. Dropping an unfinished writer
/// removes the staging file (the underlying `NamedTempFile` owns it); a failed
/// rename leaves the destination untouched.
pub struct WorktreeFileWriter {
    inner: Option<StreamingAtomicFile>,
    dest: PathBuf,
}

impl WorktreeFileWriter {
    /// Create the staging file next to `dest`, creating parents as needed.
    ///
    /// `executable` selects `0o777` (entry `100755`) or `0o666` (entry
    /// `100644`); the kernel applies the process `umask`.
    pub fn create(dest: &Path, executable: bool) -> io::Result<Self> {
        let parent = dest
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        let staging_dir = parent.unwrap_or_else(|| Path::new("."));
        let inner = match worktree_permissions(executable) {
            Some(permissions) => {
                StreamingAtomicFile::new_in_with_permissions(staging_dir, false, permissions)?
            }
            None => StreamingAtomicFile::new_in(staging_dir, false)?,
        };
        Ok(Self {
            inner: Some(inner),
            dest: dest.to_path_buf(),
        })
    }

    /// Path of the staging file, for streaming writers that take a path instead
    /// of a writer (for example the LFS downloader).
    pub fn temp_path(&self) -> &Path {
        self.inner
            .as_ref()
            .expect("worktree blob writer is open")
            .temp_path()
    }

    /// Flush and atomically publish the staging file at the destination.
    pub fn finish(mut self) -> io::Result<()> {
        let inner = self.inner.take().expect("worktree blob writer is open");
        inner.persist(&self.dest)
    }
}

impl Write for WorktreeFileWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner
            .as_mut()
            .expect("worktree blob writer is open")
            .write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner
            .as_mut()
            .expect("worktree blob writer is open")
            .flush()
    }
}

/// Write `content` to `path` as a regular file with the entry-mode permissions
/// (see [`WorktreeFileWriter`]).
pub fn write_worktree_blob(path: &Path, content: &[u8], executable: bool) -> io::Result<()> {
    let mut writer = WorktreeFileWriter::create(path, executable)?;
    writer.write_all(content)?;
    writer.finish()
}

/// Replace `path` with a symbolic link pointing at `target` (Unix).
///
/// The link is created next to the destination and renamed over it, matching
/// Git's symlink replacement semantics; replacing an existing directory is
/// refused by the rename itself.
#[cfg(unix)]
pub fn write_worktree_symlink(path: &Path, target: &[u8]) -> io::Result<()> {
    use std::{
        ffi::OsStr,
        os::unix::{ffi::OsStrExt, fs::symlink},
    };

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let temp_path = temp_sibling_path(path);
    // A stale temp from a crashed writer would make the name clash; a symlink
    // is cheap to recreate, so start clean.
    let _ = fs::remove_file(&temp_path);
    symlink(Path::new(OsStr::from_bytes(target)), &temp_path)?;
    match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

/// Non-Unix platforms do not get a regular-file fallback: the previous restore
/// implementation reported symlinks as unsupported and callers surface that as
/// a dedicated error.
#[cfg(not(unix))]
pub fn write_worktree_symlink(_path: &Path, _target: &[u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symbolic links are not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn writes_and_replaces_regular_files_with_mode_derived_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.txt");
        write_worktree_blob(&path, b"hello", false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");

        // Replacing an executable blob with a non-executable one must clear the
        // execute bit (the rename publishes the fresh staging permissions).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            write_worktree_blob(&path, b"run", true).unwrap();
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_ne!(mode & 0o111, 0, "executable entry must set an x bit");
            write_worktree_blob(&path, b"plain", false).unwrap();
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode & 0o111, 0, "non-executable entry must clear x bits");
        }
    }

    #[test]
    fn dropped_writer_leaves_no_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-published.txt");
        {
            let mut writer = WorktreeFileWriter::create(&path, false).unwrap();
            writer.write_all(b"partial").unwrap();
            // No finish(): the staging file must not survive the drop.
        }
        assert!(!path.exists());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging file must be cleaned up: {leftovers:?}"
        );
    }

    #[test]
    fn failed_rename_keeps_destination_and_cleans_staging() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("occupied");
        fs::create_dir(&dest).unwrap();
        let error = write_worktree_blob(&dest, b"content", false).unwrap_err();
        assert!(error.raw_os_error().is_some(), "unexpected error: {error}");
        assert!(dest.is_dir(), "destination directory must survive");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            leftovers,
            vec![std::ffi::OsString::from("occupied")],
            "no staging file may remain"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_writer_replaces_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("link");
        write_worktree_blob(&path, b"old", false).unwrap();
        write_worktree_symlink(&path, b"target").unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_symlink());
        assert_eq!(fs::read_link(&path).unwrap(), PathBuf::from("target"));
    }
}
