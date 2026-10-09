//! Read-only checkpoint-input materialization (plan-20260714 PD-02).
//!
//! A checkpoint-scoped `libra review --checkpoint <id>` /
//! `libra investigate --checkpoint <id>` run does NOT review the working
//! tree: the reviewers'/investigators' whole workspace is the checkpoint's
//! captured content — metadata, manifest, transcript parts — materialized
//! as READ-ONLY files inside the run directory
//! (`<run_dir>/checkpoint-input/`). This is deliberately not disguised as
//! a worktree diff: the materialized tree mirrors the checkpoint's inner
//! tree byte-for-byte, and the scoped prompt tells the agent it is
//! looking at a captured transcript, not a repository snapshot.
//!
//! Lifecycle / retention: the materialization lives inside the run
//! directory, so it shares the run's lifecycle exactly — `review clean` /
//! `investigate clean` remove it with the run, the orphaned-run cancel
//! path releases it through the recorded `workspace_root`, and
//! `agent doctor` needs no new orphan class (there is no storage outside
//! the run directory; the durable source of truth remains the checkpoint
//! objects themselves).
//!
//! The spec is produced by the command layer (which owns checkpoint
//! layout knowledge and fails closed BEFORE any run exists when the
//! checkpoint is missing, malformed, or not locally materializable);
//! this module only turns an already-validated spec into files.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::utils::object::read_git_object_bounded;

/// Directory name of the materialized input inside the run directory.
pub const CHECKPOINT_INPUT_DIR: &str = "checkpoint-input";

/// Per-file byte cap. A transcript part larger than this fails the
/// materialization closed (corrupt or hostile checkpoint) rather than
/// filling the disk.
pub const CHECKPOINT_INPUT_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Total materialized-bytes cap across every file of one checkpoint.
pub const CHECKPOINT_INPUT_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// One file of the checkpoint's inner tree, identified by its blob oid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointInputFile {
    /// Path relative to the checkpoint's inner tree root (`metadata.json`,
    /// `transcript/claude_code`, …), using `/` separators.
    pub rel_path: String,
    pub oid: String,
}

/// Validated materialization plan for one checkpoint — every listed blob
/// was confirmed locally present by the resolver before any run side
/// effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointInputSpec {
    pub checkpoint_id: String,
    pub files: Vec<CheckpointInputFile>,
}

/// Materialize `spec` under `<run_dir>/checkpoint-input/`, returning the
/// materialized root. Files are written read-only (0444 on Unix); any
/// failure returns a redacted, human-readable reason (the caller records
/// it as the run's `infra_error`).
pub fn materialize_checkpoint_input(
    storage: &Path,
    spec: &CheckpointInputSpec,
    run_dir: &Path,
) -> Result<PathBuf, String> {
    let root = run_dir.join(CHECKPOINT_INPUT_DIR);
    // Start from nothing. A paused investigate re-materializes into the
    // SAME run directory, so anything the previous turn's agent left here
    // — most importantly a symlink standing in for a file we are about to
    // write — must not survive into this one.
    match std::fs::remove_dir_all(&root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("failed to clear stale checkpoint input dir: {e}")),
    }
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("failed to create checkpoint input dir: {e}"))?;
    write_checkpoint_input_files(storage, spec, &root)?;
    Ok(root)
}

/// Production entry for investigate and review. Re-reads every blob from the
/// explicit absolute object store, clears any previous input through the
/// confined cleanup API, then writes the ordinary payload. A relative store
/// path is refused so a changed cwd cannot choose the bytes.
pub async fn materialize_validated_checkpoint_input(
    storage: &Path,
    spec: &CheckpointInputSpec,
    run_dir: &Path,
    deadline: std::time::Instant,
    cancelled: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<PathBuf, String> {
    if !storage.is_absolute() {
        return Err(
            "checkpoint input storage path must be absolute; refusing a cwd-relative store"
                .to_string(),
        );
    }
    if spec.checkpoint_id.is_empty() {
        return Err("checkpoint input spec is missing its checkpoint id".to_string());
    }
    let run_id = run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "checkpoint input run directory name is not valid".to_string())?;
    let runs_root = run_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "checkpoint input run directory has no trusted runs root".to_string())?;
    if !runs_root.is_absolute() {
        return Err(
            "checkpoint input runs root must be absolute; refusing a cwd-relative run".to_string(),
        );
    }
    let budget = scoped_io::ScopedIoBudget {
        deadline,
        cancelled,
    };
    let root_handle = scoped_io::open_scoped_run_root(runs_root, run_id, &budget)
        .map_err(|error| error.to_string())?;
    scoped_io::cleanup_checkpoint_input(&root_handle, &budget)
        .map_err(|error| error.to_string())?;
    let root = run_dir.join(CHECKPOINT_INPUT_DIR);
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("failed to create checkpoint input dir: {error}"))?;
    write_checkpoint_input_files(storage, spec, &root)?;
    Ok(root)
}

fn write_checkpoint_input_files(
    storage: &Path,
    spec: &CheckpointInputSpec,
    root: &Path,
) -> Result<(), String> {
    let mut total: u64 = 0;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for file in &spec.files {
        let rel = sanitize_rel_path(&file.rel_path)?;
        let oid = crate::internal::ai::util::parse_repo_object_id(&file.oid).map_err(|e| {
            format!(
                "invalid blob oid '{}' in checkpoint input spec: {e}",
                file.oid
            )
        })?;
        let (bytes, truncated) =
            read_git_object_bounded(storage, &oid, CHECKPOINT_INPUT_MAX_FILE_BYTES).map_err(
                |e| {
                    format!(
                        "checkpoint blob {} ({}) is not readable from the local object store: {e}",
                        file.oid, file.rel_path
                    )
                },
            )?;
        if truncated {
            return Err(format!(
                "checkpoint blob {} ({}) exceeds the {CHECKPOINT_INPUT_MAX_FILE_BYTES}-byte \
                 per-file cap; refusing to materialize",
                file.oid, file.rel_path
            ));
        }
        total = total.saturating_add(bytes.len() as u64);
        if total > CHECKPOINT_INPUT_MAX_TOTAL_BYTES {
            return Err(format!(
                "checkpoint {} materialization exceeds the \
                 {CHECKPOINT_INPUT_MAX_TOTAL_BYTES}-byte total cap; refusing",
                spec.checkpoint_id
            ));
        }
        let dest = root.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create checkpoint input subdir: {e}"))?;
            if parent != root {
                dirs.push(parent.to_path_buf());
            }
        }
        // `create_new` is the no-follow write: it fails if ANYTHING already
        // occupies the path, so a planted symlink is refused instead of
        // followed. Plain `fs::write` would open the link's target and
        // write through it, outside this directory.
        let mut handle = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| {
                format!(
                    "failed to create checkpoint input file {} (a path that already exists here \
                     is refused, never followed): {e}",
                    file.rel_path
                )
            })?;
        use std::io::Write as _;
        handle.write_all(&bytes).map_err(|e| {
            format!(
                "failed to write checkpoint input file {}: {e}",
                file.rel_path
            )
        })?;
        drop(handle);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o444)).map_err(
                |e| {
                    format!(
                        "failed to make checkpoint input file {} read-only: {e}",
                        file.rel_path
                    )
                },
            )?;
        }
    }
    // Read-only FILES in a writable DIRECTORY are not read-only input: the
    // agent could still unlink one and put a symlink in its place. Lock the
    // directories too, deepest first so a parent is never sealed before its
    // children are written.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        dirs.sort();
        dirs.dedup();
        for dir in dirs.iter().rev() {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555))
                .map_err(|e| format!("failed to make checkpoint input dir read-only: {e}"))?;
        }
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o555))
            .map_err(|e| format!("failed to make checkpoint input dir read-only: {e}"))?;
    }
    Ok(())
}

/// Reject absolute/parent-escaping components: the spec's rel paths come
/// from a checkpoint tree, but the materializer re-validates so a corrupt
/// tree can never write outside the input dir.
///
/// Checkpoint tree paths use `/`, but validating only `/` is not enough:
/// on Windows `\\` is ALSO a separator, so a single `..\evil` component
/// would survive a `/`-only split and then escape when pushed onto a
/// `PathBuf`. Every platform separator is rejected here, on every
/// platform, so a hostile tree cannot become a traversal on the one OS
/// the check was not written for. The result is re-verified through
/// `Path::components()`, which is the authority on what the OS will
/// actually do with the string.
pub(crate) fn sanitize_rel_path(rel: &str) -> Result<PathBuf, String> {
    let unsafe_component = |rel: &str| {
        Err(format!(
            "checkpoint input path '{rel}' contains an unsafe component; refusing"
        ))
    };
    if rel.is_empty() {
        return Err("checkpoint input path is empty; refusing".to_string());
    }
    // A drive-relative or UNC prefix (`C:x`, `\\?\…`) is absolute on
    // Windows and merely odd elsewhere; refuse it everywhere.
    if rel.contains(':') {
        return unsafe_component(rel);
    }
    let mut out = PathBuf::new();
    for component in rel.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.contains('\\')
            || component.contains('/')
        {
            return unsafe_component(rel);
        }
        out.push(component);
    }
    if out.as_os_str().is_empty() {
        return Err("checkpoint input path is empty; refusing".to_string());
    }
    // Belt and braces: whatever the string looked like, the OS must see a
    // pure sequence of normal components.
    if !out
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return unsafe_component(rel);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a loose blob into `storage` and return its spec entry.
    #[cfg_attr(windows, allow(dead_code))]
    fn write_blob(storage: &Path, rel_path: &str, content: &[u8]) -> CheckpointInputFile {
        use std::io::Write as _;

        let blob = git_internal::internal::object::blob::Blob::from_content_bytes(content.to_vec());
        let oid = blob.id.to_string();
        let dir = storage.join("objects").join(&oid[..2]);
        std::fs::create_dir_all(&dir).unwrap();
        let mut raw = format!("blob {}\0", content.len()).into_bytes();
        raw.extend_from_slice(content);
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&raw).unwrap();
        std::fs::write(dir.join(&oid[2..]), encoder.finish().unwrap()).unwrap();
        CheckpointInputFile {
            rel_path: rel_path.to_string(),
            oid,
        }
    }

    /// PD-02: the materialized input must be READ-ONLY in the sense that
    /// matters — an agent must not be able to replace a file. Read-only
    /// files inside a writable directory are not that: the file can be
    /// unlinked and a symlink put in its place.
    #[cfg(unix)]
    #[test]
    fn materialized_input_locks_files_and_directories() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let spec = CheckpointInputSpec {
            checkpoint_id: "abcd".to_string(),
            files: vec![
                write_blob(&storage, "metadata.json", b"{}"),
                write_blob(&storage, "transcript/claude_code", b"hello"),
            ],
        };

        let root = materialize_checkpoint_input(&storage, &spec, &run_dir).expect("materialize");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(&root.join("metadata.json")),
            0o444,
            "files are read-only"
        );
        assert_eq!(
            mode(&root.join("transcript/claude_code")),
            0o444,
            "including nested ones"
        );
        assert_eq!(mode(&root), 0o555, "and the root directory is not writable");
        assert_eq!(
            mode(&root.join("transcript")),
            0o555,
            "nor is a subdirectory — otherwise a file could be swapped for a symlink"
        );

        // Restore write permission so the tempdir can be cleaned up.
        for p in [root.join("transcript"), root.clone()] {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// PD-02: a paused investigate re-materializes into the SAME run
    /// directory. Anything the previous turn's agent left behind — above
    /// all a symlink standing in for a file we are about to write — must
    /// not be written through.
    #[cfg(unix)]
    #[test]
    fn rematerialization_does_not_write_through_a_planted_symlink() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("storage");
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let spec = CheckpointInputSpec {
            checkpoint_id: "abcd".to_string(),
            files: vec![write_blob(&storage, "metadata.json", b"REAL")],
        };

        // First materialization, then simulate a hostile agent swapping the
        // file for a link that points outside the run directory.
        let root = materialize_checkpoint_input(&storage, &spec, &run_dir).expect("first");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"UNTOUCHED").unwrap();
        std::fs::remove_file(root.join("metadata.json")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("metadata.json")).unwrap();

        // Re-materialize, as a resumed run does.
        let root = materialize_checkpoint_input(&storage, &spec, &run_dir).expect("second");
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"UNTOUCHED",
            "the write must NOT have followed the planted link out of the run directory"
        );
        assert_eq!(
            std::fs::read(root.join("metadata.json")).unwrap(),
            b"REAL",
            "and the real content is materialized in its place"
        );
        assert!(
            !std::fs::symlink_metadata(root.join("metadata.json"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the planted link is gone, not reused"
        );
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn sanitize_rejects_escapes() {
        for bad in [
            "../x",
            "a/../b",
            "/abs",
            "a//b",
            "",
            ".",
            // Windows separators and prefixes: rejected on EVERY platform,
            // so a hostile checkpoint cannot become a traversal on the one
            // OS the check was not written for.
            "..\\evil",
            "a\\..\\b",
            "\\\\server\\share",
            "C:/abs",
            "C:evil",
        ] {
            assert!(sanitize_rel_path(bad).is_err(), "{bad} must be rejected");
        }
        assert_eq!(
            sanitize_rel_path("transcript/claude_code").unwrap(),
            PathBuf::from("transcript/claude_code")
        );
    }

    #[test]
    fn fix_rg_scoped_04_readonly_cleanup() {
        scoped_io::test_support::readonly_cleanup();
    }

    #[test]
    fn fix_rg_scoped_04_metadata_boundary() {
        scoped_io::test_support::metadata_boundary();
    }

    #[test]
    fn fix_rg_scoped_04_preflight_budget() {
        scoped_io::test_support::preflight_budget();
    }

    #[test]
    fn fix_rg_scoped_04_deadline_cancel_owner() {
        scoped_io::test_support::deadline_cancel_owner();
    }

    fn validated_materialize(
        storage: &Path,
        spec: &CheckpointInputSpec,
        run_dir: &Path,
    ) -> Result<PathBuf, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(materialize_validated_checkpoint_input(
            storage,
            spec,
            run_dir,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            std::sync::Arc::new(|| false),
        ))
    }

    #[test]
    fn fix_rg_scoped_02_typed_materialization() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let storage = base.join("storage");
        let run_dir = base.join("runs").join("run-1");
        std::fs::create_dir_all(&run_dir).unwrap();
        let missing_id = CheckpointInputSpec {
            checkpoint_id: String::new(),
            files: Vec::new(),
        };
        let error = validated_materialize(&storage, &missing_id, &run_dir).unwrap_err();
        assert!(error.contains("checkpoint id"), "{error}");

        let spec = CheckpointInputSpec {
            checkpoint_id: "ckpt-typed".to_string(),
            files: vec![write_blob(&storage, "metadata.json", b"ORDINARY")],
        };
        let error =
            validated_materialize(Path::new("relative-storage"), &spec, &run_dir).unwrap_err();
        assert!(error.contains("absolute"), "{error}");

        let root = validated_materialize(&storage, &spec, &run_dir).expect("typed materialize");
        assert_eq!(
            std::fs::read(root.join("metadata.json")).unwrap(),
            b"ORDINARY"
        );
    }

    #[test]
    fn fix_rg_scoped_02_second_materialization() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let storage = base.join("storage");
        let run_dir = base.join("runs").join("run-1");
        std::fs::create_dir_all(&run_dir).unwrap();
        let spec = CheckpointInputSpec {
            checkpoint_id: "ckpt-second".to_string(),
            files: vec![write_blob(&storage, "metadata.json", b"ORDINARY")],
        };
        let first = validated_materialize(&storage, &spec, &run_dir).expect("first");
        assert_eq!(
            std::fs::read(first.join("metadata.json")).unwrap(),
            b"ORDINARY"
        );
        let second =
            validated_materialize(&storage, &spec, &run_dir).expect("second without chmod");
        assert_eq!(
            std::fs::read(second.join("metadata.json")).unwrap(),
            b"ORDINARY"
        );
    }
}

// Callers arrive in FIX-RG-SCOPED-02 and FIX-RG-SCOPED-03. This card
// delivers the API before those consumers exist, so production builds do
// not reference it yet.
#[allow(dead_code)]
mod scoped_io {
    //! Confined, bounded filesystem API, delivered before validating consumers.

    use std::{
        collections::BTreeMap,
        ffi::{OsStr, OsString},
        fs::File,
        io::{self, Read},
        path::{Component, Path, PathBuf},
        sync::Arc,
        time::Instant,
    };

    #[cfg(unix)]
    use crate::utils::beneath::EntryIdentityKey;
    use crate::utils::beneath::{EntryIdentity, EntryKind};

    const MAX_ENTRIES: usize = 8192;
    const MAX_FILES: usize = 4096;
    const MAX_DIRECTORIES: usize = 4096;
    const MAX_DEPTH: usize = 64;
    const MAX_PATH_BYTES: usize = 4096;
    const MAX_TOTAL_PATH_BYTES: usize = 8 * 1024 * 1024;
    const MAX_METADATA_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_TOTAL_METADATA_BYTES: u64 = 128 * 1024 * 1024;

    pub(crate) struct ScopedIoBudget {
        pub(crate) deadline: Instant,
        pub(crate) cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    }

    impl ScopedIoBudget {
        fn check(&self) -> Result<(), CheckpointInputIoError> {
            if (self.cancelled)() {
                return Err(CheckpointInputIoError::Cancelled);
            }
            if Instant::now() >= self.deadline {
                return Err(CheckpointInputIoError::Deadline);
            }
            Ok(())
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub(crate) enum CheckpointInputIoError {
        #[error(
            "checkpoint input operation was cancelled; keep the run and retry only after confirming cancellation has cleared"
        )]
        Cancelled,
        #[error(
            "checkpoint input operation reached its deadline; keep the run and inspect its bounded input before retrying"
        )]
        Deadline,
        #[error(
            "checkpoint input {action} failed: {source}; keep the run and inspect filesystem access before retrying"
        )]
        Io {
            action: &'static str,
            #[source]
            source: io::Error,
        },
        #[error(
            "checkpoint input confinement refused {reason}; keep the run and inspect links, identities and access manually"
        )]
        Refused { reason: &'static str },
        #[error(
            "checkpoint input {limit} budget exceeded; keep the run and inspect the oversized input manually"
        )]
        Budget { limit: &'static str },
        #[error(
            "checkpoint input identity changed during {action}; keep the run and inspect concurrent filesystem changes"
        )]
        Changed { action: &'static str },
    }

    fn io_error(action: &'static str, source: io::Error) -> CheckpointInputIoError {
        CheckpointInputIoError::Io { action, source }
    }

    fn refused(reason: &'static str) -> CheckpointInputIoError {
        CheckpointInputIoError::Refused { reason }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Snapshot {
        identity: EntryIdentity,
        len: u64,
        mode: u32,
        change_stamp: (i64, i64, i64, i64),
    }

    pub(crate) struct RunRootHandle {
        anchor: scoped_fs::RootAnchor,
    }

    /// Comparable metadata generation; an inode/file ID alone does not prove that
    /// the bytes read earlier are still the current state or manifest contents.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct MetadataIdentity {
        pub(crate) identity: EntryIdentity,
        pub(crate) len: u64,
        pub(crate) mode: u32,
        pub(crate) change_stamp: (i64, i64, i64, i64),
    }

    impl From<Snapshot> for MetadataIdentity {
        fn from(snapshot: Snapshot) -> Self {
            Self {
                identity: snapshot.identity,
                len: snapshot.len,
                mode: snapshot.mode,
                change_stamp: snapshot.change_stamp,
            }
        }
    }

    /// Bytes and physical identities from one pinned root; no lifecycle decision.
    pub(crate) struct RunMetadataBytes {
        pub(crate) state: Option<Vec<u8>>,
        pub(crate) manifest: Option<Vec<u8>>,
        pub(crate) root_identity: EntryIdentity,
        pub(crate) state_identity: Option<MetadataIdentity>,
        pub(crate) manifest_identity: Option<MetadataIdentity>,
    }

    pub(crate) fn open_scoped_run_root(
        trusted_runs_root: &Path,
        run_id: &str,
        budget: &ScopedIoBudget,
    ) -> Result<RunRootHandle, CheckpointInputIoError> {
        budget.check()?;
        if !trusted_runs_root.is_absolute() {
            return Err(refused("a trusted runs root that is not absolute"));
        }
        if trusted_runs_root.as_os_str().len() > MAX_PATH_BYTES
            || trusted_runs_root.components().count() > MAX_DEPTH
            || run_id.len() > MAX_PATH_BYTES
        {
            return Err(CheckpointInputIoError::Budget {
                limit: "trusted root path",
            });
        }
        if run_id.is_empty()
            || run_id.contains(['/', '\\', ':'])
            || !matches!(
                Path::new(run_id)
                    .components()
                    .collect::<Vec<_>>()
                    .as_slice(),
                [Component::Normal(_)]
            )
        {
            return Err(refused("an invalid run directory component"));
        }
        let anchor = scoped_fs::open_anchor(trusted_runs_root, run_id, budget)?;
        scoped_fs::verify_anchor(&anchor, budget)?;
        Ok(RunRootHandle { anchor })
    }

    fn checked_child(
        parent: &File,
        name: &OsStr,
        budget: &ScopedIoBudget,
    ) -> Result<Option<Snapshot>, CheckpointInputIoError> {
        budget.check()?;
        let snapshot = scoped_fs::snapshot_child(parent, name)?;
        budget.check()?;
        Ok(snapshot)
    }

    fn open_metadata(
        root: &RunRootHandle,
        name: &'static str,
        budget: &ScopedIoBudget,
    ) -> Result<Option<(File, Snapshot)>, CheckpointInputIoError> {
        let Some(snapshot) = checked_child(&root.anchor.directory, OsStr::new(name), budget)?
        else {
            return Ok(None);
        };
        if snapshot.identity.kind != EntryKind::File {
            return Err(refused("metadata that is not a regular no-follow file"));
        }
        if snapshot.len > MAX_METADATA_BYTES {
            return Err(CheckpointInputIoError::Budget {
                limit: "metadata per-file bytes",
            });
        }
        let file = scoped_fs::open_regular(&root.anchor.directory, OsStr::new(name))?;
        if scoped_fs::snapshot_handle(&file)? != snapshot {
            return Err(CheckpointInputIoError::Changed {
                action: "opening metadata",
            });
        }
        budget.check()?;
        Ok(Some((file, snapshot)))
    }

    fn read_metadata_file(
        entry: &mut Option<(File, Snapshot)>,
        budget: &ScopedIoBudget,
    ) -> Result<Option<Vec<u8>>, CheckpointInputIoError> {
        let Some((file, snapshot)) = entry.as_mut() else {
            return Ok(None);
        };
        let capacity = usize::try_from(snapshot.len)
            .map_err(|_| refused("metadata length outside the addressable byte budget"))?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            budget.check()?;
            let read = file
                .read(&mut buffer)
                .map_err(|e| io_error("reading metadata", e))?;
            if read == 0 {
                break;
            }
            if bytes.len().saturating_add(read) > MAX_METADATA_BYTES as usize {
                return Err(CheckpointInputIoError::Budget {
                    limit: "metadata per-file bytes",
                });
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        if scoped_fs::snapshot_handle(file)? != *snapshot || bytes.len() as u64 != snapshot.len {
            return Err(CheckpointInputIoError::Changed {
                action: "reading metadata",
            });
        }
        budget.check()?;
        Ok(Some(bytes))
    }

    pub(crate) fn read_run_metadata(
        root: &RunRootHandle,
        budget: &ScopedIoBudget,
    ) -> Result<RunMetadataBytes, CheckpointInputIoError> {
        scoped_fs::verify_anchor(&root.anchor, budget)?;
        let mut state = open_metadata(root, "state.json", budget)?;
        let mut manifest = open_metadata(root, "manifest.json", budget)?;
        let total = state.as_ref().map_or(0, |(_, snapshot)| snapshot.len)
            + manifest.as_ref().map_or(0, |(_, snapshot)| snapshot.len);
        if total > MAX_TOTAL_METADATA_BYTES {
            return Err(CheckpointInputIoError::Budget {
                limit: "metadata total bytes",
            });
        }
        let state_bytes = read_metadata_file(&mut state, budget)?;
        let manifest_bytes = read_metadata_file(&mut manifest, budget)?;
        for (name, entry) in [("state.json", &state), ("manifest.json", &manifest)] {
            let current = checked_child(&root.anchor.directory, OsStr::new(name), budget)?;
            if current != entry.as_ref().map(|(_, snapshot)| *snapshot) {
                return Err(CheckpointInputIoError::Changed {
                    action: "rechecking metadata leaves",
                });
            }
        }
        scoped_fs::verify_anchor(&root.anchor, budget)?;
        Ok(RunMetadataBytes {
            state: state_bytes,
            manifest: manifest_bytes,
            root_identity: root.anchor.identity,
            state_identity: state
                .as_ref()
                .map(|(_, snapshot)| MetadataIdentity::from(*snapshot)),
            manifest_identity: manifest
                .as_ref()
                .map(|(_, snapshot)| MetadataIdentity::from(*snapshot)),
        })
    }

    struct InputPlan {
        root: Snapshot,
        entries: BTreeMap<PathBuf, Snapshot>,
        child_counts: BTreeMap<PathBuf, usize>,
    }

    /// A descriptor pins an inode, not its current parent/name binding.
    struct DirectoryBinding {
        parent: File,
        parent_identity: EntryIdentity,
        name: OsString,
        child_identity: EntryIdentity,
    }

    /// Only the bounded current ancestry owns handles; the plan owns no handles.
    struct DirectoryChain {
        leaf: File,
        leaf_identity: EntryIdentity,
        bindings: Vec<DirectoryBinding>,
    }

    fn verify_directory_chain(chain: &DirectoryChain) -> Result<(), CheckpointInputIoError> {
        for binding in &chain.bindings {
            if scoped_fs::snapshot_handle(&binding.parent)?.identity != binding.parent_identity
                || scoped_fs::snapshot_child(&binding.parent, &binding.name)?.map(|s| s.identity)
                    != Some(binding.child_identity)
            {
                return Err(CheckpointInputIoError::Changed {
                    action: "rechecking input directory parent/name bindings",
                });
            }
        }
        if scoped_fs::snapshot_handle(&chain.leaf)?.identity != chain.leaf_identity {
            return Err(CheckpointInputIoError::Changed {
                action: "rechecking a held input directory binding",
            });
        }
        Ok(())
    }

    /// Called after every cooperative callback that precedes a Unix mutation.
    /// No cancellation callback is invoked after this final binding check and
    /// before the mutation syscall; the remaining kernel race is not atomic CAS.
    fn verify_mutation_bindings(
        root: &RunRootHandle,
        input: &Snapshot,
        chain: Option<&DirectoryChain>,
        _budget: &ScopedIoBudget,
    ) -> Result<(), CheckpointInputIoError> {
        #[cfg(unix)]
        scoped_fs::verify_anchor_bindings(&root.anchor)?;
        #[cfg(windows)]
        scoped_fs::verify_anchor(&root.anchor, _budget)?;
        if scoped_fs::snapshot_child(
            &root.anchor.directory,
            OsStr::new(super::CHECKPOINT_INPUT_DIR),
        )?
        .map(|s| s.identity)
            != Some(input.identity)
        {
            return Err(CheckpointInputIoError::Changed {
                action: "rechecking the named checkpoint-input root before mutation",
            });
        }
        if let Some(chain) = chain {
            verify_directory_chain(chain)?;
        }
        Ok(())
    }

    fn make_confined_directory_writable(
        root: &RunRootHandle,
        input: &Snapshot,
        chain: Option<&DirectoryChain>,
        directory: &File,
        expected: &Snapshot,
        budget: &ScopedIoBudget,
    ) -> Result<(), CheckpointInputIoError> {
        budget.check()?;
        verify_mutation_bindings(root, input, chain, budget)?;
        #[cfg(unix)]
        scoped_fs::make_directory_writable(directory, expected, budget, &|| {
            verify_mutation_bindings(root, input, chain, budget)
        })?;
        #[cfg(windows)]
        scoped_fs::make_directory_writable(directory, expected)?;
        budget.check()?;
        verify_mutation_bindings(root, input, chain, budget)
    }

    fn remove_confined_child(
        root: &RunRootHandle,
        input: &Snapshot,
        chain: Option<&DirectoryChain>,
        parent: &File,
        name: &OsStr,
        expected: &Snapshot,
        budget: &ScopedIoBudget,
    ) -> Result<(), CheckpointInputIoError> {
        budget.check()?;
        verify_mutation_bindings(root, input, chain, budget)?;
        #[cfg(unix)]
        scoped_fs::remove_child(parent, name, expected, budget, &|| {
            verify_mutation_bindings(root, input, chain, budget)
        })?;
        #[cfg(windows)]
        scoped_fs::remove_child(parent, name, expected, budget)?;
        // After deleting the input root itself the binding is intentionally
        // absent. Its caller rechecks the run-root and exact target absence.
        if chain.is_some() {
            verify_mutation_bindings(root, input, chain, budget)?;
        }
        Ok(())
    }

    fn open_plan_directory(
        input: &File,
        relative: &Path,
        plan: &InputPlan,
        budget: &ScopedIoBudget,
    ) -> Result<DirectoryChain, CheckpointInputIoError> {
        let mut current = input
            .try_clone()
            .map_err(|e| io_error("retaining the input root", e))?;
        if scoped_fs::snapshot_handle(&current)?.identity != plan.root.identity {
            return Err(CheckpointInputIoError::Changed {
                action: "opening the input root",
            });
        }
        let mut ancestors = Vec::new();
        let mut prefix = PathBuf::new();
        for component in relative.components() {
            budget.check()?;
            let Component::Normal(name) = component else {
                return Err(refused("a non-normal internal input path"));
            };
            prefix.push(name);
            let expected = plan
                .entries
                .get(&prefix)
                .ok_or_else(|| refused("an unplanned input ancestor"))?;
            if expected.identity.kind != EntryKind::Directory {
                return Err(refused("an input ancestor that is not a directory"));
            }
            let before = checked_child(&current, name, budget)?;
            if before.map(|s| s.identity) != Some(expected.identity) {
                return Err(CheckpointInputIoError::Changed {
                    action: "opening input ancestors",
                });
            }
            let child = scoped_fs::open_directory(&current, name)?;
            if scoped_fs::snapshot_handle(&child)?.identity != expected.identity {
                return Err(CheckpointInputIoError::Changed {
                    action: "pinning input ancestors",
                });
            }
            let parent_identity = scoped_fs::snapshot_handle(&current)?.identity;
            ancestors.push(DirectoryBinding {
                parent: current,
                parent_identity,
                name: name.to_os_string(),
                child_identity: expected.identity,
            });
            current = child;
        }
        budget.check()?;
        let leaf_identity = scoped_fs::snapshot_handle(&current)?.identity;
        let chain = DirectoryChain {
            leaf: current,
            leaf_identity,
            bindings: ancestors,
        };
        verify_directory_chain(&chain)?;
        Ok(chain)
    }

    fn directory_names(
        directory: &File,
        budget: &ScopedIoBudget,
    ) -> Result<Vec<OsString>, CheckpointInputIoError> {
        budget.check()?;
        let clone = directory
            .try_clone()
            .map_err(|e| io_error("retaining a directory for listing", e))?;
        let stream = crate::utils::beneath::read_dir_fd(clone)
            .map_err(|e| io_error("listing an input directory", e))?;
        let mut names = Vec::new();
        for entry in stream {
            budget.check()?;
            let entry = entry.map_err(|e| io_error("reading an input directory entry", e))?;
            if names.len() >= MAX_ENTRIES {
                return Err(CheckpointInputIoError::Budget { limit: "entries" });
            }
            if entry.name.to_str().is_none() {
                return Err(refused("an input name that is not UTF-8"));
            }
            if !matches!(
                Path::new(&entry.name)
                    .components()
                    .collect::<Vec<_>>()
                    .as_slice(),
                [Component::Normal(_)]
            ) {
                return Err(refused("a non-normal input directory entry"));
            }
            names.push(entry.name);
        }
        names.sort();
        Ok(names)
    }

    fn preflight_input(
        input: &File,
        root: Snapshot,
        budget: &ScopedIoBudget,
    ) -> Result<InputPlan, CheckpointInputIoError> {
        let mut plan = InputPlan {
            root,
            entries: BTreeMap::new(),
            child_counts: BTreeMap::new(),
        };
        let mut pending = vec![PathBuf::new()];
        let (mut files, mut directories, mut total_path_bytes) = (0_usize, 0_usize, 0_usize);
        while let Some(relative) = pending.pop() {
            let chain = open_plan_directory(input, &relative, &plan, budget)?;
            for name in directory_names(&chain.leaf, budget)? {
                budget.check()?;
                let path = relative.join(&name);
                if plan.entries.len() >= MAX_ENTRIES {
                    return Err(CheckpointInputIoError::Budget { limit: "entries" });
                }
                let depth = path.components().count();
                if depth > MAX_DEPTH {
                    return Err(CheckpointInputIoError::Budget { limit: "depth" });
                }
                // Names are UTF-8 and components are already checked. '/' is the
                // portable separator charged to the plan, including on Windows.
                let path_bytes = path
                    .to_str()
                    .ok_or_else(|| refused("an input path that is not UTF-8"))?
                    .len();
                if path_bytes > MAX_PATH_BYTES {
                    return Err(CheckpointInputIoError::Budget {
                        limit: "path bytes",
                    });
                }
                total_path_bytes = total_path_bytes.saturating_add(path_bytes);
                if total_path_bytes > MAX_TOTAL_PATH_BYTES {
                    return Err(CheckpointInputIoError::Budget {
                        limit: "total path bytes",
                    });
                }
                let snapshot = checked_child(&chain.leaf, &name, budget)?.ok_or(
                    CheckpointInputIoError::Changed {
                        action: "preflighting input entries",
                    },
                )?;
                match snapshot.identity.kind {
                    EntryKind::Directory => {
                        directories += 1;
                        if directories > MAX_DIRECTORIES {
                            return Err(CheckpointInputIoError::Budget {
                                limit: "directories",
                            });
                        }
                        pending.push(path.clone());
                    }
                    EntryKind::File | EntryKind::Symlink => {
                        files += 1;
                        if files > MAX_FILES {
                            return Err(CheckpointInputIoError::Budget { limit: "files" });
                        }
                    }
                    EntryKind::Other => {
                        return Err(refused(
                            "an input entry that is neither regular, directory nor link",
                        ));
                    }
                }
                if plan.entries.insert(path, snapshot).is_some() {
                    return Err(CheckpointInputIoError::Changed {
                        action: "listing duplicate input entries",
                    });
                }
                *plan.child_counts.entry(relative.clone()).or_default() += 1;
            }
        }
        Ok(plan)
    }

    fn verify_input_plan(
        input: &File,
        plan: &InputPlan,
        budget: &ScopedIoBudget,
    ) -> Result<(), CheckpointInputIoError> {
        let mut directories = vec![PathBuf::new()];
        directories.extend(plan.entries.iter().filter_map(|(path, snapshot)| {
            (snapshot.identity.kind == EntryKind::Directory).then_some(path.clone())
        }));
        for path in directories {
            let chain = open_plan_directory(input, &path, plan, budget)?;
            let names = directory_names(&chain.leaf, budget)?;
            let expected_count = plan.child_counts.get(&path).copied().unwrap_or(0);
            if names.len() != expected_count {
                return Err(CheckpointInputIoError::Changed {
                    action: "rechecking input directory membership",
                });
            }
            for name in names {
                let relative = path.join(&name);
                let expected =
                    plan.entries
                        .get(&relative)
                        .ok_or(CheckpointInputIoError::Changed {
                            action: "rechecking input directory membership",
                        })?;
                if checked_child(&chain.leaf, &name, budget)?.map(|s| s.identity)
                    != Some(expected.identity)
                {
                    return Err(CheckpointInputIoError::Changed {
                        action: "rechecking input entry identities",
                    });
                }
            }
        }
        Ok(())
    }

    pub(crate) fn cleanup_checkpoint_input(
        root: &RunRootHandle,
        budget: &ScopedIoBudget,
    ) -> Result<(), CheckpointInputIoError> {
        scoped_fs::verify_anchor(&root.anchor, budget)?;
        let name = OsStr::new(super::CHECKPOINT_INPUT_DIR);
        let Some(snapshot) = checked_child(&root.anchor.directory, name, budget)? else {
            scoped_fs::verify_anchor(&root.anchor, budget)?;
            if checked_child(&root.anchor.directory, name, budget)?.is_some() {
                return Err(CheckpointInputIoError::Changed {
                    action: "confirming absent checkpoint input",
                });
            }
            return Ok(());
        };
        if snapshot.identity.kind != EntryKind::Directory {
            return Err(refused(
                "a checkpoint-input root that is not a no-follow directory",
            ));
        }
        let input = scoped_fs::open_directory(&root.anchor.directory, name)?;
        if scoped_fs::snapshot_handle(&input)?.identity != snapshot.identity {
            return Err(CheckpointInputIoError::Changed {
                action: "pinning checkpoint input",
            });
        }
        let plan = preflight_input(&input, snapshot, budget)?;
        verify_input_plan(&input, &plan, budget)?;
        scoped_fs::verify_anchor(&root.anchor, budget)?;
        if checked_child(&root.anchor.directory, name, budget)?.map(|s| s.identity)
            != Some(snapshot.identity)
        {
            return Err(CheckpointInputIoError::Changed {
                action: "starting checkpoint-input cleanup",
            });
        }
        // Mutation starts here, only after the complete bounded read-only preflight.
        // Cancellation after this point returns an error and may leave partial work;
        // callers must never infer rollback or report successful removal from it.
        make_confined_directory_writable(root, &snapshot, None, &input, &snapshot, budget)?;
        let mut directories = plan
            .entries
            .iter()
            .filter_map(|(path, snapshot)| {
                (snapshot.identity.kind == EntryKind::Directory).then_some((path, snapshot))
            })
            .collect::<Vec<_>>();
        directories.sort_by_key(|(path, _)| path.components().count());
        for (path, expected) in &directories {
            scoped_fs::verify_anchor(&root.anchor, budget)?;
            if checked_child(&root.anchor.directory, name, budget)?.map(|s| s.identity)
                != Some(snapshot.identity)
            {
                return Err(CheckpointInputIoError::Changed {
                    action: "making checkpoint input directories writable",
                });
            }
            #[cfg(test)]
            test_support::set_mutation_phase("chmod");
            let chain = open_plan_directory(&input, path, &plan, budget)?;
            make_confined_directory_writable(
                root,
                &snapshot,
                Some(&chain),
                &chain.leaf,
                expected,
                budget,
            )?;
            // The reparent seam runs only after this input directory is owner-writable.
            // macOS refuses to rename a 0555 directory even when its parent is writable.
            #[cfg(test)]
            test_support::mark_reparent_callback_boundary(path);
            budget.check()?;
            drop(chain);
            let rechecked = open_plan_directory(&input, path, &plan, budget)?;
            if scoped_fs::snapshot_handle(&rechecked.leaf)?.identity != expected.identity {
                return Err(CheckpointInputIoError::Changed {
                    action: "rechecking writable input directory ancestry",
                });
            }
            budget.check()?;
        }
        let mut entries = plan.entries.iter().collect::<Vec<_>>();
        entries.sort_by(|(a, _), (b, _)| {
            b.components()
                .count()
                .cmp(&a.components().count())
                .then_with(|| b.cmp(a))
        });
        for (path, expected) in entries {
            scoped_fs::verify_anchor(&root.anchor, budget)?;
            if checked_child(&root.anchor.directory, name, budget)?.map(|s| s.identity)
                != Some(snapshot.identity)
            {
                return Err(CheckpointInputIoError::Changed {
                    action: "deleting checkpoint input",
                });
            }
            let parent = path
                .parent()
                .ok_or_else(|| refused("an input entry without a relative parent"))?;
            let leaf = path
                .file_name()
                .ok_or_else(|| refused("an input entry without a leaf name"))?;
            #[cfg(test)]
            test_support::set_mutation_phase("unlink");
            let chain = open_plan_directory(&input, parent, &plan, budget)?;
            #[cfg(test)]
            test_support::mark_reparent_callback_boundary(parent);
            budget.check()?;
            remove_confined_child(
                root,
                &snapshot,
                Some(&chain),
                &chain.leaf,
                leaf,
                expected,
                budget,
            )?;
            scoped_fs::verify_anchor(&root.anchor, budget)?;
        }
        if !directory_names(&input, budget)?.is_empty() {
            return Err(CheckpointInputIoError::Changed {
                action: "confirming an empty checkpoint-input root",
            });
        }
        // Windows holds ancestor handles without FILE_SHARE_DELETE. Release the
        // target's own handle before obtaining its DELETE handle; retain run/root.
        drop(input);
        scoped_fs::verify_anchor(&root.anchor, budget)?;
        remove_confined_child(
            root,
            &snapshot,
            None,
            &root.anchor.directory,
            name,
            &snapshot,
            budget,
        )?;
        scoped_fs::verify_anchor(&root.anchor, budget)?;
        if checked_child(&root.anchor.directory, name, budget)?.is_some() {
            return Err(CheckpointInputIoError::Changed {
                action: "confirming checkpoint-input removal",
            });
        }
        Ok(())
    }

    #[cfg(windows)]
    mod scoped_fs {
        use std::{
            ffi::{OsStr, c_void},
            fs::File,
            io,
            os::windows::{
                ffi::OsStrExt,
                fs::MetadataExt,
                io::{AsRawHandle, FromRawHandle},
            },
            path::{Component, Path, PathBuf},
            ptr,
        };

        use windows_sys::Win32::{
            Foundation::{
                HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE,
                RtlNtStatusToDosError, UNICODE_STRING,
            },
            Storage::FileSystem::{
                CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
                FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
                FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
                FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX,
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
                FILE_ID_128, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ, FILE_SHARE_WRITE, FileBasicInfo, FileDispositionInfoEx,
                FileIdInfo, GetFileInformationByHandleEx, OPEN_EXISTING, SYNCHRONIZE,
                SetFileInformationByHandle,
            },
        };

        use super::{CheckpointInputIoError, ScopedIoBudget, Snapshot, io_error, refused};
        use crate::utils::beneath::{EntryIdentity, EntryIdentityKey, EntryKind};

        struct Ancestor {
            directory: File,
            identity: EntryIdentity,
        }

        pub(super) struct RootAnchor {
            pub(super) directory: File,
            pub(super) identity: EntryIdentity,
            ancestors: Vec<Ancestor>,
        }

        fn handle(file: &File) -> HANDLE {
            file.as_raw_handle() as HANDLE
        }

        fn open_path(path: &Path, access: u32) -> Result<File, CheckpointInputIoError> {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if wide.len() > 32_768 || wide[..wide.len() - 1].contains(&0) {
                return Err(refused("an invalid or oversized Windows handle path"));
            }
            // SAFETY: wide is a live, NUL-terminated buffer. No inherited handle or
            // security descriptor is supplied. OPEN_REPARSE_POINT opens the link
            // itself. Omitting SHARE_DELETE pins every live directory ancestor.
            let raw = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                    ptr::null_mut(),
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                return Err(io_error(
                    "opening a no-reparse Windows handle",
                    io::Error::last_os_error(),
                ));
            }
            // SAFETY: CreateFileW returned a newly owned valid handle; File takes
            // responsibility for closing it exactly once.
            Ok(unsafe { File::from_raw_handle(raw.cast()) })
        }

        // These repr(C) layouts match the Windows SDK OBJECT_ATTRIBUTES and
        // IO_STATUS_BLOCK. Keeping the necessary binding local avoids adding a
        // dependency or enabling WDK features for the rest of the crate.
        #[repr(C)]
        struct NtObjectAttributes {
            length: u32,
            root_directory: HANDLE,
            object_name: *const UNICODE_STRING,
            attributes: u32,
            security_descriptor: *const c_void,
            security_quality_of_service: *const c_void,
        }

        #[repr(C)]
        union NtStatusOrPointer {
            status: i32,
            pointer: *mut c_void,
        }

        #[repr(C)]
        struct NtIoStatusBlock {
            status: NtStatusOrPointer,
            information: usize,
        }

        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn NtCreateFile(
                file_handle: *mut HANDLE,
                desired_access: u32,
                object_attributes: *const NtObjectAttributes,
                io_status: *mut NtIoStatusBlock,
                allocation_size: *const i64,
                file_attributes: u32,
                share_access: u32,
                create_disposition: u32,
                create_options: u32,
                ea_buffer: *const c_void,
                ea_length: u32,
            ) -> i32;
        }

        fn open_child(
            parent: &File,
            name: &OsStr,
            access: u32,
            refuse_reparse: bool,
        ) -> Result<File, CheckpointInputIoError> {
            open_child_with_sharing(
                parent,
                name,
                access,
                refuse_reparse,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
            )
        }

        fn open_child_with_sharing(
            parent: &File,
            name: &OsStr,
            access: u32,
            refuse_reparse: bool,
            share_access: u32,
        ) -> Result<File, CheckpointInputIoError> {
            let path = Path::new(name);
            let mut components = path.components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
            {
                return Err(refused(
                    "a child name containing a path separator or prefix",
                ));
            }
            let wide: Vec<u16> = name.encode_wide().collect();
            if wide.is_empty()
                || wide.contains(&0)
                || wide.contains(&(b'\\' as u16))
                || wide.contains(&(b'/' as u16))
                || wide.contains(&(b':' as u16))
            {
                return Err(refused("an unsafe Windows child component"));
            }
            let bytes = wide
                .len()
                .checked_mul(2)
                .and_then(|n| u16::try_from(n).ok())
                .ok_or_else(|| refused("an oversized Windows child component"))?;
            let name = UNICODE_STRING {
                Length: bytes,
                MaximumLength: bytes,
                Buffer: wide.as_ptr().cast_mut(),
            };
            let attributes = NtObjectAttributes {
                length: u32::try_from(std::mem::size_of::<NtObjectAttributes>())
                    .map_err(|_| refused("an oversized native Windows attributes buffer"))?,
                root_directory: handle(parent),
                object_name: &name,
                attributes: OBJ_CASE_INSENSITIVE
                    | if refuse_reparse { OBJ_DONT_REPARSE } else { 0 },
                security_descriptor: ptr::null(),
                security_quality_of_service: ptr::null(),
            };
            let mut io_status = NtIoStatusBlock {
                status: NtStatusOrPointer { status: 0 },
                information: 0,
            };
            let mut raw: HANDLE = ptr::null_mut();
            const FILE_OPEN: u32 = 1;
            const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
            const FILE_OPEN_REPARSE_POINT: u32 = 0x20_0000;
            // SAFETY: every pointer refers to a live, correctly aligned C layout
            // with checked byte lengths. RootDirectory is a retained directory
            // handle; the name is one component, so no ancestor path is resolved.
            // FILE_OPEN does not create. IRP_MJ_CREATE is synchronous: the I/O
            // manager waits inside create/open even when a driver pends an oplock
            // request (Microsoft "Breaking Oplocks"). NONALERT plus SYNCHRONIZE
            // preserves that synchronous contract; there is no invented pending
            // FileHandle completion protocol. OPEN_REPARSE_POINT opens a link itself,
            // while ordinary/metadata callers additionally refuse reparse. Kernel
            // waits remain cooperative: the caller owns and joins this blocking work.
            let status = unsafe {
                NtCreateFile(
                    &mut raw,
                    access | SYNCHRONIZE,
                    &attributes,
                    &mut io_status,
                    ptr::null(),
                    0,
                    share_access,
                    FILE_OPEN,
                    FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                    ptr::null(),
                    0,
                )
            };
            if status < 0 {
                // SAFETY: translating an NTSTATUS has no pointer or ownership
                // preconditions; it provides the actionable Win32 error code.
                let code = unsafe { RtlNtStatusToDosError(status) };
                let code = i32::try_from(code)
                    .map_err(|_| refused("an unrepresentable native Windows error"))?;
                return Err(io_error(
                    "opening a child relative to its pinned Windows parent",
                    io::Error::from_raw_os_error(code),
                ));
            }
            if raw.is_null() || raw == INVALID_HANDLE_VALUE {
                return Err(refused(
                    "a native Windows open without a valid completed handle",
                ));
            }
            // SAFETY: this synchronous create/open completed and returned a newly
            // owned valid handle. File closes it on every following error path.
            let file = unsafe { File::from_raw_handle(raw.cast()) };
            // This chosen call requests neither async creation nor alternate oplock
            // completion. Accept only STATUS_SUCCESS, never a generic NT_SUCCESS
            // classification that could silently admit an unexpected status.
            if status != 0 {
                return Err(refused(
                    "an unexpected native Windows open completion status",
                ));
            }
            Ok(file)
        }

        pub(super) fn snapshot_handle(file: &File) -> Result<Snapshot, CheckpointInputIoError> {
            let mut id = FILE_ID_INFO {
                VolumeSerialNumber: 0,
                FileId: FILE_ID_128 {
                    Identifier: [0; 16],
                },
            };
            let mut basic: FILE_BASIC_INFO = Default::default();
            let id_size = u32::try_from(std::mem::size_of::<FILE_ID_INFO>())
                .map_err(|_| refused("an oversized Windows file identity buffer"))?;
            let basic_size = u32::try_from(std::mem::size_of::<FILE_BASIC_INFO>())
                .map_err(|_| refused("an oversized Windows file information buffer"))?;
            // SAFETY: both structures are initialized, aligned and uniquely owned;
            // each call writes its exact structure size through a live handle.
            if unsafe {
                GetFileInformationByHandleEx(
                    handle(file),
                    FileIdInfo,
                    (&mut id as *mut FILE_ID_INFO).cast(),
                    id_size,
                )
            } == 0
            {
                return Err(io_error(
                    "reading a Windows file identity",
                    io::Error::last_os_error(),
                ));
            }
            if unsafe {
                GetFileInformationByHandleEx(
                    handle(file),
                    FileBasicInfo,
                    (&mut basic as *mut FILE_BASIC_INFO).cast(),
                    basic_size,
                )
            } == 0
            {
                return Err(io_error(
                    "reading Windows file attributes",
                    io::Error::last_os_error(),
                ));
            }
            let metadata = file
                .metadata()
                .map_err(|e| io_error("reading pinned Windows file metadata", e))?;
            let kind = if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                EntryKind::Symlink
            } else if basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                EntryKind::Directory
            } else if metadata.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            Ok(Snapshot {
                identity: EntryIdentity {
                    key: EntryIdentityKey {
                        volume: id.VolumeSerialNumber,
                        file_id: id.FileId.Identifier,
                    },
                    kind,
                },
                len: metadata.file_size(),
                mode: basic.FileAttributes,
                // Reading bytes may update access time. Creation, last-write and
                // change time detect content/attribute changes without treating a
                // successful read itself as a concurrent mutation.
                change_stamp: (basic.CreationTime, basic.LastWriteTime, basic.ChangeTime, 0),
            })
        }

        pub(super) fn snapshot_child(
            parent: &File,
            name: &OsStr,
        ) -> Result<Option<Snapshot>, CheckpointInputIoError> {
            match open_child(parent, name, FILE_READ_ATTRIBUTES, false) {
                Ok(file) => snapshot_handle(&file).map(Some),
                Err(CheckpointInputIoError::Io { source, .. })
                    if source.kind() == io::ErrorKind::NotFound =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        }

        pub(super) fn open_directory(
            parent: &File,
            name: &OsStr,
        ) -> Result<File, CheckpointInputIoError> {
            let file = open_child(
                parent,
                name,
                FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
                true,
            )?;
            if snapshot_handle(&file)?.identity.kind != EntryKind::Directory {
                return Err(refused(
                    "a Windows directory reparse point or non-directory",
                ));
            }
            Ok(file)
        }

        pub(super) fn open_regular(
            parent: &File,
            name: &OsStr,
        ) -> Result<File, CheckpointInputIoError> {
            // Hold data-writer and delete exclusion through the complete read
            // and leaf/root rechecks. Timestamps alone cannot identify a single
            // Windows content generation while a data writer remains open.
            let file =
                open_child_with_sharing(parent, name, FILE_GENERIC_READ, true, FILE_SHARE_READ)?;
            if snapshot_handle(&file)?.identity.kind != EntryKind::File {
                return Err(refused(
                    "a Windows metadata reparse point or non-regular file",
                ));
            }
            Ok(file)
        }

        pub(super) fn open_anchor(
            path: &Path,
            run_id: &str,
            budget: &ScopedIoBudget,
        ) -> Result<RootAnchor, CheckpointInputIoError> {
            budget.check()?;
            let mut components = path.components();
            let prefix = match components.next() {
                Some(Component::Prefix(prefix)) => prefix,
                _ => {
                    return Err(refused(
                        "a Windows runs root without an absolute volume prefix",
                    ));
                }
            };
            if !matches!(components.next(), Some(Component::RootDir)) {
                return Err(refused("a drive-relative Windows runs root"));
            }
            let mut volume = PathBuf::from(prefix.as_os_str());
            volume.push("\\");
            let mut directory = open_path(&volume, FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY)?;
            if snapshot_handle(&directory)?.identity.kind != EntryKind::Directory {
                return Err(refused("a Windows root volume reparse point"));
            }
            let mut ancestors = Vec::new();
            for component in components {
                budget.check()?;
                let Component::Normal(name) = component else {
                    return Err(refused("an unsafe Windows runs-root component"));
                };
                let next = open_directory(&directory, name)?;
                ancestors.push(Ancestor {
                    identity: snapshot_handle(&directory)?.identity,
                    directory,
                });
                directory = next;
            }
            budget.check()?;
            let run = open_directory(&directory, OsStr::new(run_id))?;
            ancestors.push(Ancestor {
                identity: snapshot_handle(&directory)?.identity,
                directory,
            });
            let identity = snapshot_handle(&run)?.identity;
            let anchor = RootAnchor {
                directory: run,
                identity,
                ancestors,
            };
            verify_anchor(&anchor, budget)?;
            Ok(anchor)
        }

        pub(super) fn verify_anchor(
            anchor: &RootAnchor,
            budget: &ScopedIoBudget,
        ) -> Result<(), CheckpointInputIoError> {
            budget.check()?;
            for ancestor in &anchor.ancestors {
                budget.check()?;
                if snapshot_handle(&ancestor.directory)?.identity != ancestor.identity {
                    return Err(CheckpointInputIoError::Changed {
                        action: "verifying a pinned Windows ancestor",
                    });
                }
            }
            if snapshot_handle(&anchor.directory)?.identity != anchor.identity {
                return Err(CheckpointInputIoError::Changed {
                    action: "verifying a pinned Windows run root",
                });
            }
            budget.check()
        }

        pub(super) fn make_directory_writable(
            directory: &File,
            expected: &Snapshot,
        ) -> Result<(), CheckpointInputIoError> {
            if snapshot_handle(directory)?.identity != expected.identity
                || expected.identity.kind != EntryKind::Directory
            {
                return Err(CheckpointInputIoError::Changed {
                    action: "checking a Windows input directory",
                });
            }
            // No attribute or ACL changes. FileDispositionInfoEx below removes a
            // proven readonly leaf without modifying regular payload permissions.
            Ok(())
        }

        pub(super) fn remove_child(
            parent: &File,
            name: &OsStr,
            expected: &Snapshot,
            budget: &ScopedIoBudget,
        ) -> Result<(), CheckpointInputIoError> {
            budget.check()?;
            let file = open_child(parent, name, FILE_READ_ATTRIBUTES | DELETE, false)?;
            let actual = snapshot_handle(&file)?;
            if actual.identity != expected.identity
                || (actual.identity.kind != EntryKind::Directory && actual != *expected)
            {
                return Err(CheckpointInputIoError::Changed {
                    action: "deleting a pinned Windows input entry",
                });
            }
            budget.check()?;
            let info = FILE_DISPOSITION_INFO_EX {
                Flags: FILE_DISPOSITION_FLAG_DELETE
                    | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
                    | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
            };
            let size = u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO_EX>())
                .map_err(|_| refused("an oversized Windows deletion buffer"))?;
            // SAFETY: file owns a DELETE-capable no-reparse handle with matching
            // identity; info has the exact initialized structure size. An internal
            // reparse leaf is deleted itself, never opened through to its target.
            if unsafe {
                SetFileInformationByHandle(
                    handle(&file),
                    FileDispositionInfoEx,
                    (&info as *const FILE_DISPOSITION_INFO_EX).cast(),
                    size,
                )
            } == 0
            {
                return Err(io_error(
                    "deleting a confined Windows input handle",
                    io::Error::last_os_error(),
                ));
            }
            // POSIX deletion becomes visible when the deleting handle closes.
            drop(file);
            budget.check()?;
            if snapshot_child(parent, name)?.is_some() {
                return Err(CheckpointInputIoError::Changed {
                    action: "confirming Windows input deletion",
                });
            }
            Ok(())
        }
    }

    #[cfg(unix)]
    mod scoped_fs {
        use std::{
            ffi::{CString, OsStr, OsString},
            fs::File,
            os::{
                fd::{AsRawFd, FromRawFd},
                unix::{ffi::OsStrExt, fs::MetadataExt},
            },
            path::{Component, Path},
        };

        use super::{
            CheckpointInputIoError, EntryIdentity, EntryIdentityKey, EntryKind, ScopedIoBudget,
            Snapshot, io_error, refused,
        };

        struct AnchorStep {
            parent: File,
            parent_identity: EntryIdentity,
            name: OsString,
            identity: EntryIdentity,
        }

        pub(super) struct RootAnchor {
            pub(super) directory: File,
            pub(super) identity: EntryIdentity,
            ancestry: Vec<AnchorStep>,
        }

        fn name_cstring(name: &OsStr) -> Result<CString, CheckpointInputIoError> {
            if name.is_empty() || name.as_bytes().contains(&b'/') || name == "." || name == ".." {
                return Err(refused("a non-normal descriptor-relative component"));
            }
            CString::new(name.as_bytes()).map_err(|_| refused("a component containing NUL"))
        }

        fn open_at(
            parent: &File,
            name: &OsStr,
            flags: i32,
        ) -> Result<File, CheckpointInputIoError> {
            let name = name_cstring(name)?;
            // SAFETY: the borrowed parent descriptor and NUL-terminated component
            // stay live; no creation flag is used, and the returned fd is owned once.
            let descriptor = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if descriptor < 0 {
                return Err(io_error(
                    "opening a no-follow child handle",
                    std::io::Error::last_os_error(),
                ));
            }
            // SAFETY: openat returned a new owned descriptor, checked non-negative.
            Ok(unsafe { File::from_raw_fd(descriptor) })
        }

        pub(super) fn open_directory(
            parent: &File,
            name: &OsStr,
        ) -> Result<File, CheckpointInputIoError> {
            let file = open_at(
                parent,
                name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NONBLOCK,
            )?;
            if snapshot_handle(&file)?.identity.kind != EntryKind::Directory {
                return Err(refused(
                    "a no-follow directory handle with a non-directory kind",
                ));
            }
            Ok(file)
        }

        pub(super) fn open_regular(
            parent: &File,
            name: &OsStr,
        ) -> Result<File, CheckpointInputIoError> {
            let file = open_at(parent, name, libc::O_RDONLY | libc::O_NONBLOCK)?;
            if snapshot_handle(&file)?.identity.kind != EntryKind::File {
                return Err(refused("a no-follow file handle with a non-regular kind"));
            }
            Ok(file)
        }

        fn identity(device: u64, inode: u64, kind: EntryKind) -> EntryIdentity {
            EntryIdentity {
                key: EntryIdentityKey {
                    volume: device,
                    file_id: u128::from(inode).to_le_bytes(),
                },
                kind,
            }
        }

        fn identifier(value: impl TryInto<u64>) -> Result<u64, CheckpointInputIoError> {
            value
                .try_into()
                .map_err(|_| refused("a filesystem identity outside its supported range"))
        }

        fn stat_nanoseconds(stat: &libc::stat) -> (i64, i64) {
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
            {
                (stat.st_ctime_nsec, stat.st_mtime_nsec)
            }
            #[cfg(any(
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            ))]
            {
                (stat.st_ctim.tv_nsec, stat.st_mtim.tv_nsec)
            }
            #[cfg(not(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            )))]
            {
                let _ = stat;
                (0, 0)
            }
        }

        pub(super) fn snapshot_handle(file: &File) -> Result<Snapshot, CheckpointInputIoError> {
            let metadata = file
                .metadata()
                .map_err(|e| io_error("identifying a held filesystem handle", e))?;
            let file_type = metadata.file_type();
            let kind = if file_type.is_symlink() {
                EntryKind::Symlink
            } else if file_type.is_dir() {
                EntryKind::Directory
            } else if file_type.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            Ok(Snapshot {
                identity: identity(metadata.dev(), metadata.ino(), kind),
                len: metadata.len(),
                mode: metadata.mode(),
                change_stamp: (
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                ),
            })
        }

        pub(super) fn snapshot_child(
            parent: &File,
            name: &OsStr,
        ) -> Result<Option<Snapshot>, CheckpointInputIoError> {
            let name = name_cstring(name)?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: parent and component stay live, stat is an appropriately sized
            // output buffer; its contents are only read after a successful fstatat.
            let result = unsafe {
                libc::fstatat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(io_error("identifying a no-follow child", error));
            }
            // SAFETY: successful fstatat initialized the stat output buffer.
            let stat = unsafe { stat.assume_init() };
            let kind = match stat.st_mode & libc::S_IFMT {
                libc::S_IFDIR => EntryKind::Directory,
                libc::S_IFREG => EntryKind::File,
                libc::S_IFLNK => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
            let volume = identifier(stat.st_dev)?;
            let inode = identifier(stat.st_ino)?;
            let len = stat
                .st_size
                .try_into()
                .map_err(|_| refused("a negative filesystem entry length"))?;
            let (ctime_nsec, mtime_nsec) = stat_nanoseconds(&stat);
            Ok(Some(Snapshot {
                identity: identity(volume, inode, kind),
                len,
                mode: {
                    #[cfg(target_os = "linux")]
                    {
                        stat.st_mode
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        u32::from(stat.st_mode)
                    }
                },
                change_stamp: (stat.st_ctime, ctime_nsec, stat.st_mtime, mtime_nsec),
            }))
        }

        pub(super) fn open_anchor(
            path: &Path,
            run_id: &str,
            budget: &ScopedIoBudget,
        ) -> Result<RootAnchor, CheckpointInputIoError> {
            budget.check()?;
            let slash = c"/";
            // SAFETY: literal is NUL-terminated; the checked fd is owned below.
            let descriptor = unsafe {
                libc::open(
                    slash.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if descriptor < 0 {
                return Err(io_error(
                    "opening the filesystem root anchor",
                    std::io::Error::last_os_error(),
                ));
            }
            // SAFETY: open returned a new owned descriptor, checked non-negative.
            let mut current = unsafe { File::from_raw_fd(descriptor) };
            let mut ancestry = Vec::new();
            let components = path.components().chain(Path::new(run_id).components());
            for component in components {
                budget.check()?;
                let name = match component {
                    Component::RootDir => continue,
                    Component::Normal(name) => name,
                    _ => return Err(refused("a trusted root path with non-normal components")),
                };
                let parent_identity = snapshot_handle(&current)?.identity;
                let before = snapshot_child(&current, name)?
                    .ok_or_else(|| refused("a missing trusted root or run directory"))?;
                if before.identity.kind != EntryKind::Directory {
                    return Err(refused(
                        "a trusted root ancestor or run that is not a no-follow directory",
                    ));
                }
                let child = open_directory(&current, name)?;
                if snapshot_handle(&child)?.identity != before.identity {
                    return Err(CheckpointInputIoError::Changed {
                        action: "pinning trusted root ancestors",
                    });
                }
                ancestry.push(AnchorStep {
                    parent: current,
                    parent_identity,
                    name: name.to_os_string(),
                    identity: before.identity,
                });
                current = child;
            }
            let identity = snapshot_handle(&current)?.identity;
            let anchor = RootAnchor {
                directory: current,
                identity,
                ancestry,
            };
            verify_anchor(&anchor, budget)?;
            Ok(anchor)
        }

        pub(super) fn verify_anchor(
            anchor: &RootAnchor,
            budget: &ScopedIoBudget,
        ) -> Result<(), CheckpointInputIoError> {
            for step in &anchor.ancestry {
                budget.check()?;
                if snapshot_handle(&step.parent)?.identity != step.parent_identity
                    || snapshot_child(&step.parent, &step.name)?.map(|s| s.identity)
                        != Some(step.identity)
                {
                    return Err(CheckpointInputIoError::Changed {
                        action: "rechecking trusted root ancestry",
                    });
                }
            }
            if snapshot_handle(&anchor.directory)?.identity != anchor.identity {
                return Err(CheckpointInputIoError::Changed {
                    action: "rechecking the held run root",
                });
            }
            budget.check()?;
            verify_anchor_bindings(anchor)
        }

        pub(super) fn verify_anchor_bindings(
            anchor: &RootAnchor,
        ) -> Result<(), CheckpointInputIoError> {
            for step in &anchor.ancestry {
                if snapshot_handle(&step.parent)?.identity != step.parent_identity
                    || snapshot_child(&step.parent, &step.name)?.map(|s| s.identity)
                        != Some(step.identity)
                {
                    return Err(CheckpointInputIoError::Changed {
                        action: "rechecking trusted root bindings after a cooperative callback",
                    });
                }
            }
            if snapshot_handle(&anchor.directory)?.identity != anchor.identity {
                return Err(CheckpointInputIoError::Changed {
                    action: "rechecking the held run root after a cooperative callback",
                });
            }
            Ok(())
        }

        pub(super) fn make_directory_writable(
            directory: &File,
            expected: &Snapshot,
            budget: &ScopedIoBudget,
            validate_bindings: &dyn Fn() -> Result<(), CheckpointInputIoError>,
        ) -> Result<(), CheckpointInputIoError> {
            budget.check()?;
            validate_bindings()?;
            let before = snapshot_handle(directory)?;
            if before.identity != expected.identity || before.identity.kind != EntryKind::Directory
            {
                return Err(CheckpointInputIoError::Changed {
                    action: "making an input directory writable",
                });
            }
            // Only already-proven input directory descriptors receive owner write
            // and search. Regular payload handles and outside paths are never chmod'd.
            let mode = (before.mode & 0o7777) | 0o700;
            let mode = {
                #[cfg(target_os = "linux")]
                {
                    mode
                }
                #[cfg(not(target_os = "linux"))]
                {
                    libc::mode_t::try_from(mode)
                        .map_err(|_| refused("an unsupported directory permission mode"))?
                }
            };
            validate_bindings()?;
            // SAFETY: the held descriptor stays live and its complete named
            // ancestry has just been revalidated, after every budget callback.
            // A concurrent external rename can still race this syscall; this
            // is not a kernel-enforced compare-ancestry-and-chmod operation.
            if unsafe { libc::fchmod(directory.as_raw_fd(), mode) } < 0 {
                return Err(io_error(
                    "making a confined input directory writable",
                    std::io::Error::last_os_error(),
                ));
            }
            if snapshot_handle(directory)?.identity != expected.identity {
                return Err(CheckpointInputIoError::Changed {
                    action: "rechecking a writable input directory",
                });
            }
            validate_bindings()?;
            Ok(())
        }

        pub(super) fn remove_child(
            parent: &File,
            name: &OsStr,
            expected: &Snapshot,
            budget: &ScopedIoBudget,
            validate_bindings: &dyn Fn() -> Result<(), CheckpointInputIoError>,
        ) -> Result<(), CheckpointInputIoError> {
            budget.check()?;
            validate_bindings()?;
            let parent_identity = snapshot_handle(parent)?.identity;
            if parent_identity.kind != EntryKind::Directory {
                return Err(refused("a deletion parent that is not a directory"));
            }
            if snapshot_child(parent, name)?.map(|s| s.identity) != Some(expected.identity) {
                return Err(CheckpointInputIoError::Changed {
                    action: "checking a deletion leaf",
                });
            }
            // A real regular/directory handle pins its physical object until after
            // deletion. Internal symlinks are never opened and are unlinked itself.
            let held = match expected.identity.kind {
                EntryKind::File => Some(open_regular(parent, name)?),
                EntryKind::Directory => Some(open_directory(parent, name)?),
                EntryKind::Symlink => None,
                EntryKind::Other => {
                    return Err(refused("a deletion leaf with an unsupported kind"));
                }
            };
            if let Some(held) = &held
                && snapshot_handle(held)?.identity != expected.identity
            {
                return Err(CheckpointInputIoError::Changed {
                    action: "pinning a deletion leaf",
                });
            }
            budget.check()?;
            validate_bindings()?;
            if snapshot_handle(parent)?.identity != parent_identity
                || snapshot_child(parent, name)?.map(|s| s.identity) != Some(expected.identity)
            {
                return Err(CheckpointInputIoError::Changed {
                    action: "rechecking a deletion leaf",
                });
            }
            let name_c = name_cstring(name)?;
            let flags = if expected.identity.kind == EntryKind::Directory {
                libc::AT_REMOVEDIR
            } else {
                0
            };
            validate_bindings()?;
            // SAFETY: the held parent/name ancestry and leaf identity have just
            // been revalidated after every cooperative callback. unlinkat never
            // follows a link leaf and is not recursive. Unix still permits an
            // external rename in the last check/syscall gap; this is not CAS.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name_c.as_ptr(), flags) } < 0 {
                return Err(io_error(
                    "removing a confined input entry",
                    std::io::Error::last_os_error(),
                ));
            }
            if snapshot_handle(parent)?.identity != parent_identity
                || snapshot_child(parent, name)?.is_some()
            {
                return Err(CheckpointInputIoError::Changed {
                    action: "confirming a deletion leaf",
                });
            }
            budget.check()
        }
    }

    #[cfg(not(any(unix, windows)))]
    compile_error!("checkpoint input scoped handles require Unix or Windows");

    #[cfg(test)]
    pub(crate) mod test_support {
        use std::{
            fs,
            io::Write,
            path::{Path, PathBuf},
            sync::{
                Arc,
                atomic::{AtomicBool, AtomicUsize, Ordering},
            },
            thread,
            time::{Duration, Instant},
        };

        use super::*;

        struct Fixture {
            _temp: tempfile::TempDir,
            runs: PathBuf,
            run: PathBuf,
            input: PathBuf,
        }

        impl Fixture {
            fn new() -> Self {
                let temp = tempfile::tempdir().unwrap();
                // A test-owned physical root, never a production symlink fallback.
                let physical = fs::canonicalize(temp.path()).unwrap();
                let runs = physical.join("runs");
                let run = runs.join("run-1");
                let input = run.join(super::super::CHECKPOINT_INPUT_DIR);
                fs::create_dir_all(&input).unwrap();
                Self {
                    _temp: temp,
                    runs,
                    run,
                    input,
                }
            }

            fn root(&self, budget: &ScopedIoBudget) -> RunRootHandle {
                open_scoped_run_root(&self.runs, "run-1", budget).unwrap()
            }

            fn create(&self, relative: &Path, directory: bool) {
                #[cfg(unix)]
                {
                    use std::{
                        ffi::CString,
                        os::{
                            fd::{AsRawFd, FromRawFd},
                            unix::ffi::OsStrExt,
                        },
                    };
                    let budget = generous_budget();
                    let root = self.root(&budget);
                    let mut parent = scoped_fs::open_directory(
                        &root.anchor.directory,
                        OsStr::new(super::super::CHECKPOINT_INPUT_DIR),
                    )
                    .unwrap();
                    let components = relative.components().collect::<Vec<_>>();
                    for (index, component) in components.iter().enumerate() {
                        let Component::Normal(name) = component else {
                            panic!("fixture must be relative and normal");
                        };
                        let name_c = CString::new(name.as_bytes()).unwrap();
                        if index + 1 < components.len() || directory {
                            // SAFETY: owned fixture parent fd and valid component;
                            // only this isolated fixture is created or opened here.
                            let result = unsafe {
                                libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700)
                            };
                            assert!(
                                result == 0
                                    || std::io::Error::last_os_error().kind()
                                        == std::io::ErrorKind::AlreadyExists
                            );
                            parent = scoped_fs::open_directory(&parent, name).unwrap();
                        } else {
                            // SAFETY: the fixture parent and component are live;
                            // create_new and no-follow prevent replacing any entry.
                            let descriptor = unsafe {
                                libc::openat(
                                    parent.as_raw_fd(),
                                    name_c.as_ptr(),
                                    libc::O_WRONLY
                                        | libc::O_CREAT
                                        | libc::O_EXCL
                                        | libc::O_NOFOLLOW
                                        | libc::O_CLOEXEC,
                                    0o600,
                                )
                            };
                            assert!(
                                descriptor >= 0,
                                "fixture create failed: {}",
                                std::io::Error::last_os_error()
                            );
                            // SAFETY: the newly created descriptor has one owner.
                            let mut file = unsafe { File::from_raw_fd(descriptor) };
                            file.write_all(b"fixture payload").unwrap();
                        }
                    }
                }
                #[cfg(windows)]
                {
                    let destination = self.input.join(relative);
                    if directory {
                        fs::create_dir_all(&destination).unwrap();
                    } else {
                        fs::create_dir_all(destination.parent().unwrap()).unwrap();
                        fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(destination)
                            .unwrap()
                            .write_all(b"fixture payload")
                            .unwrap();
                    }
                }
            }

            fn seal_root(&self) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&self.input, fs::Permissions::from_mode(0o555)).unwrap();
                }
                #[cfg(windows)]
                {
                    let mut permissions = fs::metadata(&self.input).unwrap().permissions();
                    permissions.set_readonly(true);
                    fs::set_permissions(&self.input, permissions).unwrap();
                }
            }

            fn root_attributes(&self) -> (bool, u32) {
                let metadata = fs::symlink_metadata(&self.input).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    (
                        metadata.permissions().readonly(),
                        metadata.permissions().mode(),
                    )
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    (
                        metadata.permissions().readonly(),
                        metadata.file_attributes(),
                    )
                }
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fn restore(directory: &File, depth: usize) {
                        assert!(
                            depth <= 128,
                            "only bounded test-owned trees may be restored"
                        );
                        directory
                            .set_permissions(fs::Permissions::from_mode(0o700))
                            .unwrap();
                        let stream =
                            crate::utils::beneath::read_dir_fd(directory.try_clone().unwrap())
                                .unwrap();
                        for entry in stream {
                            let entry = entry.unwrap();
                            let Some(snapshot) =
                                scoped_fs::snapshot_child(directory, &entry.name).unwrap()
                            else {
                                continue;
                            };
                            if snapshot.identity.kind == EntryKind::Directory {
                                let child =
                                    scoped_fs::open_directory(directory, &entry.name).unwrap();
                                restore(&child, depth + 1);
                            }
                        }
                    }
                    // Test teardown after assertions only; no link is followed and
                    // long-path trees are walked by fd, outside production helpers.
                    let directory = File::open(self._temp.path()).unwrap();
                    restore(&directory, 0);
                }
                #[cfg(windows)]
                {
                    fn restore(path: &Path, depth: usize) {
                        assert!(
                            depth <= 128,
                            "only bounded test-owned trees may be restored"
                        );
                        let metadata = fs::symlink_metadata(path).unwrap();
                        if metadata.file_type().is_symlink() {
                            return;
                        }
                        let mut permissions = metadata.permissions();
                        permissions.set_readonly(false);
                        fs::set_permissions(path, permissions).unwrap();
                        if metadata.is_dir() {
                            for entry in fs::read_dir(path).unwrap() {
                                restore(&entry.unwrap().path(), depth + 1);
                            }
                        }
                    }
                    // Attributes are restored solely for isolated fixture teardown,
                    // after every sentinel and zero-mutation assertion has run.
                    restore(self._temp.path(), 0);
                }
            }
        }

        fn generous_budget() -> ScopedIoBudget {
            ScopedIoBudget {
                deadline: Instant::now() + Duration::from_secs(180),
                cancelled: Arc::new(|| false),
            }
        }

        fn seal(path: &Path, directory: bool) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    path,
                    fs::Permissions::from_mode(if directory { 0o555 } else { 0o444 }),
                )
                .unwrap();
            }
            #[cfg(windows)]
            {
                let mut permissions = fs::metadata(path).unwrap().permissions();
                permissions.set_readonly(true);
                fs::set_permissions(path, permissions).unwrap();
                let _ = directory;
            }
        }

        fn attributes(path: &Path) -> (bool, u32) {
            let metadata = fs::symlink_metadata(path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                (
                    metadata.permissions().readonly(),
                    metadata.permissions().mode(),
                )
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                (
                    metadata.permissions().readonly(),
                    metadata.file_attributes(),
                )
            }
        }

        fn file_link(target: &Path, link: &Path) {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, link).unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(target, link).unwrap();
        }

        fn directory_link(target: &Path, link: &Path) {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, link).unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(target, link).unwrap();
        }

        struct DeniedMetadataAccess {
            #[cfg(unix)]
            path: PathBuf,
            #[cfg(windows)]
            _handle: File,
        }

        impl DeniedMetadataAccess {
            fn new(path: &Path) -> Self {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
                    Self {
                        path: path.to_path_buf(),
                    }
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    Self {
                        _handle: fs::OpenOptions::new()
                            .read(true)
                            .share_mode(0)
                            .open(path)
                            .unwrap(),
                    }
                }
            }
        }

        #[cfg(unix)]
        impl Drop for DeniedMetadataAccess {
            fn drop(&mut self) {
                use std::os::unix::fs::PermissionsExt;
                // Release test-only access denial after the actual failed operation.
                fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }

        thread_local! {
            static MUTATION_PHASE: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
            static REPARENT_BOUNDARY: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
        }

        pub(super) fn set_mutation_phase(phase: &'static str) {
            MUTATION_PHASE.with(|current| current.set(Some(phase)));
        }

        pub(super) fn mark_reparent_callback_boundary(relative: &Path) {
            if relative == Path::new("sub") {
                REPARENT_BOUNDARY.with(|boundary| {
                    boundary.set(MUTATION_PHASE.with(std::cell::Cell::get));
                });
            }
        }

        fn take_reparent_boundary() -> Option<&'static str> {
            REPARENT_BOUNDARY.with(|boundary| boundary.replace(None))
        }

        struct ReparentSeamReset;

        impl Drop for ReparentSeamReset {
            fn drop(&mut self) {
                MUTATION_PHASE.with(|phase| phase.set(None));
                REPARENT_BOUNDARY.with(|boundary| boundary.set(None));
            }
        }

        fn reparent_refuses_outside_mutation(phase: &'static str) {
            let _reset = ReparentSeamReset;
            MUTATION_PHASE.with(|current| current.set(None));
            REPARENT_BOUNDARY.with(|boundary| boundary.set(None));
            let fixture = Fixture::new();
            fixture.create(Path::new("sub/payload"), false);
            seal(&fixture.input.join("sub/payload"), false);
            seal(&fixture.input.join("sub"), true);
            fixture.seal_root();
            let outside = fixture
                .runs
                .parent()
                .unwrap()
                .join(format!("outside-{phase}"));
            fs::create_dir(&outside).unwrap();
            let sentinel = outside.join("sentinel");
            fs::write(&sentinel, b"outside never changes").unwrap();
            seal(&sentinel, false);
            let outside_before = attributes(&outside);
            let sentinel_before = attributes(&sentinel);
            let original_payload = fs::read(fixture.input.join("sub/payload")).unwrap();
            let root = fixture.root(&generous_budget());
            let attempted = Arc::new(AtomicBool::new(false));
            let observed = Arc::new(std::sync::Mutex::new(None));
            let attempt = attempted.clone();
            let observation = observed.clone();
            let sub = fixture.input.join("sub");
            let moved = outside.join("moved-sub");
            let destination = moved.clone();
            let budget = ScopedIoBudget {
                deadline: Instant::now() + Duration::from_secs(20),
                cancelled: Arc::new(move || {
                    if take_reparent_boundary() == Some(phase)
                        && !attempt.swap(true, Ordering::SeqCst)
                    {
                        // This is the actual cancellation callback immediately
                        // after a sub directory handle has been acquired. The
                        // seam only identifies the point; rename is a real syscall.
                        let before = attributes(&sub);
                        let outcome = fs::rename(&sub, &destination);
                        *observation.lock().unwrap() = Some((before, outcome));
                    }
                    false
                }),
            };
            let result = cleanup_checkpoint_input(&root, &budget);
            assert!(
                attempted.load(Ordering::SeqCst),
                "the {phase} reparent seam must execute"
            );
            assert_eq!(attributes(&outside), outside_before);
            assert_eq!(attributes(&sentinel), sentinel_before);
            assert_eq!(fs::read(&sentinel).unwrap(), b"outside never changes");
            let (before, moved_result) = observed.lock().unwrap().take().unwrap();
            #[cfg(unix)]
            {
                moved_result.unwrap();
                // Check preservation before accepting any post-mutation error.
                // The former helper chmods this outside directory or deletes its
                // payload, so these are genuine behavioral RED assertions.
                assert_eq!(
                    attributes(&moved),
                    before,
                    "{phase} changed outside directory attributes"
                );
                assert_eq!(
                    fs::read(moved.join("payload")).unwrap(),
                    original_payload,
                    "{phase} changed outside bytes"
                );
                assert!(
                    matches!(result, Err(CheckpointInputIoError::Changed { .. })),
                    "reparent must be refused before mutation: {result:?}"
                );
                assert!(fixture.input.exists());
            }
            #[cfg(windows)]
            {
                // Actual held no-share-delete ancestors block the rename. This
                // is an executed negative assertion, not a platform skip, and
                // the legitimate supported readonly cleanup still must succeed.
                let refused_move = moved_result.unwrap_err();
                assert!(
                    matches!(refused_move.raw_os_error(), Some(5 | 32)),
                    "expected real Windows sharing/access refusal: {refused_move}"
                );
                result.unwrap();
                assert!(!fixture.input.exists() && !moved.exists());
                assert!(!original_payload.is_empty());
                assert!(
                    before.0,
                    "the moved target fixture must actually be readonly"
                );
            }
        }

        pub(crate) fn readonly_cleanup() {
            let fixture = Fixture::new();
            fixture.create(Path::new("sub/payload"), false);
            let outside = fixture.runs.parent().unwrap().join("outside");
            fs::create_dir(&outside).unwrap();
            let sentinel = outside.join("sentinel");
            fs::write(&sentinel, b"outside stays byte-identical").unwrap();
            fs::hard_link(
                fixture.input.join("sub/payload"),
                outside.join("payload-alias"),
            )
            .unwrap();
            seal(&fixture.input.join("sub/payload"), false);
            seal(&fixture.input.join("sub"), true);
            file_link(&sentinel, &fixture.input.join("outside-file-link"));
            directory_link(&outside, &fixture.input.join("outside-directory-link"));
            seal(&sentinel, false);
            seal(&outside, true);
            fixture.seal_root();
            let sentinel_before = attributes(&sentinel);
            let directory_before = attributes(&outside);
            let payload_before = attributes(&outside.join("payload-alias"));
            let other_run = fixture.runs.join("run-2");
            fs::create_dir(&other_run).unwrap();
            fs::write(other_run.join("ordinary-object"), b"retained").unwrap();
            fs::write(fixture.run.join("catalog-sentinel"), b"retained").unwrap();
            let budget = generous_budget();
            let root = fixture.root(&budget);
            cleanup_checkpoint_input(&root, &budget).unwrap();
            assert!(!fixture.input.exists());
            cleanup_checkpoint_input(&root, &budget).unwrap();
            assert_eq!(
                fs::read(&sentinel).unwrap(),
                b"outside stays byte-identical"
            );
            assert_eq!(attributes(&sentinel), sentinel_before);
            assert_eq!(attributes(&outside), directory_before);
            assert_eq!(attributes(&outside.join("payload-alias")), payload_before);
            assert_eq!(
                fs::read(outside.join("payload-alias")).unwrap(),
                b"fixture payload"
            );
            assert_eq!(
                fs::read(other_run.join("ordinary-object")).unwrap(),
                b"retained"
            );
            assert_eq!(
                fs::read(fixture.run.join("catalog-sentinel")).unwrap(),
                b"retained"
            );
            directory_link(&outside, &fixture.input);
            assert!(matches!(
                cleanup_checkpoint_input(&root, &budget),
                Err(CheckpointInputIoError::Refused { .. })
            ));
            assert_eq!(attributes(&outside), directory_before);
            assert_eq!(
                fs::read(&sentinel).unwrap(),
                b"outside stays byte-identical"
            );
            drop(root);
            let alias = fixture.runs.parent().unwrap().join("runs-alias");
            directory_link(&fixture.runs, &alias);
            assert!(open_scoped_run_root(&alias, "run-1", &budget).is_err());
            let run_link = fixture.runs.join("run-link");
            directory_link(&outside, &run_link);
            assert!(open_scoped_run_root(&fixture.runs, "run-link", &budget).is_err());
            assert!(open_scoped_run_root(&fixture.runs, "../outside", &budget).is_err());
            assert_eq!(attributes(&outside), directory_before);
            assert_eq!(
                fs::read(&sentinel).unwrap(),
                b"outside stays byte-identical"
            );
        }

        pub(crate) fn metadata_boundary() {
            let fixture = Fixture::new();
            let budget = generous_budget();
            let root = fixture.root(&budget);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;

                use windows_sys::Win32::Storage::FileSystem::{
                    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
                };
                // An existing compatible writer stays live through the entire
                // metadata call. The new read must reject its data-write access,
                // independently of timestamp publication or suppression.
                let state_path = fixture.run.join("state.json");
                fs::write(&state_path, b"stable synthetic metadata").unwrap();
                let held_writer = fs::OpenOptions::new()
                    .write(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                    .open(&state_path)
                    .unwrap();
                assert!(
                    matches!(read_run_metadata(&root, &budget), Err(CheckpointInputIoError::Io { source, .. }) if source.raw_os_error() == Some(32)),
                    "a live data writer must produce ERROR_SHARING_VIOLATION"
                );
                drop(held_writer);
                let readable = read_run_metadata(&root, &budget).unwrap();
                assert_eq!(
                    readable.state.as_deref(),
                    Some(b"stable synthetic metadata".as_slice())
                );
                fs::remove_file(state_path).unwrap();
            }
            let missing = read_run_metadata(&root, &budget).unwrap();
            assert!(missing.state.is_none() && missing.manifest.is_none());
            assert!(missing.state_identity.is_none() && missing.manifest_identity.is_none());
            assert!(
                !fixture.run.join("state.json").exists()
                    && !fixture.run.join("manifest.json").exists()
            );
            for name in ["state.json", "manifest.json"] {
                fs::File::create(fixture.run.join(name))
                    .unwrap()
                    .set_len(MAX_METADATA_BYTES)
                    .unwrap();
            }
            let exact = read_run_metadata(&root, &budget).unwrap();
            assert_eq!(
                exact.state.as_ref().unwrap().len() as u64,
                MAX_METADATA_BYTES
            );
            assert_eq!(
                exact.manifest.as_ref().unwrap().len() as u64,
                MAX_METADATA_BYTES
            );
            assert_eq!(
                exact.state.as_ref().unwrap().len() + exact.manifest.as_ref().unwrap().len(),
                MAX_TOTAL_METADATA_BYTES as usize
            );
            assert_eq!(exact.root_identity, missing.root_identity);
            assert_eq!(exact.state_identity.unwrap().len, MAX_METADATA_BYTES);
            assert_eq!(exact.manifest_identity.unwrap().len, MAX_METADATA_BYTES);
            drop(exact);
            fs::OpenOptions::new()
                .write(true)
                .open(fixture.run.join("state.json"))
                .unwrap()
                .set_len(MAX_METADATA_BYTES + 1)
                .unwrap();
            assert!(matches!(
                read_run_metadata(&root, &budget),
                Err(CheckpointInputIoError::Budget { .. })
            ));
            assert_eq!(
                fs::metadata(fixture.run.join("state.json")).unwrap().len(),
                MAX_METADATA_BYTES + 1
            );
            fs::OpenOptions::new()
                .write(true)
                .open(fixture.run.join("state.json"))
                .unwrap()
                .set_len(MAX_METADATA_BYTES)
                .unwrap();
            fs::OpenOptions::new()
                .write(true)
                .open(fixture.run.join("manifest.json"))
                .unwrap()
                .set_len(MAX_METADATA_BYTES + 1)
                .unwrap();
            // Two 64 MiB leaves already sum to 128 MiB. A combined +1 necessarily
            // violates a leaf limit too; do not invent an unreachable total-only case.
            assert!(matches!(
                read_run_metadata(&root, &budget),
                Err(CheckpointInputIoError::Budget { .. })
            ));
            fs::write(fixture.run.join("state.json"), b"first").unwrap();
            fs::write(fixture.run.join("manifest.json"), b"manifest").unwrap();
            let first = read_run_metadata(&root, &budget).unwrap();
            thread::sleep(Duration::from_millis(2));
            fs::write(fixture.run.join("state.json"), b"other").unwrap();
            let second = read_run_metadata(&root, &budget).unwrap();
            assert_eq!(
                first.state_identity.unwrap().identity,
                second.state_identity.unwrap().identity
            );
            assert_ne!(first.state_identity, second.state_identity);
            assert_eq!(second.state.as_deref(), Some(b"other".as_slice()));
            // Inject a same-inode content change after the first actual read chunk.
            // Calibrate only setup checks on this same pinned root; no guessed timer.
            fs::write(fixture.run.join("state.json"), vec![1_u8; 64 * 1024]).unwrap();
            fs::remove_file(fixture.run.join("manifest.json")).unwrap();
            let setup_checks = Arc::new(AtomicUsize::new(0));
            let setup_counter = setup_checks.clone();
            let setup_budget = ScopedIoBudget {
                deadline: budget.deadline,
                cancelled: Arc::new(move || {
                    setup_counter.fetch_add(1, Ordering::SeqCst);
                    false
                }),
            };
            scoped_fs::verify_anchor(&root.anchor, &setup_budget).unwrap();
            drop(open_metadata(&root, "state.json", &setup_budget).unwrap());
            drop(open_metadata(&root, "manifest.json", &setup_budget).unwrap());
            let trigger = setup_checks.load(Ordering::SeqCst) + 1;
            let read_checks = Arc::new(AtomicUsize::new(0));
            let read_counter = read_checks.clone();
            let attempted = Arc::new(AtomicBool::new(false));
            let changed = Arc::new(AtomicBool::new(false));
            let attempted_write = attempted.clone();
            let successful_write = changed.clone();
            let failed_write = Arc::new(std::sync::Mutex::new(None));
            let failed_writer = failed_write.clone();
            let state_path = fixture.run.join("state.json");
            let changing_budget = ScopedIoBudget {
                deadline: budget.deadline,
                cancelled: Arc::new(move || {
                    if read_counter.fetch_add(1, Ordering::SeqCst) == trigger {
                        attempted_write.store(true, Ordering::SeqCst);
                        match fs::write(&state_path, vec![2_u8; 64 * 1024]) {
                            Ok(()) => successful_write.store(true, Ordering::SeqCst),
                            Err(error) => *failed_writer.lock().unwrap() = error.raw_os_error(),
                        }
                    }
                    false
                }),
            };
            let changed_read = read_run_metadata(&root, &changing_budget);
            assert!(attempted.load(Ordering::SeqCst));
            if changed.load(Ordering::SeqCst) {
                assert!(matches!(
                    changed_read,
                    Err(CheckpointInputIoError::Changed { .. })
                ));
            } else {
                // A Windows no-write-share metadata handle may block the writer;
                // assert preserved bytes and actual refusal instead of skipping it.
                #[cfg(unix)]
                panic!("the Unix synthetic metadata writer must succeed");
                #[cfg(windows)]
                {
                    assert!(
                        matches!(*failed_write.lock().unwrap(), Some(5 | 32)),
                        "expected actual Windows write-sharing refusal"
                    );
                    assert_eq!(changed_read.unwrap().state.unwrap(), vec![1_u8; 64 * 1024]);
                }
            }
            fs::remove_file(fixture.run.join("state.json")).unwrap();
            fs::write(fixture.run.join("manifest.json"), b"manifest").unwrap();
            file_link(
                &fixture.run.join("manifest.json"),
                &fixture.run.join("state.json"),
            );
            assert!(matches!(
                read_run_metadata(&root, &budget),
                Err(CheckpointInputIoError::Refused { .. })
            ));
            drop(root);
            fs::remove_file(fixture.run.join("state.json")).unwrap();
            fs::write(fixture.run.join("state.json"), b"access-denial sentinel").unwrap();
            let io_root = fixture.root(&budget);
            let denial = DeniedMetadataAccess::new(&fixture.run.join("state.json"));
            assert!(matches!(
                read_run_metadata(&io_root, &budget),
                Err(CheckpointInputIoError::Io { .. })
            ));
            drop(denial);
            assert_eq!(
                fs::read(fixture.run.join("state.json")).unwrap(),
                b"access-denial sentinel"
            );
            drop(io_root);
            let changed_root = fixture.root(&budget);
            let displaced = fixture.runs.join("displaced-run");
            let rename = fs::rename(&fixture.run, &displaced);
            match rename {
                Ok(()) => assert!(matches!(
                    read_run_metadata(&changed_root, &budget),
                    Err(CheckpointInputIoError::Changed { .. })
                )),
                Err(error) => {
                    // Windows held no-share-delete ancestors enforce refusal before
                    // the root can move. Assert this actual confinement, not a skip.
                    #[cfg(unix)]
                    panic!("unexpected Unix rename error: {error}");
                    #[cfg(windows)]
                    {
                        assert!(
                            matches!(error.raw_os_error(), Some(5 | 32)),
                            "expected actual Windows rename-sharing refusal: {error}"
                        );
                        assert!(fixture.run.exists() && !displaced.exists());
                        assert_eq!(
                            changed_root.anchor.identity,
                            scoped_fs::snapshot_handle(&changed_root.anchor.directory)
                                .unwrap()
                                .identity
                        );
                    }
                }
            }
        }

        fn assert_oversize(fixture: &Fixture, limit: &'static str) {
            fixture.seal_root();
            let before = fixture.root_attributes();
            let budget = generous_budget();
            let root = fixture.root(&budget);
            let error = cleanup_checkpoint_input(&root, &budget).unwrap_err();
            assert!(
                matches!(&error, CheckpointInputIoError::Budget { limit: actual } if *actual == limit),
                "expected {limit}, got {error}"
            );
            assert_eq!(
                fixture.root_attributes(),
                before,
                "oversize must not chmod input root"
            );
            assert!(
                fixture.input.exists(),
                "oversize must not remove input root"
            );
        }

        fn wide_tree(files: usize, directories: usize) -> Fixture {
            let fixture = Fixture::new();
            for index in 0..files {
                fs::write(
                    fixture.input.join(format!("f-{index:04}")),
                    b"fixture payload",
                )
                .unwrap();
            }
            for index in 0..directories {
                fs::create_dir(fixture.input.join(format!("d-{index:04}"))).unwrap();
            }
            fixture
        }

        fn exact_cleanup(fixture: &Fixture) {
            fixture.seal_root();
            let budget = generous_budget();
            let root = fixture.root(&budget);
            cleanup_checkpoint_input(&root, &budget).unwrap();
            assert!(!fixture.input.exists());
        }

        pub(crate) fn preflight_budget() {
            reparent_refuses_outside_mutation("chmod");
            reparent_refuses_outside_mutation("unlink");
            exact_cleanup(&wide_tree(MAX_FILES, MAX_DIRECTORIES));
            let oversized_entries = wide_tree(MAX_FILES + 1, MAX_DIRECTORIES);
            assert_oversize(&oversized_entries, "entries");
            assert_eq!(
                fs::read_dir(&oversized_entries.input).unwrap().count(),
                MAX_ENTRIES + 1
            );
            exact_cleanup(&wide_tree(MAX_FILES, 0));
            let oversized_files = wide_tree(MAX_FILES + 1, 0);
            let file_before = fs::read(oversized_files.input.join("f-0000")).unwrap();
            assert_oversize(&oversized_files, "files");
            assert_eq!(
                fs::read(oversized_files.input.join("f-0000")).unwrap(),
                file_before
            );
            assert_eq!(
                fs::read_dir(&oversized_files.input).unwrap().count(),
                MAX_FILES + 1
            );
            exact_cleanup(&wide_tree(1, MAX_DIRECTORIES));
            let oversized_directories = wide_tree(1, MAX_DIRECTORIES + 1);
            assert_oversize(&oversized_directories, "directories");
            assert_eq!(
                fs::read_dir(&oversized_directories.input).unwrap().count(),
                MAX_DIRECTORIES + 2
            );
            let depth_fixture = |depth: usize| {
                let fixture = Fixture::new();
                let mut path = PathBuf::new();
                for _ in 1..depth {
                    path.push("d");
                }
                path.push("leaf");
                fixture.create(&path, false);
                fixture
            };
            exact_cleanup(&depth_fixture(MAX_DEPTH));
            assert_oversize(&depth_fixture(MAX_DEPTH + 1), "depth");
            let path_fixture = |last: usize| {
                let fixture = Fixture::new();
                let mut path = PathBuf::new();
                for _ in 0..16 {
                    path.push("d".repeat(254));
                }
                path.push("f".repeat(last));
                assert_eq!(
                    path.components()
                        .map(|c| c.as_os_str().len())
                        .sum::<usize>()
                        + 16,
                    4080 + last
                );
                fixture.create(&path, false);
                fixture
            };
            exact_cleanup(&path_fixture(16));
            assert_oversize(&path_fixture(17), "path bytes");
            let total_fixture = |plus_one: bool| {
                let fixture = Fixture::new();
                let mut parent = PathBuf::new();
                for _ in 0..7 {
                    parent.push("d".repeat(255));
                }
                // Directory path sum = 7161. 4096 leaf paths of 2046 bytes plus
                // 1031 one-byte extensions sum with it to exactly 8 MiB.
                for index in 0..MAX_FILES {
                    let length = if index < 1031 || (plus_one && index == 1031) {
                        255
                    } else {
                        254
                    };
                    let base = format!("f-{index:04}-");
                    let name = format!("{base}{}", "x".repeat(length - base.len()));
                    fixture.create(&parent.join(name), false);
                }
                fixture
            };
            exact_cleanup(&total_fixture(false));
            assert_oversize(&total_fixture(true), "total path bytes");
            let race = Fixture::new();
            race.create(Path::new("payload"), false);
            let budget = generous_budget();
            let root = race.root(&budget);
            let original = scoped_fs::open_directory(
                &root.anchor.directory,
                OsStr::new(super::super::CHECKPOINT_INPUT_DIR),
            )
            .unwrap();
            let snapshot = scoped_fs::snapshot_handle(&original).unwrap();
            let plan = preflight_input(&original, snapshot, &budget).unwrap();
            fs::write(race.input.join("unplanned"), b"keep").unwrap();
            assert!(matches!(
                verify_input_plan(&original, &plan, &budget),
                Err(CheckpointInputIoError::Changed { .. })
            ));
            assert!(race.input.join("payload").exists() && race.input.join("unplanned").exists());
            fs::create_dir(race.run.join("state.json")).unwrap();
            assert!(matches!(
                read_run_metadata(&root, &budget),
                Err(CheckpointInputIoError::Refused { .. })
            ));
        }

        pub(crate) fn deadline_cancel_owner() {
            // Pin the six typed user-facing error messages in the same shared
            // fixture, so these assertions execute on Unix and Windows alike.
            let messages = [
                (
                    CheckpointInputIoError::Cancelled,
                    "checkpoint input operation was cancelled; keep the run and retry only after confirming cancellation has cleared",
                ),
                (
                    CheckpointInputIoError::Deadline,
                    "checkpoint input operation reached its deadline; keep the run and inspect its bounded input before retrying",
                ),
                (
                    io_error(
                        "reading fixture metadata",
                        std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "synthetic fixture denial",
                        ),
                    ),
                    "checkpoint input reading fixture metadata failed: synthetic fixture denial; keep the run and inspect filesystem access before retrying",
                ),
                (
                    refused("a synthetic unsafe leaf"),
                    "checkpoint input confinement refused a synthetic unsafe leaf; keep the run and inspect links, identities and access manually",
                ),
                (
                    CheckpointInputIoError::Budget {
                        limit: "synthetic entries",
                    },
                    "checkpoint input synthetic entries budget exceeded; keep the run and inspect the oversized input manually",
                ),
                (
                    CheckpointInputIoError::Changed {
                        action: "checking synthetic identity",
                    },
                    "checkpoint input identity changed during checking synthetic identity; keep the run and inspect concurrent filesystem changes",
                ),
            ];
            for (error, expected) in messages {
                assert_eq!(error.to_string(), expected);
            }
            let fixture = wide_tree(256, 0);
            fixture.seal_root();
            let before = fixture.root_attributes();
            let root = fixture.root(&generous_budget());
            let checks = Arc::new(AtomicUsize::new(0));
            let completed = Arc::new(AtomicBool::new(false));
            let worker_checks = checks.clone();
            let worker_completed = completed.clone();
            let worker = thread::spawn(move || {
                let budget = ScopedIoBudget {
                    deadline: Instant::now() + Duration::from_secs(10),
                    cancelled: Arc::new(move || {
                        worker_checks.fetch_add(1, Ordering::SeqCst) >= 128
                    }),
                };
                let result = cleanup_checkpoint_input(&root, &budget);
                worker_completed.store(true, Ordering::SeqCst);
                result
            });
            assert!(matches!(
                worker.join().unwrap(),
                Err(CheckpointInputIoError::Cancelled)
            ));
            assert!(completed.load(Ordering::SeqCst));
            assert_eq!(fixture.root_attributes(), before);
            assert_eq!(fs::read_dir(&fixture.input).unwrap().count(), 256);
            let count_after_join = checks.load(Ordering::SeqCst);
            thread::sleep(Duration::from_millis(5));
            assert_eq!(checks.load(Ordering::SeqCst), count_after_join);
            let deadline_root = fixture.root(&generous_budget());
            let deadline = Instant::now() + Duration::from_millis(5);
            let start = Instant::now();
            let worker = thread::spawn(move || {
                let budget = ScopedIoBudget {
                    deadline,
                    cancelled: Arc::new(|| {
                        thread::sleep(Duration::from_millis(1));
                        false
                    }),
                };
                let metadata = read_run_metadata(&deadline_root, &budget);
                assert!(matches!(metadata, Err(CheckpointInputIoError::Deadline)));
                // The very same absolute deadline stays expired for cleanup.
                cleanup_checkpoint_input(&deadline_root, &budget)
            });
            assert!(matches!(
                worker.join().unwrap(),
                Err(CheckpointInputIoError::Deadline)
            ));
            assert!(start.elapsed() < Duration::from_secs(2));
            assert_eq!(fixture.root_attributes(), before);
            let partial = wide_tree(3, 0);
            partial.seal_root();
            let partial_root = partial.root(&generous_budget());
            let first_deleted = partial.input.join("f-0002");
            let cancelled = Arc::new(AtomicBool::new(false));
            let actual_cancel = cancelled.clone();
            let worker = thread::spawn(move || {
                let budget = ScopedIoBudget {
                    deadline: Instant::now() + Duration::from_secs(10),
                    cancelled: Arc::new(move || {
                        if !first_deleted.exists() {
                            actual_cancel.store(true, Ordering::SeqCst);
                        }
                        actual_cancel.load(Ordering::SeqCst)
                    }),
                };
                let result = cleanup_checkpoint_input(&partial_root, &budget);
                assert!(matches!(
                    read_run_metadata(&partial_root, &budget),
                    Err(CheckpointInputIoError::Cancelled)
                ));
                result
            });
            assert!(matches!(
                worker.join().unwrap(),
                Err(CheckpointInputIoError::Cancelled)
            ));
            assert!(cancelled.load(Ordering::SeqCst));
            assert!(!partial.input.join("f-0002").exists());
            assert!(partial.input.join("f-0000").exists() && partial.input.join("f-0001").exists());
            let retained = fs::read_dir(&partial.input).unwrap().count();
            thread::sleep(Duration::from_millis(5));
            assert_eq!(fs::read_dir(&partial.input).unwrap().count(), retained);
        }
    }
}

#[allow(dead_code, unused_imports)]
pub(crate) use scoped_io::{
    CheckpointInputIoError, MetadataIdentity, RunMetadataBytes, RunRootHandle, ScopedIoBudget,
    cleanup_checkpoint_input, open_scoped_run_root, read_run_metadata,
};
