//! Storage backend selection, compile-time feature dispatch, and pre-integration
//! runtime gate (`ADR-RB-01` #3).
//!
//! While [`REDB_RUNTIME_UNLOCKED`] is `false` (Track A through `RB-29`), any
//! runtime request selecting [`StorageBackendKind::Redb`] or calling
//! [`assert_redb_runtime_unlocked`] fails closed with `LBR-CLI-003`, ignoring
//! `LIBRA_TEST` and all other environment variables.

use std::{env, ffi::OsString, fmt};

use serde::{Deserialize, Serialize};

use crate::storage::{
    error::{StorageBackendError, StorageResult},
    has_redb_backend, has_sqlite_backend,
};

/// Environment variable allowing new repositories to select a storage backend
/// when no manifest or legacy database exists yet.
pub const LIBRA_STORAGE_BACKEND_ENV: &str = "LIBRA_STORAGE_BACKEND";

/// Compile-time pre-integration lock for the `redb` runtime and `libra-migrate`
/// live cutover commands (`ADR-RB-01` #3).
///
/// Remains `false` throughout Track A and Track B until `RB-29a` unlocks it
/// atomically alongside full 146-file wiring. There is intentionally no runtime
/// environment bypass (including `LIBRA_TEST`).
pub const REDB_RUNTIME_UNLOCKED: bool = false;

/// Supported metadata storage engine backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackendKind {
    /// Legacy SQLite (SeaORM) storage backend.
    Sqlite,
    /// Dual-file `redb 5.0` storage backend with capture frame queue.
    Redb,
}

impl StorageBackendKind {
    /// Canonical lowercase identifier used in manifests and environment overrides.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Redb => "redb",
        }
    }

    /// Returns whether this backend is compiled into the current binary.
    #[must_use]
    pub const fn is_compiled(self) -> bool {
        match self {
            Self::Sqlite => has_sqlite_backend(),
            Self::Redb => has_redb_backend(),
        }
    }

    /// Compile-time default backend for newly initialized repositories when no
    /// manifest, legacy database, or environment override applies.
    ///
    /// Defaults to `Sqlite` whenever `storage-sqlite` is compiled (prior to
    /// `RB-33` runtime default cutover), and `Redb` in `storage-redb`-only builds.
    #[must_use]
    pub const fn compiled_default() -> Self {
        Self::compiled_default_for(has_sqlite_backend(), has_redb_backend())
    }

    /// Parse a `LIBRA_STORAGE_BACKEND` value (`sqlite` or `redb`, case-insensitive).
    ///
    /// Invalid values are reported with a fixed sanitized placeholder rather than
    /// echoing arbitrary environment text (`GC-RB-03` / `ER-11`).
    pub fn parse_env_value(raw: &str) -> StorageResult<Self> {
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("sqlite") {
            Ok(Self::Sqlite)
        } else if trimmed.eq_ignore_ascii_case("redb") {
            Ok(Self::Redb)
        } else {
            let placeholder = if trimmed.is_empty() {
                "<empty>"
            } else {
                "<invalid>"
            };
            Err(StorageBackendError::UnsupportedBackend {
                requested: placeholder.to_string(),
                detail: format!("expected `sqlite` or `redb` in `{LIBRA_STORAGE_BACKEND_ENV}`"),
            })
        }
    }

    /// Resolve the active backend from the process environment (`LIBRA_STORAGE_BACKEND`),
    /// enforcing compiled feature availability and the compile-time
    /// [`REDB_RUNTIME_UNLOCKED`] gate.
    ///
    /// Invalid UTF-8 in `LIBRA_STORAGE_BACKEND` fails closed (`LBR-CLI-003`)
    /// rather than silently falling back to the default backend.
    pub fn resolve() -> StorageResult<Self> {
        Self::resolve_with_env_lookup(|key| env::var_os(key))
    }

    /// Resolve the active backend using an injected environment lookup closure
    /// (reads only [`LIBRA_STORAGE_BACKEND_ENV`]; never inspects `LIBRA_TEST`).
    pub fn resolve_with_env_lookup<F>(lookup: F) -> StorageResult<Self>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        match lookup(LIBRA_STORAGE_BACKEND_ENV) {
            None => Self::resolve_from_override(None),
            Some(os_val) => {
                let text = os_val.into_string().map_err(|_| {
                    StorageBackendError::UnsupportedBackend {
                        requested: "<non-utf8>".to_string(),
                        detail: format!(
                            "`{LIBRA_STORAGE_BACKEND_ENV}` must be valid UTF-8 (`sqlite` or `redb`)"
                        ),
                    }
                })?;
                Self::resolve_from_override(Some(text.as_str()))
            }
        }
    }

    /// Resolve the active backend from an optional explicit override string
    /// against this binary's compiled features and [`REDB_RUNTIME_UNLOCKED`].
    pub fn resolve_from_override(env_override: Option<&str>) -> StorageResult<Self> {
        Self::resolve_with_capabilities(env_override, has_sqlite_backend(), has_redb_backend())
    }

    /// Core feature-capability and compile-time gate resolution logic.
    pub(crate) fn resolve_with_capabilities(
        env_override: Option<&str>,
        sqlite_compiled: bool,
        redb_compiled: bool,
    ) -> StorageResult<Self> {
        let selected = match env_override {
            Some(raw) => Self::parse_env_value(raw)?,
            None => Self::compiled_default_for(sqlite_compiled, redb_compiled),
        };

        match selected {
            Self::Sqlite if !sqlite_compiled => {
                return Err(StorageBackendError::UnsupportedBackend {
                    requested: "sqlite".to_string(),
                    detail: "this Libra binary was compiled without `storage-sqlite`; rebuild with `--features storage-sqlite` or migrate the repository to `redb` with `libra-migrate --to redb`".to_string(),
                });
            }
            Self::Redb if !redb_compiled => {
                return Err(StorageBackendError::UnsupportedBackend {
                    requested: "redb".to_string(),
                    detail: "this Libra binary was compiled without `storage-redb`; rebuild with `--features storage-redb` or migrate back with `libra-migrate --to sqlite`".to_string(),
                });
            }
            _ => {}
        }

        if selected == Self::Redb {
            assert_redb_runtime_unlocked()?;
        }

        Ok(selected)
    }

    const fn compiled_default_for(sqlite_compiled: bool, _redb_compiled: bool) -> Self {
        if sqlite_compiled {
            Self::Sqlite
        } else {
            Self::Redb
        }
    }
}

impl fmt::Display for StorageBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Enforce the compile-time pre-integration runtime lock (`ADR-RB-01` #3).
///
/// Returns `Err(StorageBackendError::PreReleaseGateLocked)` (`LBR-CLI-003`)
/// unconditionally while [`REDB_RUNTIME_UNLOCKED`] is `false`, ignoring
/// `LIBRA_TEST` and every other runtime environment variable.
pub fn assert_redb_runtime_unlocked() -> StorageResult<()> {
    if !REDB_RUNTIME_UNLOCKED {
        return Err(StorageBackendError::PreReleaseGateLocked {
            detail: "compile-time constant `REDB_RUNTIME_UNLOCKED` is `false`; live `redb` CLI runtime and `libra-migrate` cutover remain disabled until integration cutover (`RB-29a`)".to_string(),
        });
    }
    Ok(())
}

#[cfg(all(test, feature = "storage-sqlite", feature = "storage-redb"))]
mod tests {
    use std::{ffi::OsString, process::Command};

    use super::*;

    #[test]
    fn dual_feature_resolution_and_pre_release_cli_gate() {
        // Isolated child-process mode exercises the real public `StorageBackendKind::resolve()`
        // entry point directly against live process env vars (`LIBRA_STORAGE_BACKEND=redb`, `LIBRA_TEST=1`).
        if let Some(mode) = env::var_os("LIBRA_RB01C_CHILD_MODE") {
            match mode.to_str() {
                Some("locked_redb") => {
                    let err = StorageBackendKind::resolve().expect_err(
                        "public resolve() must fail closed for redb even with LIBRA_TEST=1",
                    );
                    assert!(
                        matches!(err, StorageBackendError::PreReleaseGateLocked { .. }),
                        "expected PreReleaseGateLocked from resolve(), got {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-CLI-003");
                    return;
                }
                Some("default_unset") => {
                    let kind = StorageBackendKind::resolve().expect(
                        "public resolve() with unset env must select sqlite before cutover",
                    );
                    assert_eq!(kind, StorageBackendKind::Sqlite);
                    return;
                }
                other => panic!("unexpected LIBRA_RB01C_CHILD_MODE: {other:?}"),
            }
        }

        const {
            assert!(
                !REDB_RUNTIME_UNLOCKED,
                "REDB_RUNTIME_UNLOCKED must stay false prior to RB-29a"
            );
        }

        // AC4: assert_redb_runtime_unlocked() fails closed with LBR-CLI-003.
        let gate_err = assert_redb_runtime_unlocked()
            .expect_err("assert_redb_runtime_unlocked must fail while locked");
        assert!(
            matches!(gate_err, StorageBackendError::PreReleaseGateLocked { .. }),
            "expected PreReleaseGateLocked, got {gate_err:?}"
        );
        assert_eq!(gate_err.stable_code_str(), "LBR-CLI-003");

        // AC3 (process-level public resolve() verification with real env vars).
        let exe = env::current_exe().expect("locate current test executable");
        let locked_status = Command::new(&exe)
            .arg("--exact")
            .arg("storage::backend::tests::dual_feature_resolution_and_pre_release_cli_gate")
            .arg("--nocapture")
            .env("LIBRA_RB01C_CHILD_MODE", "locked_redb")
            .env(LIBRA_STORAGE_BACKEND_ENV, "redb")
            .env("LIBRA_TEST", "1")
            .status()
            .expect("spawn isolated locked_redb child");
        assert!(
            locked_status.success(),
            "isolated locked_redb child failed with {locked_status}"
        );

        let default_status = Command::new(&exe)
            .arg("--exact")
            .arg("storage::backend::tests::dual_feature_resolution_and_pre_release_cli_gate")
            .arg("--nocapture")
            .env("LIBRA_RB01C_CHILD_MODE", "default_unset")
            .env_remove(LIBRA_STORAGE_BACKEND_ENV)
            .status()
            .expect("spawn isolated default_unset child");
        assert!(
            default_status.success(),
            "isolated default_unset child failed with {default_status}"
        );

        // Explicit sqlite override succeeds in dual-feature builds.
        assert_eq!(
            StorageBackendKind::resolve_with_env_lookup(|key| match key {
                LIBRA_STORAGE_BACKEND_ENV => Some(OsString::from("SQLITE")),
                _ => None,
            })
            .expect("explicit sqlite override must succeed"),
            StorageBackendKind::Sqlite
        );

        // Unknown, empty, and sensitive invalid override values fail closed with
        // LBR-CLI-003 and NEVER echo the raw invalid text (GC-RB-03).
        let sensitive_input = "/Users/private-home/secret-token=sk-live-98765";
        for bad in ["", "   ", "postgres", sensitive_input] {
            let err = StorageBackendKind::resolve_from_override(Some(bad))
                .expect_err("invalid override value must fail closed");
            assert!(
                matches!(err, StorageBackendError::UnsupportedBackend { .. }),
                "expected UnsupportedBackend for invalid override, got {err:?}"
            );
            assert_eq!(err.stable_code_str(), "LBR-CLI-003");
            let displayed = err.to_string();
            assert!(
                !displayed.contains(sensitive_input) && !displayed.contains("sk-live-98765"),
                "invalid override Display must not echo sensitive input: {displayed}"
            );
            let cli_rendered = err.into_cli_error().render();
            assert!(
                !cli_rendered.contains(sensitive_input) && !cli_rendered.contains("sk-live-98765"),
                "invalid override CliError must not echo sensitive input: {cli_rendered}"
            );
        }

        // Non-UTF-8 LIBRA_STORAGE_BACKEND fails closed with LBR-CLI-003 rather
        // than silently falling back to default.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let non_utf8 = OsString::from_vec(vec![0xFF, 0xFE]);
            let err = StorageBackendKind::resolve_with_env_lookup(|key| {
                (key == LIBRA_STORAGE_BACKEND_ENV).then(|| non_utf8.clone())
            })
            .expect_err("non-utf8 env override must fail closed");
            assert!(
                matches!(err, StorageBackendError::UnsupportedBackend { .. }),
                "expected UnsupportedBackend for non-utf8, got {err:?}"
            );
            assert_eq!(err.stable_code_str(), "LBR-CLI-003");
        }
    }
}
