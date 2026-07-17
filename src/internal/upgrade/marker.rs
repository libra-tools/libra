//! Official-install marker (plan-20260714 §A.2/§A.4).
//!
//! `INSTALL_DIR/.libra-official-install.json` records how the current target
//! binary was installed. An installation is OFFICIAL only when ALL hold
//! (§A.2):
//!
//! 1. the marker parses and carries
//!    `install_source == "official_signed_manifest"`;
//! 2. the marker's `version`/`sha256`/`size`/`platform` match the actual
//!    target binary;
//! 3. the install directory passes the §A.5 ownership/permission/no-follow
//!    validation (enforced by [`super::lock::InstallDir`]).
//!
//! Neither "the file happens to live in the default directory" nor "the
//! binary hashes to itself" can establish official provenance on its own —
//! the marker is only ever written after a signed-manifest install, and a
//! marker that fails validation renders the install NON-official (auto mode
//! then degrades per §A.2).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::lock::{EntryKind, InstallDir, InstallDirError};

/// Marker file name inside the install directory.
pub const MARKER_FILE_NAME: &str = ".libra-official-install.json";

/// The only accepted provenance value (§A.4).
pub const OFFICIAL_INSTALL_SOURCE: &str = "official_signed_manifest";

/// Installed target name required on the normal command path (§A.2).
pub const TARGET_BINARY_NAME: &str = "libra";

/// On-disk marker document (§A.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallMarker {
    pub schema_version: u32,
    /// RFC3339 install time (informational).
    pub installed_at: String,
    pub install_source: String,
    /// Release-matrix platform id (e.g. `darwin-arm64`).
    pub platform: String,
    /// Release version installed (canonical `X.Y.Z`).
    pub version: String,
    /// Lowercase 64-hex sha256 of the installed target binary.
    pub sha256: String,
    pub size: u64,
    /// Key id of the manifest signature that authorized the install.
    pub manifest_key_id: String,
}

/// Why an install is not official.
#[derive(Debug, thiserror::Error)]
pub enum MarkerError {
    #[error(transparent)]
    Dir(#[from] InstallDirError),
    #[error("install marker {MARKER_FILE_NAME} is not valid JSON: {0}")]
    Corrupt(String),
    #[error("install marker has unsupported schema_version {0}")]
    Schema(u32),
    #[error("install marker install_source '{0}' is not '{OFFICIAL_INSTALL_SOURCE}'")]
    WrongSource(String),
    #[error("install marker field {field} does not match the target binary ({detail})")]
    TargetMismatch { field: &'static str, detail: String },
    #[error("target binary '{TARGET_BINARY_NAME}' is missing or not a regular file")]
    TargetMissing,
}

/// Read and structurally validate the marker. `Ok(None)` when absent (the
/// install is simply non-official); corrupt/mismatched schema is an error so
/// callers can degrade loudly rather than silently (§A.2).
pub fn read_marker(dir: &InstallDir) -> Result<Option<InstallMarker>, MarkerError> {
    let Some(bytes) = dir.read_file(MARKER_FILE_NAME)? else {
        return Ok(None);
    };
    let marker: InstallMarker =
        serde_json::from_slice(&bytes).map_err(|e| MarkerError::Corrupt(e.to_string()))?;
    if marker.schema_version != 1 {
        return Err(MarkerError::Schema(marker.schema_version));
    }
    if marker.install_source != OFFICIAL_INSTALL_SOURCE {
        return Err(MarkerError::WrongSource(marker.install_source.clone()));
    }
    Ok(Some(marker))
}

/// Atomically persist the marker (`0600`, fd-relative, §A.5 discipline).
pub fn write_marker(dir: &InstallDir, marker: &InstallMarker) -> Result<(), MarkerError> {
    let mut bytes =
        serde_json::to_vec_pretty(marker).map_err(|e| MarkerError::Corrupt(e.to_string()))?;
    bytes.push(b'\n');
    dir.write_file_atomic(MARKER_FILE_NAME, &bytes, 0o600)?;
    Ok(())
}

/// Hash + size of the current target binary, read via the directory fd.
pub fn target_identity(dir: &InstallDir) -> Result<(String, u64), MarkerError> {
    match dir.stat_entry(TARGET_BINARY_NAME)? {
        Some(EntryKind::Regular { .. }) => {}
        _ => return Err(MarkerError::TargetMissing),
    }
    let bytes = dir
        .read_file(TARGET_BINARY_NAME)?
        .ok_or(MarkerError::TargetMissing)?;
    let digest = hex::encode(Sha256::digest(&bytes));
    Ok((digest, bytes.len() as u64))
}

/// Full §A.2 official check for the target inside `dir`. Returns the marker
/// when every condition holds.
pub fn official_marker_for_target(
    dir: &InstallDir,
    expected_platform: &str,
) -> Result<Option<InstallMarker>, MarkerError> {
    let Some(marker) = read_marker(dir)? else {
        return Ok(None);
    };
    if marker.platform != expected_platform {
        return Err(MarkerError::TargetMismatch {
            field: "platform",
            detail: format!(
                "marker '{}' vs running platform '{expected_platform}'",
                marker.platform
            ),
        });
    }
    let (actual_sha256, actual_size) = target_identity(dir)?;
    if !marker.sha256.eq_ignore_ascii_case(&actual_sha256) {
        return Err(MarkerError::TargetMismatch {
            field: "sha256",
            detail: format!("marker {} vs target {actual_sha256}", marker.sha256),
        });
    }
    if marker.size != actual_size {
        return Err(MarkerError::TargetMismatch {
            field: "size",
            detail: format!("marker {} vs target {actual_size}", marker.size),
        });
    }
    Ok(Some(marker))
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn install_dir_with_target(contents: &[u8]) -> (tempfile::TempDir, InstallDir) {
        let guard = tempfile::tempdir().unwrap();
        let path = guard.path().canonicalize().unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = InstallDir::open_validated(&path).unwrap();
        dir.write_file_atomic(TARGET_BINARY_NAME, contents, 0o755)
            .unwrap();
        (guard, dir)
    }

    fn marker_for(contents: &[u8]) -> InstallMarker {
        InstallMarker {
            schema_version: 1,
            installed_at: "2026-07-17T00:00:00Z".into(),
            install_source: OFFICIAL_INSTALL_SOURCE.into(),
            platform: "darwin-arm64".into(),
            version: "1.2.3".into(),
            sha256: hex::encode(Sha256::digest(contents)),
            size: contents.len() as u64,
            manifest_key_id: "test-key-1".into(),
        }
    }

    #[test]
    fn absent_marker_is_simply_non_official() {
        let (_g, dir) = install_dir_with_target(b"binary");
        assert!(read_marker(&dir).unwrap().is_none());
        assert!(
            official_marker_for_target(&dir, "darwin-arm64")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn valid_marker_round_trips_and_validates() {
        let (_g, dir) = install_dir_with_target(b"binary");
        write_marker(&dir, &marker_for(b"binary")).unwrap();
        let marker = official_marker_for_target(&dir, "darwin-arm64")
            .unwrap()
            .expect("official");
        assert_eq!(marker.version, "1.2.3");
        assert_eq!(marker.manifest_key_id, "test-key-1");
    }

    #[test]
    fn wrong_source_corrupt_and_mismatches_are_errors_not_official() {
        let (_g, dir) = install_dir_with_target(b"binary");
        // Wrong provenance value.
        let mut wrong = marker_for(b"binary");
        wrong.install_source = "manual-copy".into();
        write_marker(&dir, &wrong).unwrap();
        assert!(matches!(
            read_marker(&dir),
            Err(MarkerError::WrongSource(_))
        ));
        // Hash mismatch: a marker copied next to a DIFFERENT binary can
        // never establish provenance (§A.2).
        write_marker(&dir, &marker_for(b"other-binary")).unwrap();
        assert!(matches!(
            official_marker_for_target(&dir, "darwin-arm64"),
            Err(MarkerError::TargetMismatch {
                field: "sha256",
                ..
            })
        ));
        // Platform mismatch.
        write_marker(&dir, &marker_for(b"binary")).unwrap();
        assert!(matches!(
            official_marker_for_target(&dir, "linux-amd64"),
            Err(MarkerError::TargetMismatch {
                field: "platform",
                ..
            })
        ));
        // Corrupt JSON.
        dir.write_file_atomic(MARKER_FILE_NAME, b"{ nope", 0o600)
            .unwrap();
        assert!(matches!(read_marker(&dir), Err(MarkerError::Corrupt(_))));
        // Unsupported schema.
        let mut future = marker_for(b"binary");
        future.schema_version = 2;
        write_marker(&dir, &future).unwrap();
        assert!(matches!(read_marker(&dir), Err(MarkerError::Schema(2))));
    }

    #[test]
    fn missing_target_is_never_official() {
        let (_g, dir) = install_dir_with_target(b"binary");
        write_marker(&dir, &marker_for(b"binary")).unwrap();
        dir.remove_file(TARGET_BINARY_NAME).unwrap();
        assert!(matches!(
            official_marker_for_target(&dir, "darwin-arm64"),
            Err(MarkerError::TargetMissing)
        ));
    }
}
