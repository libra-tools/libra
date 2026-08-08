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

use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use git_internal::hash::ObjectHash;
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
    let mut total: u64 = 0;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for file in &spec.files {
        let rel = sanitize_rel_path(&file.rel_path)?;
        let oid = ObjectHash::from_str(&file.oid).map_err(|e| {
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
        for dir in dirs.iter().rev().chain(std::iter::once(&root)) {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555))
                .map_err(|e| format!("failed to make checkpoint input dir read-only: {e}"))?;
        }
    }
    Ok(root)
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
}
