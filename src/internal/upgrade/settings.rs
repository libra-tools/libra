//! `{LIBRA_HOME}/upgrade/settings.json` — the auto-upgrade mode switch
//! (plan-20260714 §A.3).
//!
//! The upgrade configuration is a reserved namespace stored in a standalone
//! JSON file, never in the SQLite `config_kv` table. Two read paths with
//! different failure contracts exist on purpose:
//!
//! - [`read_mode`] (strict) backs the `libra config` surface: a missing file
//!   reads as `off`, but a corrupt or unsupported file is a hard error the
//!   user must fix.
//! - [`effective_mode_for_upgrade`] (lenient) backs the auto-upgrade decision
//!   itself: any unknown or corrupt state degrades to `off` with a
//!   once-per-process warning, so a damaged settings file can never trigger
//!   an upgrade — and never breaks unrelated commands either.

use std::{
    fmt, fs, io,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

use serde::Deserialize;

use super::home::{LibraHomeError, resolve_libra_home};

/// The single supported config key of the reserved `upgrade.*` namespace.
pub const UPGRADE_MODE_KEY: &str = "upgrade.mode";

/// Schema version this binary reads and writes.
pub const UPGRADE_SETTINGS_SCHEMA_VERSION: u64 = 1;

/// The auto-upgrade mode (`upgrade.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpgradeMode {
    /// Check for and install official upgrades automatically.
    Auto,
    /// Never check for upgrades (the default).
    #[default]
    Off,
    /// Only upgrade when the user explicitly asks.
    Manual,
}

impl UpgradeMode {
    /// Parse a mode value, case-insensitively (§A.3). Only the exact three
    /// enum spellings are accepted — no whitespace normalization, so a padded
    /// CLI value or hand-edited `" auto "` on disk is an error, not `auto`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "off" => Some(Self::Off),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    /// Canonical lowercase spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Manual => "manual",
        }
    }
}

impl fmt::Display for UpgradeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// On-disk document shape. Unknown fields are ignored for forward
/// compatibility; an unknown `schema_version` is rejected instead, because it
/// may change the meaning of known fields. `mode` is required and non-null —
/// only a missing FILE reads as `off`; an existing file without a valid mode
/// is damaged state and must surface as an error, not silently as `off`.
#[derive(Deserialize)]
struct SettingsDocument {
    #[serde(default = "default_schema_version")]
    schema_version: u64,
    mode: String,
}

fn default_schema_version() -> u64 {
    UPGRADE_SETTINGS_SCHEMA_VERSION
}

/// Failures of the strict settings read/write paths.
#[derive(Debug, thiserror::Error)]
pub enum UpgradeSettingsError {
    /// The Libra home directory could not be determined.
    #[error(transparent)]
    Home(#[from] LibraHomeError),
    /// The settings file exists but could not be read.
    #[error("cannot read upgrade settings at {path}: {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The settings file is present but not usable by this binary.
    #[error(
        "upgrade settings file {path} is invalid: {detail}; rewrite it with: \
         libra config set --global upgrade.mode <auto|manual|off>"
    )]
    Invalid { path: PathBuf, detail: String },
    /// The settings file could not be written.
    #[error("cannot write upgrade settings at {path}: {source}")]
    WriteFailed {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Path of the upgrade settings file: `{LIBRA_HOME}/upgrade/settings.json`.
pub fn settings_path() -> Result<PathBuf, UpgradeSettingsError> {
    Ok(resolve_libra_home()?.join("upgrade").join("settings.json"))
}

/// Strict read for the `libra config` surface.
///
/// # Returns
/// - `Ok(None)` — the settings file does not exist (renders as `off`).
/// - `Ok(Some(mode))` — the stored mode.
///
/// # Errors
/// Any unreadable, non-JSON, unknown-schema, missing-mode or invalid-mode
/// file (§A.3: "文件损坏→get 错").
pub fn read_mode() -> Result<Option<UpgradeMode>, UpgradeSettingsError> {
    let path = settings_path()?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(UpgradeSettingsError::Unreadable { path, source: err }),
    };
    let document: SettingsDocument =
        serde_json::from_slice(&bytes).map_err(|err| UpgradeSettingsError::Invalid {
            path: path.clone(),
            detail: format!("not valid JSON: {err}"),
        })?;
    if document.schema_version != UPGRADE_SETTINGS_SCHEMA_VERSION {
        return Err(UpgradeSettingsError::Invalid {
            path,
            detail: format!(
                "unsupported schema_version {} (this binary supports {})",
                document.schema_version, UPGRADE_SETTINGS_SCHEMA_VERSION
            ),
        });
    }
    match UpgradeMode::parse(&document.mode) {
        Some(mode) => Ok(Some(mode)),
        None => Err(UpgradeSettingsError::Invalid {
            path,
            detail: format!("mode '{}' is not one of auto, manual, off", document.mode),
        }),
    }
}

/// Atomically persist `mode` (used by both `set` and `unset` — `unset` writes
/// `off` and keeps the file, §A.3).
///
/// Creates `{LIBRA_HOME}/upgrade/` as needed. On Unix the directory is
/// `0700` and the file `0600`, matching the upgrade state-file permission
/// policy (§A.5).
///
/// # Returns
/// The settings file path.
pub fn write_mode(mode: UpgradeMode) -> Result<PathBuf, UpgradeSettingsError> {
    let path = settings_path()?;
    let parent = path.parent().ok_or_else(|| UpgradeSettingsError::Invalid {
        path: path.clone(),
        detail: "settings path has no parent directory".to_string(),
    })?;
    let write_failed = |err: io::Error| UpgradeSettingsError::WriteFailed {
        path: path.clone(),
        source: err,
    };
    crate::utils::atomic_write::ensure_dir_exists(parent, true).map_err(write_failed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(write_failed)?;
    }
    let document = serde_json::json!({
        "schema_version": UPGRADE_SETTINGS_SCHEMA_VERSION,
        "mode": mode.as_str(),
    });
    let mut bytes =
        serde_json::to_vec_pretty(&document).map_err(|err| UpgradeSettingsError::Invalid {
            path: path.clone(),
            detail: format!("cannot serialize settings: {err}"),
        })?;
    bytes.push(b'\n');
    crate::utils::atomic_write::write_atomic(&path, &bytes, true).map_err(write_failed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(write_failed)?;
    }
    Ok(path)
}

/// Whether the lenient read already warned in this process (warn once, §A.3).
static LENIENT_READ_WARNED: AtomicBool = AtomicBool::new(false);

/// Test-only reset of the once-per-process warning latch.
#[cfg(test)]
pub(crate) fn reset_lenient_warning_for_tests() {
    LENIENT_READ_WARNED.store(false, Ordering::Relaxed);
}

/// Lenient read for the auto-upgrade decision path itself.
///
/// Any failure (unresolvable home, unreadable or corrupt file) degrades to
/// [`UpgradeMode::Off`] with a once-per-process warning (§A.3: 升级路径未知/
/// 损坏均视 `off` 并 warning 一次) — a damaged settings file must never
/// trigger an upgrade or break the user's actual command.
pub fn effective_mode_for_upgrade() -> UpgradeMode {
    match read_mode() {
        Ok(Some(mode)) => mode,
        Ok(None) => UpgradeMode::Off,
        Err(err) => {
            if !LENIENT_READ_WARNED.swap(true, Ordering::Relaxed) {
                crate::utils::error::emit_warning(format!(
                    "auto-upgrade disabled (treating upgrade.mode as off): {err}"
                ));
            }
            UpgradeMode::Off
        }
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;
    use crate::utils::test::ScopedEnvVar;

    fn scoped_home() -> (tempfile::TempDir, ScopedEnvVar) {
        let dir = tempfile::tempdir().expect("test fixture operation should succeed");
        let env = ScopedEnvVar::set(super::super::home::LIBRA_HOME_ENV, dir.path());
        (dir, env)
    }

    #[test]
    fn invalid_error_display_is_actionable() {
        // Pin the user-facing Display: names the file and the exact rewrite command.
        let err = UpgradeSettingsError::Invalid {
            path: PathBuf::from("/x/upgrade/settings.json"),
            detail: "boom".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("/x/upgrade/settings.json"), "{msg}");
        assert!(msg.contains("boom"), "{msg}");
        assert!(
            msg.contains("libra config set --global upgrade.mode <auto|manual|off>"),
            "{msg}"
        );
    }

    #[test]
    fn parse_is_case_insensitive_and_rejects_unknown() {
        assert_eq!(UpgradeMode::parse("AUTO"), Some(UpgradeMode::Auto));
        assert_eq!(UpgradeMode::parse("Manual"), Some(UpgradeMode::Manual));
        assert_eq!(UpgradeMode::parse("off"), Some(UpgradeMode::Off));
        assert_eq!(UpgradeMode::parse("on"), None);
        assert_eq!(UpgradeMode::parse(""), None);
        // No whitespace normalization: padded values are invalid (§A.3).
        assert_eq!(UpgradeMode::parse(" auto "), None);
        assert_eq!(UpgradeMode::parse("auto\n"), None);
    }

    #[test]
    #[serial(env)]
    fn padded_on_disk_mode_is_rejected() {
        let (_dir, _env) = scoped_home();
        let path = settings_path().expect("test fixture operation should succeed");
        fs::create_dir_all(
            path.parent()
                .expect("test fixture operation should succeed"),
        )
        .expect("test fixture operation should succeed");
        fs::write(&path, br#"{ "schema_version": 1, "mode": " auto " }"#)
            .expect("test fixture operation should succeed");
        assert!(matches!(
            read_mode(),
            Err(UpgradeSettingsError::Invalid { .. })
        ));
    }

    #[test]
    #[serial(env, warning_tracker)]
    fn lenient_read_warns_exactly_once_per_process() {
        use crate::utils::output::{reset_warning_tracker, warning_was_emitted};
        let (_dir, _env) = scoped_home();
        let path = settings_path().expect("test fixture operation should succeed");
        fs::create_dir_all(
            path.parent()
                .expect("test fixture operation should succeed"),
        )
        .expect("test fixture operation should succeed");
        fs::write(&path, b"{ corrupt").expect("test fixture operation should succeed");

        reset_lenient_warning_for_tests();
        reset_warning_tracker();
        assert_eq!(effective_mode_for_upgrade(), UpgradeMode::Off);
        assert!(
            warning_was_emitted(),
            "first lenient read of corrupt settings must warn"
        );

        // The latch suppresses every subsequent warning in this process.
        reset_warning_tracker();
        assert_eq!(effective_mode_for_upgrade(), UpgradeMode::Off);
        assert!(
            !warning_was_emitted(),
            "second lenient read must not warn again"
        );
    }

    #[test]
    #[serial(env)]
    fn missing_file_reads_as_none() {
        let (_dir, _env) = scoped_home();
        assert_eq!(
            read_mode().expect("test fixture operation should succeed"),
            None
        );
    }

    #[test]
    #[serial(env)]
    fn write_then_read_roundtrips_and_keeps_file_on_off() {
        let (_dir, _env) = scoped_home();
        let path = write_mode(UpgradeMode::Auto).expect("test fixture operation should succeed");
        assert_eq!(
            read_mode().expect("test fixture operation should succeed"),
            Some(UpgradeMode::Auto)
        );
        // `unset` semantics: write off, keep the file.
        write_mode(UpgradeMode::Off).expect("test fixture operation should succeed");
        assert!(path.exists(), "unset must keep the settings file");
        assert_eq!(
            read_mode().expect("test fixture operation should succeed"),
            Some(UpgradeMode::Off)
        );
    }

    #[test]
    #[serial(env)]
    #[cfg(unix)]
    fn written_file_and_dir_have_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, _env) = scoped_home();
        let path = write_mode(UpgradeMode::Manual).expect("test fixture operation should succeed");
        let file_mode = fs::metadata(&path)
            .expect("test fixture operation should succeed")
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = fs::metadata(
            path.parent()
                .expect("test fixture operation should succeed"),
        )
        .expect("test fixture operation should succeed")
        .permissions()
        .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    #[serial(env)]
    fn corrupt_json_is_a_strict_error_but_lenient_off() {
        let (_dir, _env) = scoped_home();
        let path = settings_path().expect("test fixture operation should succeed");
        fs::create_dir_all(
            path.parent()
                .expect("test fixture operation should succeed"),
        )
        .expect("test fixture operation should succeed");
        fs::write(&path, b"{ not json").expect("test fixture operation should succeed");
        assert!(matches!(
            read_mode(),
            Err(UpgradeSettingsError::Invalid { .. })
        ));
        assert_eq!(effective_mode_for_upgrade(), UpgradeMode::Off);
    }

    #[test]
    #[serial(env)]
    fn invalid_mode_value_is_a_strict_error() {
        let (_dir, _env) = scoped_home();
        let path = settings_path().expect("test fixture operation should succeed");
        fs::create_dir_all(
            path.parent()
                .expect("test fixture operation should succeed"),
        )
        .expect("test fixture operation should succeed");
        fs::write(&path, br#"{ "schema_version": 1, "mode": "sometimes" }"#)
            .expect("test fixture operation should succeed");
        assert!(matches!(
            read_mode(),
            Err(UpgradeSettingsError::Invalid { .. })
        ));
    }

    #[test]
    #[serial(env)]
    fn newer_schema_version_is_rejected() {
        let (_dir, _env) = scoped_home();
        let path = settings_path().expect("test fixture operation should succeed");
        fs::create_dir_all(
            path.parent()
                .expect("test fixture operation should succeed"),
        )
        .expect("test fixture operation should succeed");
        fs::write(&path, br#"{ "schema_version": 2, "mode": "auto" }"#)
            .expect("test fixture operation should succeed");
        assert!(matches!(
            read_mode(),
            Err(UpgradeSettingsError::Invalid { .. })
        ));
    }

    #[test]
    #[serial(env)]
    fn missing_or_null_mode_field_is_damaged_state() {
        let (_dir, _env) = scoped_home();
        let path = settings_path().expect("test fixture operation should succeed");
        fs::create_dir_all(
            path.parent()
                .expect("test fixture operation should succeed"),
        )
        .expect("test fixture operation should succeed");
        // An existing file must carry a valid mode: missing/null must NOT be
        // conflated with an explicit `off`.
        for body in [
            br#"{}"#.as_slice(),
            br#"{ "schema_version": 1 }"#.as_slice(),
            br#"{ "schema_version": 1, "mode": null }"#.as_slice(),
        ] {
            fs::write(&path, body).expect("test fixture operation should succeed");
            assert!(
                matches!(read_mode(), Err(UpgradeSettingsError::Invalid { .. })),
                "body {:?} must be a strict error",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    #[serial(env)]
    fn unknown_fields_are_ignored() {
        let (_dir, _env) = scoped_home();
        let path = settings_path().expect("test fixture operation should succeed");
        fs::create_dir_all(
            path.parent()
                .expect("test fixture operation should succeed"),
        )
        .expect("test fixture operation should succeed");
        fs::write(
            &path,
            br#"{ "schema_version": 1, "mode": "manual", "future_field": true }"#,
        )
        .expect("test fixture operation should succeed");
        assert_eq!(
            read_mode().expect("test fixture operation should succeed"),
            Some(UpgradeMode::Manual)
        );
    }
}
