//! FUSE mount helpers shared by worktree commands and AI task workspaces.

use std::{
    ffi::OsString,
    fs, io,
    path::{Component, Path, PathBuf},
    process::Command,
};

use super::util;

const FUSE_TASK_WORKTREE_PREFIX: &str = "libra-task-worktree-fuse-";

/// Descriptor-pinned filesystem operations for user-created overlay worktrees.
/// Keep these separate from legacy task-worktree sweeping: those directories
/// have a different ownership and cleanup contract.
#[cfg(feature = "worktree-fuse")]
pub mod managed {
    use std::{
        ffi::{CString, OsString},
        fs, io,
        os::unix::{ffi::OsStrExt, fs::MetadataExt, io::AsRawFd},
        path::{Component, Path, PathBuf},
    };

    use crate::utils::beneath;

    pub struct Target {
        pub path: PathBuf,
        anchor_path: PathBuf,
        anchor: fs::File,
        suffix: PathBuf,
    }

    pub struct Directory {
        pub path: PathBuf,
        pub file: fs::File,
        anchor_path: PathBuf,
        anchor: fs::File,
        suffix: PathBuf,
        parent: fs::File,
        name: OsString,
        owned: bool,
        created_parents: Vec<(fs::File, OsString, fs::File)>,
    }

    fn component_name(name: &std::ffi::OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "FUSE path contains a NUL byte")
        })
    }

    fn same_directory(left: &fs::File, right: &fs::File) -> io::Result<bool> {
        let a = left.metadata()?;
        let b = right.metadata()?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }

    impl Target {
        /// Resolve aliases above the deepest existing parent once. The final
        /// component and all subsequently created components are no-follow.
        /// Resolution never creates directories, so every overlap check can
        /// run before any filesystem or branch mutation.
        pub fn resolve(path: &Path, invoking_workdir: &Path) -> io::Result<Self> {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                invoking_workdir.join(path)
            };
            if !absolute.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "FUSE target requires an absolute pinned invocation directory",
                ));
            }
            let absolute = super::normalize_abs_path(&absolute);
            let name = absolute.file_name().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "FUSE target cannot be a filesystem root",
                )
            })?;
            let mut suffix = vec![name.to_os_string()];
            let mut ancestor = absolute.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "FUSE target has no parent directory",
                )
            })?;
            loop {
                match fs::symlink_metadata(ancestor) {
                    Ok(_) => break,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let part = ancestor.file_name().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::NotFound,
                                "FUSE target has no existing ancestor",
                            )
                        })?;
                        suffix.push(part.to_os_string());
                        ancestor = ancestor.parent().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::NotFound,
                                "FUSE target has no existing ancestor",
                            )
                        })?;
                    }
                    Err(error) => return Err(error),
                }
            }
            let anchor_path = fs::canonicalize(ancestor)?;
            let anchor = beneath::open_root(&anchor_path)?;
            let suffix: PathBuf = suffix.into_iter().rev().collect();
            let path = anchor_path.join(&suffix);
            Self {
                path,
                anchor_path,
                anchor,
                suffix,
            }
            .inspect()
        }

        /// Pin managed paths at common storage before descending; do not
        /// canonicalize an intermediate managed component into a symlink.
        pub fn beneath(anchor_path: &Path, path: &Path) -> io::Result<Self> {
            let suffix = path
                .strip_prefix(anchor_path)
                .map_err(io::Error::other)?
                .to_path_buf();
            if suffix.as_os_str().is_empty()
                || suffix
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "FUSE managed target is outside its pinned root",
                ));
            }
            Self {
                path: path.to_path_buf(),
                anchor_path: anchor_path.to_path_buf(),
                anchor: beneath::open_root(anchor_path)?,
                suffix,
            }
            .inspect()
        }

        fn inspect(self) -> io::Result<Self> {
            match beneath::lstat_beneath(&self.anchor, &self.suffix) {
                Ok(stat) if stat.is_symlink || !stat.is_dir => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "FUSE target '{}' must be a real directory, not a symlink or file",
                            self.path.display()
                        ),
                    ));
                }
                Ok(_) => {
                    let dir = beneath::open_beneath(&self.anchor, &self.suffix)?;
                    if beneath::read_dir_fd(dir)?.next().transpose()?.is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("FUSE target '{}' is not empty", self.path.display()),
                        ));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            Ok(self)
        }

        pub fn create(self, require_new: bool) -> io::Result<Directory> {
            let components: Vec<OsString> = self
                .suffix
                .components()
                .map(|component| match component {
                    Component::Normal(name) => Ok(name.to_os_string()),
                    _ => Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "non-canonical FUSE target",
                    )),
                })
                .collect::<io::Result<_>>()?;
            let mut parent = self.anchor.try_clone()?;
            let mut created_parents = Vec::new();
            let outcome = (|| {
                for (index, name) in components.iter().enumerate() {
                    let final_component = index + 1 == components.len();
                    let mut owned = false;
                    let directory = match beneath::open_beneath(&parent, Path::new(name)) {
                        Ok(_) if final_component && require_new => {
                            return Err(io::Error::new(
                                io::ErrorKind::AlreadyExists,
                                format!(
                                    "FUSE worktree directory '{}' already exists; choose another name",
                                    self.path.display()
                                ),
                            ));
                        }
                        Ok(directory) => directory,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            let name_c = component_name(name)?;
                            // SAFETY: parent is a live directory descriptor and
                            // name_c is a single checked, NUL-terminated component.
                            if unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700) }
                                != 0
                            {
                                return Err(io::Error::last_os_error());
                            }
                            owned = true;
                            beneath::open_beneath(&parent, Path::new(name))?
                        }
                        Err(error) => return Err(error),
                    };
                    if final_component {
                        return Ok(Directory {
                            path: self.path,
                            file: directory,
                            anchor_path: self.anchor_path,
                            anchor: self.anchor,
                            suffix: self.suffix,
                            parent,
                            name: name.clone(),
                            owned,
                            created_parents: std::mem::take(&mut created_parents),
                        });
                    }
                    if owned {
                        created_parents.push((
                            parent.try_clone()?,
                            name.clone(),
                            directory.try_clone()?,
                        ));
                    }
                    parent = directory;
                }
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "FUSE target has no final component",
                ))
            })();
            if outcome.is_err() {
                for (parent, name, directory) in created_parents.iter().rev() {
                    remove_empty_owned_directory(parent, name, directory)?;
                }
            }
            outcome
        }
    }

    impl Directory {
        pub fn child_target(&self, relative: &Path) -> io::Result<Target> {
            if relative.as_os_str().is_empty()
                || relative
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "non-canonical FUSE child directory",
                ));
            }
            Target {
                path: self.path.join(relative),
                anchor_path: self.path.clone(),
                anchor: self.file.try_clone()?,
                suffix: relative.to_path_buf(),
            }
            .inspect()
        }

        /// Confirm the mount API's pathname still refers to the pinned tree.
        pub fn validate_path(&self) -> io::Result<()> {
            let current_anchor = beneath::open_root(&self.anchor_path)?;
            let current = beneath::open_beneath(&self.anchor, &self.suffix)?;
            if !same_directory(&current_anchor, &self.anchor)?
                || !same_directory(&current, &self.file)?
            {
                return Err(io::Error::other(format!(
                    "FUSE directory '{}' changed during creation; retry after stopping concurrent directory moves",
                    self.path.display()
                )));
            }
            Ok(())
        }

        pub fn is_empty(&self) -> io::Result<bool> {
            Ok(beneath::read_dir_fd(self.file.try_clone()?)?
                .next()
                .transpose()?
                .is_none())
        }

        /// Roll back only the leaf we created. Never traverse a replacement
        /// symlink or remove a caller-owned, pre-existing mountpoint.
        pub fn rollback(&self) -> io::Result<()> {
            if self.owned {
                remove_owned_directory(&self.parent, &self.name, &self.file)?;
            }
            for (parent, name, directory) in self.created_parents.iter().rev() {
                remove_empty_owned_directory(parent, name, directory)?;
            }
            Ok(())
        }
    }

    fn remove_empty_owned_directory(
        parent: &fs::File,
        name: &std::ffi::OsStr,
        directory: &fs::File,
    ) -> io::Result<()> {
        let current = beneath::open_beneath(parent, Path::new(name))?;
        if !same_directory(&current, directory)? {
            return Err(io::Error::other(
                "refusing FUSE parent cleanup: the directory was replaced",
            ));
        }
        let name = component_name(name)?;
        // SAFETY: rmdir removes only the empty owned entry beneath this fd.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) {
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }

    fn remove_owned_directory(
        parent: &fs::File,
        name: &std::ffi::OsStr,
        directory: &fs::File,
    ) -> io::Result<()> {
        let current = beneath::open_beneath(parent, Path::new(name))?;
        if !same_directory(&current, directory)? {
            return Err(io::Error::other(
                "refusing FUSE cleanup: the owned directory was replaced",
            ));
        }
        for entry in beneath::read_dir_fd(directory.try_clone()?)? {
            let entry = entry?;
            let relative = Path::new(&entry.name);
            let stat = beneath::lstat_beneath(directory, relative)?;
            if stat.is_dir && !stat.is_symlink {
                let child = beneath::open_beneath(directory, relative)?;
                remove_owned_directory(directory, &entry.name, &child)?;
            } else {
                let name = component_name(&entry.name)?;
                // SAFETY: unlinkat only removes the named entry relative to
                // this pinned descriptor; it never follows a symlink.
                if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        let name = component_name(name)?;
        // SAFETY: the directory identity was checked above. AT_REMOVEDIR
        // cannot follow a replacement symlink out of the pinned parent.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Remove a registry-validated directory under a pinned storage boundary.
    /// Callers must establish ownership before passing the relative path.
    pub fn remove_registered_tree(root: &fs::File, relative: &Path) -> io::Result<()> {
        let parent_path = relative
            .parent()
            .ok_or_else(|| io::Error::other("FUSE cleanup path has no parent"))?;
        let name = relative
            .file_name()
            .ok_or_else(|| io::Error::other("FUSE cleanup path has no final component"))?;
        let parent = match beneath::open_beneath(root, parent_path) {
            Ok(parent) => parent,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let directory = match beneath::open_beneath(&parent, Path::new(name)) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        remove_owned_directory(&parent, name, &directory)
    }

    /// Remove only an empty managed mountpoint/container. An unexpected file
    /// keeps both the directory and its registration available for recovery.
    pub fn remove_empty_registered_directory(root: &fs::File, relative: &Path) -> io::Result<()> {
        let parent_path = relative
            .parent()
            .ok_or_else(|| io::Error::other("FUSE cleanup path has no parent"))?;
        let name = relative
            .file_name()
            .ok_or_else(|| io::Error::other("FUSE cleanup path has no final component"))?;
        let parent = match beneath::open_beneath(root, parent_path) {
            Ok(parent) => parent,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        match beneath::open_beneath(&parent, Path::new(name)) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        let name = component_name(name)?;
        // SAFETY: rmdir is descriptor-relative and refuses non-empty trees or
        // symlinks; unlike recursive cleanup it cannot delete unexpected data.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Hide only the managed container, preserving existing overlay metadata
    /// semantics. Without this opaque directory the lower tree exposes the
    /// mountpoint and upper directory back into their own overlay.
    pub fn prepare_worktree_mask(upper: &Directory) -> io::Result<()> {
        let mask = upper
            .child_target(Path::new(".libra/worktree"))?
            .create(true)?;
        mask.validate_path()?;
        // The add path requires a new managed root, so a retained upper is
        // rejected before reaching this setup step.
        if !mask.is_empty()? {
            return Err(io::Error::other(
                "FUSE worktree opacity mask must be empty before mounting",
            ));
        }

        #[cfg(target_os = "macos")]
        {
            use std::os::fd::FromRawFd;

            // libfuse-fs uses OCI whiteouts on macOS, so an xattr alone does
            // not mask the lower directory. Create its opaque marker through
            // the pinned mask descriptor, never replacing an existing entry.
            // SAFETY: mask is a live directory descriptor and the fixed leaf
            // is exclusively created without following a pre-existing link.
            let raw = unsafe {
                libc::openat(
                    mask.file.as_raw_fd(),
                    c".wh..wh..opq".as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if raw < 0 {
                return Err(io::Error::other(format!(
                    "cannot protect FUSE worktree against recursive mounts: OCI opaque marker is unavailable on '{}': {}",
                    mask.path.display(),
                    io::Error::last_os_error()
                )));
            }
            // SAFETY: openat returned a new owned file descriptor.
            let marker = unsafe { fs::File::from_raw_fd(raw) };
            marker.sync_all()?;
            mask.file.sync_all()?;
            Ok(())
        }

        #[cfg(target_os = "linux")]
        {
            let name = c"user.fuseoverlayfs.opaque";
            let value = b"y";
            // SAFETY: the directory fd and both byte buffers remain valid for the call.
            let set_result = unsafe {
                libc::fsetxattr(
                    mask.file.as_raw_fd(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            };
            if set_result != 0 {
                return Err(io::Error::other(format!(
                    "cannot protect FUSE worktree against recursive mounts: opaque xattr is unavailable on '{}': {}",
                    mask.path.display(),
                    io::Error::last_os_error()
                )));
            }
            let mut observed = [0_u8; 2];
            // SAFETY: observed is a writable buffer with the advertised length.
            let length = unsafe {
                libc::fgetxattr(
                    mask.file.as_raw_fd(),
                    name.as_ptr(),
                    observed.as_mut_ptr().cast(),
                    observed.len(),
                )
            };
            if length != 1 || observed[0] != b'y' {
                return Err(io::Error::other(
                    "cannot verify FUSE worktree opaque xattr; refusing to mount",
                ));
            }
            Ok(())
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "FUSE managed worktrees require an opaque-directory marker",
        ))
    }

    /// Bind the mounted metadata to the registered upper directory. Exclusive
    /// creation refuses a marker injected or left by another worktree.
    pub fn prepare_mount_binding(upper: &Directory, mount_id: &str) -> io::Result<()> {
        use std::{io::Write, os::unix::io::FromRawFd};

        uuid::Uuid::parse_str(mount_id)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let metadata = beneath::open_beneath(&upper.file, Path::new(".libra"))?;
        // SAFETY: metadata is a live directory descriptor; the fixed leaf is
        // exclusively created without following a pre-existing link.
        let raw = unsafe {
            libc::openat(
                metadata.as_raw_fd(),
                c"worktree-mount-id".as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned file descriptor.
        let mut file = unsafe { fs::File::from_raw_fd(raw) };
        writeln!(file, "{mount_id}")?;
        file.sync_all()?;
        metadata.sync_all()
    }
    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::symlink;

        use tempfile::tempdir;

        use super::*;

        #[test]
        fn managed_target_blocks_an_intermediate_symlink_swap() {
            let temp = tempdir().expect("tempdir");
            let root = temp.path().canonicalize().expect("canonical root");
            fs::create_dir(root.join("parent")).expect("parent");
            fs::create_dir(root.join("outside")).expect("outside");
            let target =
                Target::beneath(&root, &root.join("parent/workspace")).expect("pin target");
            fs::rename(root.join("parent"), root.join("retained")).expect("rename parent");
            symlink(root.join("outside"), root.join("parent")).expect("swap symlink");
            assert!(
                target.create(true).is_err(),
                "creation must not follow the replacement parent"
            );
            assert!(!root.join("outside/workspace").exists());
        }

        #[test]
        fn rollback_preserves_preexisting_target_and_removes_only_owned_parents() {
            let temp = tempdir().expect("tempdir");
            let existing = temp.path().join("existing");
            fs::create_dir(&existing).expect("existing target");
            Target::resolve(&existing, temp.path())
                .expect("resolve")
                .create(false)
                .expect("pin existing")
                .rollback()
                .expect("rollback existing");
            assert!(existing.is_dir());
            let created = Target::resolve(Path::new("new/parent/workspace"), temp.path())
                .expect("resolve missing ancestors")
                .create(true)
                .expect("create");
            assert_eq!(
                created.path,
                temp.path()
                    .canonicalize()
                    .expect("pinned base")
                    .join("new/parent/workspace"),
                "relative targets use the invoking workdir rather than process cwd"
            );
            fs::write(created.path.join("owned.txt"), b"owned").expect("owned contents");
            created.rollback().expect("rollback owned");
            assert!(!temp.path().join("new").exists());
            assert!(existing.is_dir());
        }

        #[test]
        fn rollback_does_not_follow_a_replacement_or_nested_symlink() {
            let temp = tempdir().expect("tempdir");
            let outside = temp.path().join("outside");
            fs::create_dir(&outside).expect("outside");
            fs::write(outside.join("keep"), b"keep").expect("keep");
            let created = Target::resolve(&temp.path().join("owned"), temp.path())
                .expect("resolve")
                .create(true)
                .expect("create");
            symlink(&outside, created.path.join("nested")).expect("nested symlink");
            created.rollback().expect("unlink owned nested symlink");
            assert_eq!(
                fs::read(outside.join("keep")).expect("outside intact"),
                b"keep"
            );
            let replaced = Target::resolve(&temp.path().join("replaced"), temp.path())
                .expect("resolve")
                .create(true)
                .expect("create");
            fs::rename(&replaced.path, temp.path().join("retained")).expect("move owned dir");
            symlink(&outside, &replaced.path).expect("replacement symlink");
            assert!(replaced.rollback().is_err());
            assert_eq!(
                fs::read(outside.join("keep")).expect("outside intact"),
                b"keep"
            );
        }

        #[test]
        fn registered_cleanup_stays_under_the_original_storage_descriptor() {
            let temp = tempdir().expect("tempdir");
            let common = temp.path().join("common");
            fs::create_dir_all(common.join("owned/upper")).expect("original storage");
            fs::write(common.join("owned/upper/data"), b"owned").expect("owned data");
            let root = beneath::open_root(&common).expect("pin storage");
            let retained = temp.path().join("retained");
            fs::rename(&common, &retained).expect("rename original storage");
            fs::create_dir_all(common.join("owned/upper")).expect("replacement storage");
            fs::write(common.join("owned/upper/keep"), b"keep").expect("replacement data");

            remove_registered_tree(&root, Path::new("owned/upper")).expect("remove owned upper");
            remove_empty_registered_directory(&root, Path::new("owned"))
                .expect("remove owned container");
            assert!(!retained.join("owned").exists());
            assert_eq!(
                fs::read(common.join("owned/upper/keep")).expect("replacement preserved"),
                b"keep"
            );
        }

        #[test]
        fn opacity_mask_is_prepared_before_mounting() {
            let temp = tempdir().expect("tempdir");
            let upper = Target::resolve(&temp.path().join("upper"), temp.path())
                .expect("resolve")
                .create(true)
                .expect("create upper");
            prepare_worktree_mask(&upper).expect("set and read back opaque xattr");
            let mount_id = uuid::Uuid::new_v4().to_string();
            prepare_mount_binding(&upper, &mount_id).expect("write mount binding");
            assert_eq!(
                fs::read_to_string(upper.path.join(".libra/worktree-mount-id"))
                    .expect("mount binding"),
                format!("{mount_id}\n")
            );
            assert!(
                prepare_mount_binding(&upper, &uuid::Uuid::new_v4().to_string()).is_err(),
                "a mount binding must never overwrite an existing marker"
            );
            let mask_entries: Vec<_> = fs::read_dir(upper.path.join(".libra/worktree"))
                .expect("mask")
                .map(|entry| {
                    entry
                        .expect("mask entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            #[cfg(target_os = "macos")]
            assert_eq!(mask_entries, [".wh..wh..opq"]);
            #[cfg(not(target_os = "macos"))]
            assert!(mask_entries.is_empty());
            assert!(
                !upper.path.join(".libra/commondir").exists(),
                "the mask must not invent a new worktree identity"
            );
            upper.rollback().expect("rollback");
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FuseTaskWorktreeSweepReport {
    pub scanned: usize,
    pub cleaned: usize,
    pub skipped_live_owner: usize,
    pub failures: Vec<FuseTaskWorktreeSweepFailure>,
}

impl FuseTaskWorktreeSweepReport {
    pub fn is_empty(&self) -> bool {
        self.scanned == 0
            && self.cleaned == 0
            && self.skipped_live_owner == 0
            && self.failures.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuseTaskWorktreeSweepFailure {
    pub path: PathBuf,
    pub message: String,
}

pub fn normalize_abs_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new(comp.as_os_str())),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

pub fn is_mount_active(mountpoint: &Path) -> bool {
    let target = normalize_abs_path(mountpoint);

    #[cfg(target_os = "linux")]
    {
        mountinfo_contains_mountpoint(&target)
    }

    #[cfg(not(target_os = "linux"))]
    {
        mount_command_contains_mountpoint(&target)
    }
}

pub fn unmount_path(path: &Path) -> io::Result<()> {
    unmount_path_inner(path, false)
}

pub fn force_unmount_path(path: &Path) -> io::Result<()> {
    unmount_path_inner(path, true)
}

fn unmount_path_inner(path: &Path, force: bool) -> io::Result<()> {
    let target = normalize_abs_path(path);
    if !force && !is_mount_active(&target) {
        return Ok(());
    }

    let mut errors = Vec::new();
    for (program, args) in unmount_commands(&target) {
        match Command::new(program).args(&args).output() {
            Ok(output) if output.status.success() => {
                if !is_mount_active(&target) {
                    return Ok(());
                }
                errors.push(format!(
                    "{} succeeded but {} is still mounted",
                    format_command(program, &args),
                    target.display()
                ));
            }
            Ok(output) => {
                errors.push(format!(
                    "{} exited with status {}{}",
                    format_command(program, &args),
                    output.status,
                    command_output_suffix(&output.stderr)
                ));
            }
            Err(err) => {
                errors.push(format!(
                    "{} failed to start: {}",
                    format_command(program, &args),
                    err
                ));
            }
        }
    }

    if !is_mount_active(&target) {
        return Ok(());
    }

    Err(io::Error::other(format!(
        "failed to unmount FUSE path {}: {}",
        target.display(),
        errors.join("; ")
    )))
}

pub fn resolve_task_worktree_mountpoint_arg(path: &Path) -> PathBuf {
    if path.file_name().and_then(|name| name.to_str()) == Some("workspace") {
        return path.to_path_buf();
    }

    let workspace = path.join("workspace");
    if is_fuse_task_worktree_cleanup_root(path) && workspace.exists() {
        workspace
    } else {
        path.to_path_buf()
    }
}

pub fn fuse_task_worktree_cleanup_root(mountpoint: &Path) -> Option<PathBuf> {
    if mountpoint.file_name().and_then(|name| name.to_str()) != Some("workspace") {
        return None;
    }
    let cleanup_root = mountpoint.parent()?;
    // Managed named FUSE worktrees share this historical prefix space, but
    // their upper layer and registry must only be removed by `worktree remove`.
    let fuse_container = cleanup_root.parent()?;
    if fuse_container
        .file_name()
        .is_some_and(|name| name == "fuse")
        && fuse_container
            .parent()
            .is_some_and(|path| path.file_name().is_some_and(|name| name == "worktree"))
        && fuse_container
            .parent()
            .and_then(Path::parent)
            .is_some_and(|path| path.file_name().is_some_and(|name| name == ".libra"))
    {
        return None;
    }
    if is_fuse_task_worktree_cleanup_root(cleanup_root) {
        Some(cleanup_root.to_path_buf())
    } else {
        None
    }
}

fn is_fuse_task_worktree_cleanup_root(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(FUSE_TASK_WORKTREE_PREFIX))
}

/// Returns the FUSE task-worktree cleanup root that contains `path`, if any.
///
/// Only matches when `path` is at or below `<cleanup_root>/workspace` — the
/// FUSE mount point. Other siblings of the cleanup root (`upper/`, `lower/`,
/// the cleanup root itself) and unrelated nested directories whose basenames
/// happen to share the FUSE prefix are excluded.
///
/// Symlinked paths are resolved via `fs::canonicalize` when possible so that
/// callers passing `~/.../workspace` indirections still match. When
/// canonicalization fails (e.g. the path does not exist on disk, as in unit
/// tests with synthetic fixtures), we fall back to lexical normalization.
pub fn enclosing_fuse_task_worktree_root(path: &Path) -> Option<PathBuf> {
    let normalized = fs::canonicalize(path).unwrap_or_else(|_| normalize_abs_path(path));
    for ancestor in normalized.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) != Some("workspace") {
            continue;
        }
        if let Some(parent) = ancestor.parent()
            && is_fuse_task_worktree_cleanup_root(parent)
        {
            return Some(parent.to_path_buf());
        }
    }
    None
}

/// Returns a stable, FUSE-external cargo target directory for builds rooted at
/// `path` if and only if `path` is inside a FUSE task worktree.
///
/// The libfuse-fs overlay used to back task worktrees rejects some directory
/// creation operations with `EPERM`, which breaks `cargo` (it cannot create
/// `./target` inside the workspace). Redirecting `CARGO_TARGET_DIR` outside the
/// FUSE mount lets builds proceed without changing the workspace contents.
///
/// The path is deterministic per worktree so successive builds reuse the same
/// incremental cache, and the worktree id keeps separate task worktrees from
/// stomping on each other.
pub fn fuse_workspace_cargo_target_dir(path: &Path) -> Option<PathBuf> {
    let cleanup_root = enclosing_fuse_task_worktree_root(path)?;
    let worktree_id = cleanup_root.file_name()?.to_string_lossy().into_owned();
    let mut dir = std::env::temp_dir();
    dir.push("libra-fuse-cargo-target");
    dir.push(worktree_id);
    Some(dir)
}

pub fn sweep_repo_fuse_task_worktrees(
    repo_working_dir: &Path,
) -> io::Result<FuseTaskWorktreeSweepReport> {
    let storage_path = util::try_get_storage_path(Some(repo_working_dir.to_path_buf()))?;
    sweep_fuse_task_worktrees_dir(&storage_path.join("worktrees").join("tasks"))
}

pub fn sweep_fuse_task_worktrees_dir(tasks_dir: &Path) -> io::Result<FuseTaskWorktreeSweepReport> {
    let mut report = FuseTaskWorktreeSweepReport::default();
    if !tasks_dir.exists() {
        return Ok(report);
    }

    for entry in fs::read_dir(tasks_dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                report.failures.push(FuseTaskWorktreeSweepFailure {
                    path: tasks_dir.to_path_buf(),
                    message: err.to_string(),
                });
                continue;
            }
        };
        let root = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                report.failures.push(FuseTaskWorktreeSweepFailure {
                    path: root,
                    message: err.to_string(),
                });
                continue;
            }
        };
        if !file_type.is_dir() || !is_fuse_task_worktree_cleanup_root(&root) {
            continue;
        }

        report.scanned += 1;
        if let Some(pid) = task_worktree_owner_pid(&root)
            && pid != std::process::id()
            && pid_is_live(pid)
        {
            report.skipped_live_owner += 1;
            continue;
        }

        if let Err(err) = unmount_and_remove_fuse_task_worktree(&root) {
            report.failures.push(FuseTaskWorktreeSweepFailure {
                path: root,
                message: err.to_string(),
            });
        } else {
            report.cleaned += 1;
        }
    }

    Ok(report)
}

fn unmount_and_remove_fuse_task_worktree(root: &Path) -> io::Result<()> {
    let workspace = root.join("workspace");
    if workspace.exists() || is_mount_active(&workspace) {
        force_unmount_path(&workspace)?;
    }
    fs::remove_dir_all(root)
}

fn task_worktree_owner_pid(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let suffix = name.strip_prefix(FUSE_TASK_WORKTREE_PREFIX)?;
    let pid = suffix.split('-').next()?;
    pid.parse().ok()
}

#[cfg(unix)]
fn pid_is_live(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    // SAFETY: kill(pid, 0) performs a liveness probe without sending a signal.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }

    match io::Error::last_os_error().raw_os_error() {
        Some(code) if code == libc::ESRCH => false,
        Some(code) if code == libc::EPERM => true,
        _ => false,
    }
}

#[cfg(not(unix))]
fn pid_is_live(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn mountinfo_contains_mountpoint(target: &Path) -> bool {
    let Ok(content) = fs::read_to_string("/proc/self/mountinfo") else {
        return mount_command_contains_mountpoint(target);
    };
    let target = target.to_string_lossy();
    content.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let _ = parts.next();
        let _ = parts.next();
        let _ = parts.next();
        let _ = parts.next();
        match parts.next() {
            Some(mountpoint) => decode_mount_escaped(mountpoint) == target,
            None => false,
        }
    })
}

fn mount_command_contains_mountpoint(target: &Path) -> bool {
    let Ok(output) = Command::new("mount").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let target = target.to_string_lossy();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(mount_command_mountpoint)
        .any(|mountpoint| normalize_abs_path(Path::new(mountpoint)).to_string_lossy() == target)
}

fn mount_command_mountpoint(line: &str) -> Option<&str> {
    let (_, after_on) = line.split_once(" on ")?;
    let (mountpoint, _) = after_on.rsplit_once(" (")?;
    Some(mountpoint)
}

#[cfg(any(target_os = "linux", test))]
fn decode_mount_escaped(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn unmount_commands(target: &Path) -> Vec<(&'static str, Vec<OsString>)> {
    let target = target.as_os_str().to_os_string();
    let mut commands = Vec::new();

    #[cfg(target_os = "linux")]
    {
        commands.push(("fusermount3", vec![OsString::from("-u"), target.clone()]));
        commands.push(("fusermount", vec![OsString::from("-u"), target.clone()]));
    }

    #[cfg(target_os = "macos")]
    {
        commands.push(("/sbin/umount", vec![target.clone()]));
    }

    commands.push(("umount", vec![target]));
    commands
}

fn format_command(program: &str, args: &[OsString]) -> String {
    let mut rendered = program.to_string();
    for arg in args {
        rendered.push(' ');
        rendered.push_str(&arg.to_string_lossy());
    }
    rendered
}

fn command_output_suffix(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parses_mount_command_mountpoint_with_spaces() {
        let line = "macfuse0 on /Volumes/Data/linked path/workspace (macfuse, local)";

        assert_eq!(
            mount_command_mountpoint(line),
            Some("/Volumes/Data/linked path/workspace")
        );
    }

    #[test]
    fn decodes_linux_mountinfo_escapes() {
        assert_eq!(
            decode_mount_escaped("/tmp/libra\\040task\\011workspace\\134x"),
            "/tmp/libra task\tworkspace\\x"
        );
    }

    #[test]
    fn resolves_task_worktree_mountpoint_from_cleanup_root() {
        let temp = tempdir().expect("create temp dir");
        let root = temp
            .path()
            .join("libra-task-worktree-fuse-123-019ddec6-de60-7383");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("create workspace");

        assert_eq!(resolve_task_worktree_mountpoint_arg(&root), workspace);
    }

    #[test]
    fn keeps_workspace_mountpoint_arg_unchanged() {
        let temp = tempdir().expect("create temp dir");
        let workspace = temp
            .path()
            .join("libra-task-worktree-fuse-123-019ddec6-de60-7383")
            .join("workspace");

        assert_eq!(resolve_task_worktree_mountpoint_arg(&workspace), workspace);
    }

    #[test]
    fn detects_fuse_task_worktree_cleanup_root_from_workspace() {
        let mountpoint =
            Path::new("/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id/workspace");

        assert_eq!(
            fuse_task_worktree_cleanup_root(mountpoint),
            Some(PathBuf::from(
                "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id"
            ))
        );
        assert_eq!(
            fuse_task_worktree_cleanup_root(Path::new("/tmp/workspace")),
            None
        );
        assert_eq!(
            fuse_task_worktree_cleanup_root(Path::new(
                "/repo/.libra/worktree/fuse/libra-task-worktree-fuse-collision/workspace"
            )),
            None,
            "legacy cleanup must not classify managed named FUSE storage"
        );
    }

    #[test]
    fn enclosing_fuse_task_worktree_root_walks_to_cleanup_dir() {
        let cwd = Path::new(
            "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d/workspace/src/main.rs",
        );
        assert_eq!(
            enclosing_fuse_task_worktree_root(cwd),
            Some(PathBuf::from(
                "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d"
            ))
        );
    }

    #[test]
    fn enclosing_fuse_task_worktree_root_matches_workspace_dir_itself() {
        let cwd =
            Path::new("/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d/workspace");
        assert_eq!(
            enclosing_fuse_task_worktree_root(cwd),
            Some(PathBuf::from(
                "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d"
            ))
        );
    }

    #[test]
    fn enclosing_fuse_task_worktree_root_ignores_cleanup_root_itself() {
        // Only `<cleanup_root>/workspace[/...]` is the FUSE mount point.
        // Passing the cleanup root directly is not "inside the worktree" for
        // build purposes — that path holds `lower/`, `upper/` and the
        // unmount-time scaffolding rather than the user's cwd.
        let cwd = Path::new("/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d");
        assert_eq!(enclosing_fuse_task_worktree_root(cwd), None);
    }

    #[test]
    fn enclosing_fuse_task_worktree_root_ignores_overlay_layers() {
        for layer in ["upper", "lower"] {
            let cwd = PathBuf::from(format!(
                "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d/{layer}/main.rs"
            ));
            assert_eq!(
                enclosing_fuse_task_worktree_root(&cwd),
                None,
                "layer {layer} must not be treated as the FUSE mount point"
            );
        }
    }

    #[test]
    fn enclosing_fuse_task_worktree_root_picks_outer_when_workspace_contains_prefix_dir() {
        // A workspace might happen to contain a directory whose basename
        // re-uses the FUSE prefix (the user could check it in by accident).
        // The detection must still resolve to the *outer* worktree's cleanup
        // root, not the inner basename, because only the outer path is the
        // real FUSE mount.
        let cwd = Path::new(
            "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d/workspace/foo/libra-task-worktree-fuse-99-zz/bar.rs",
        );
        assert_eq!(
            enclosing_fuse_task_worktree_root(cwd),
            Some(PathBuf::from(
                "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d"
            ))
        );
    }

    #[test]
    fn enclosing_fuse_task_worktree_root_ignores_copy_backend() {
        let cwd = Path::new(
            "/repo/.libra/worktrees/tasks/libra-task-worktree-copy-7-019d/workspace/src/main.rs",
        );
        assert_eq!(enclosing_fuse_task_worktree_root(cwd), None);
    }

    #[test]
    fn enclosing_fuse_task_worktree_root_returns_none_outside_worktree() {
        assert_eq!(
            enclosing_fuse_task_worktree_root(Path::new("/repo/src/main.rs")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn enclosing_fuse_task_worktree_root_resolves_symlinked_workspace() {
        // Real worktrees on macOS often live under `/var/folders/...` while a
        // friendlier symlink may be advertised via `/tmp` (which itself is a
        // symlink to `/private/tmp`). Canonicalisation must still reach the
        // FUSE prefix in the resolved path.
        let temp = tempdir().expect("create temp dir");
        let cleanup_root = temp.path().join("libra-task-worktree-fuse-13-019sym");
        let workspace = cleanup_root.join("workspace");
        fs::create_dir_all(workspace.join("src")).expect("create workspace tree");
        let symlink = temp.path().join("symlinked-cwd");
        std::os::unix::fs::symlink(&workspace, &symlink).expect("create symlink");

        let resolved =
            enclosing_fuse_task_worktree_root(&symlink).expect("symlinked cwd must resolve");
        let expected = fs::canonicalize(&cleanup_root).unwrap_or(cleanup_root);
        assert_eq!(resolved, expected);
    }

    #[test]
    fn fuse_workspace_cargo_target_dir_returns_stable_per_worktree_path() {
        let cwd =
            Path::new("/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d/workspace/src");
        let dir = fuse_workspace_cargo_target_dir(cwd).expect("FUSE worktree must yield a dir");
        let expected = std::env::temp_dir()
            .join("libra-fuse-cargo-target")
            .join("libra-task-worktree-fuse-7-019d");
        assert_eq!(dir, expected);

        // Same worktree → same dir, regardless of subpath depth (incremental
        // builds rely on this).
        let nested = Path::new(
            "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-7-019d/workspace/deep/nest/x.rs",
        );
        assert_eq!(fuse_workspace_cargo_target_dir(nested), Some(expected));
    }

    #[test]
    fn fuse_workspace_cargo_target_dir_returns_none_outside_fuse_worktree() {
        assert_eq!(
            fuse_workspace_cargo_target_dir(Path::new("/repo/src/main.rs")),
            None
        );
    }

    #[test]
    fn parses_task_worktree_owner_pid() {
        let root =
            Path::new("/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-019ddec6-id");

        assert_eq!(task_worktree_owner_pid(root), Some(29353));
        assert_eq!(
            task_worktree_owner_pid(Path::new(
                "/repo/.libra/worktrees/tasks/libra-task-worktree-copy-29353-id"
            )),
            None
        );
    }

    #[test]
    fn sweep_fuse_task_worktrees_dir_allows_missing_directory() {
        let temp = tempdir().expect("create temp dir");
        let report =
            sweep_fuse_task_worktrees_dir(&temp.path().join("missing")).expect("sweep succeeds");

        assert!(report.is_empty());
    }

    #[test]
    fn sweep_fuse_task_worktrees_dir_cleans_stale_fuse_roots() {
        let temp = tempdir().expect("create temp dir");
        let tasks_dir = temp.path().join("tasks");
        let root = tasks_dir.join("libra-task-worktree-fuse-999999-019ddec6-de60-7383");
        fs::create_dir_all(root.join("workspace")).expect("create workspace");

        let report = sweep_fuse_task_worktrees_dir(&tasks_dir).expect("sweep succeeds");

        assert_eq!(report.scanned, 1);
        assert_eq!(report.cleaned, 1);
        assert_eq!(report.skipped_live_owner, 0);
        assert!(report.failures.is_empty());
        assert!(!root.exists());
    }

    #[test]
    fn sweep_fuse_task_worktrees_dir_ignores_non_fuse_roots() {
        let temp = tempdir().expect("create temp dir");
        let tasks_dir = temp.path().join("tasks");
        let copy_root = tasks_dir.join("libra-task-worktree-copy-999999-019ddec6-de60-7383");
        let regular_root = tasks_dir.join("regular");
        fs::create_dir_all(copy_root.join("workspace")).expect("create copy workspace");
        fs::create_dir_all(&regular_root).expect("create regular root");

        let report = sweep_fuse_task_worktrees_dir(&tasks_dir).expect("sweep succeeds");

        assert!(report.is_empty());
        assert!(copy_root.exists());
        assert!(regular_root.exists());
    }

    #[test]
    fn sweep_fuse_task_worktrees_dir_skips_live_foreign_owner() {
        let temp = tempdir().expect("create temp dir");
        let tasks_dir = temp.path().join("tasks");
        let current_pid = std::process::id();
        let live_foreign_pid = if current_pid == 1 { 2 } else { 1 };
        let root = tasks_dir.join(format!(
            "libra-task-worktree-fuse-{live_foreign_pid}-019ddec6-de60-7383"
        ));
        fs::create_dir_all(root.join("workspace")).expect("create workspace");

        let report = sweep_fuse_task_worktrees_dir(&tasks_dir).expect("sweep succeeds");

        if pid_is_live(live_foreign_pid) {
            assert_eq!(report.scanned, 1);
            assert_eq!(report.cleaned, 0);
            assert_eq!(report.skipped_live_owner, 1);
            assert!(root.exists());
        } else {
            assert_eq!(report.cleaned, 1);
            assert!(!root.exists());
        }
    }
}
