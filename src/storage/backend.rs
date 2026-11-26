//! Storage backend selection, compile-time feature dispatch, and pre-integration
//! runtime gate (`ADR-RB-01` #3).
//!
//! While [`REDB_RUNTIME_UNLOCKED`] is `false` (Track A through `RB-29`), any
//! runtime request selecting [`StorageBackendKind::Redb`] or calling
//! [`assert_redb_runtime_unlocked`] fails closed with `LBR-CLI-003`, ignoring
//! `LIBRA_TEST` and all other environment variables.

use std::{env, ffi::OsString, fmt, path::Path};

use serde::{Deserialize, Serialize};

use crate::storage::{
    error::{StorageBackendError, StorageResult},
    has_redb_backend, has_sqlite_backend,
    manifest::{StorageAuthority, detect_storage_authority},
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

    /// Resolve the active backend for a repository's `<common_storage>` directory
    /// with manifest-first precedence (`ADR-RB-01` #3), strictly inspecting
    /// `storage-format.json` and legacy `libra.db` BEFORE reading
    /// `LIBRA_STORAGE_BACKEND`:
    ///
    /// 1. `storage-format.json` with `finalized == false` → unconditionally fails
    ///    closed with [`StorageBackendError::ManifestNotFinalized`] (`LBR-IO-001`),
    ///    regardless of `LIBRA_STORAGE_BACKEND`.
    /// 2. `storage-format.json` with `finalized == true` → authoritative backend
    ///    is `manifest.backend`; conflicting `LIBRA_STORAGE_BACKEND` fails closed
    ///    with [`StorageBackendError::BackendMismatch`] (`LBR-CLI-003`).
    /// 3. Absent manifest + existing `libra.db` → authoritative backend is `Sqlite`;
    ///    `LIBRA_STORAGE_BACKEND=redb` fails closed with
    ///    [`StorageBackendError::BackendMismatch`] (`LBR-CLI-003`).
    /// 4. Fresh empty repository (no manifest and no `libra.db`, ignoring any
    ///    uncommitted `.libra/redb/` fragments) → allows `LIBRA_STORAGE_BACKEND`
    ///    to override the compiled default, still subject to compiled-feature and
    ///    [`REDB_RUNTIME_UNLOCKED`] checks.
    ///
    /// Invalid UTF-8 in `LIBRA_STORAGE_BACKEND` fails closed (`LBR-CLI-003`)
    /// rather than silently falling back to the default backend.
    pub fn resolve(common_storage: &Path) -> StorageResult<Self> {
        Self::resolve_with_env_lookup(common_storage, |key| env::var_os(key))
    }

    /// Resolve the active backend when no repository storage directory exists
    /// (for example, `libra init`, `libra clone`, or outside a repository).
    /// Evaluates `LIBRA_STORAGE_BACKEND` against compiled features and
    /// [`REDB_RUNTIME_UNLOCKED`] using [`StorageAuthority::FreshEmpty`].
    pub fn resolve_without_repo() -> StorageResult<Self> {
        let env_override = Self::read_env_override_from_lookup(|key| env::var_os(key))?;
        Self::resolve_authority_with_capabilities(
            &StorageAuthority::FreshEmpty,
            env_override.as_deref(),
            has_sqlite_backend(),
            has_redb_backend(),
        )
    }

    /// Resolve the active backend for `<common_storage>` using an injected
    /// environment lookup closure (reads only [`LIBRA_STORAGE_BACKEND_ENV`] and
    /// only AFTER verifying manifest/legacy on-disk authority; never inspects
    /// `LIBRA_TEST`).
    fn resolve_with_env_lookup<F>(common_storage: &Path, lookup: F) -> StorageResult<Self>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        // Inspect manifest and legacy `libra.db` FIRST so an unfinalized or
        // corrupt manifest fails closed (`LBR-IO-001`) before consulting env.
        let authority = detect_storage_authority(common_storage)?;
        let env_override = Self::read_env_override_from_lookup(lookup)?;
        Self::resolve_authority_with_capabilities(
            &authority,
            env_override.as_deref(),
            has_sqlite_backend(),
            has_redb_backend(),
        )
    }

    /// Test-only helper resolving `<common_storage>` from an explicit override
    /// string against this binary's compiled features and [`REDB_RUNTIME_UNLOCKED`].
    #[cfg(all(test, feature = "storage-sqlite"))]
    pub(crate) fn resolve_from_override(
        common_storage: &Path,
        env_override: Option<&str>,
    ) -> StorageResult<Self> {
        Self::resolve_with_capabilities(
            common_storage,
            env_override,
            has_sqlite_backend(),
            has_redb_backend(),
        )
    }

    /// Test-only helper exercising manifest-first resolution against explicit
    /// compiled-feature capability flags.
    #[cfg(test)]
    pub(crate) fn resolve_with_capabilities(
        common_storage: &Path,
        env_override: Option<&str>,
        sqlite_compiled: bool,
        redb_compiled: bool,
    ) -> StorageResult<Self> {
        let authority = detect_storage_authority(common_storage)?;
        Self::resolve_authority_with_capabilities(
            &authority,
            env_override,
            sqlite_compiled,
            redb_compiled,
        )
    }

    fn read_env_override_from_lookup<F>(lookup: F) -> StorageResult<Option<String>>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        match lookup(LIBRA_STORAGE_BACKEND_ENV) {
            None => Ok(None),
            Some(os_val) => {
                let text = os_val.into_string().map_err(|_| {
                    StorageBackendError::UnsupportedBackend {
                        requested: "<non-utf8>".to_string(),
                        detail: format!(
                            "`{LIBRA_STORAGE_BACKEND_ENV}` must be valid UTF-8 (`sqlite` or `redb`)"
                        ),
                    }
                })?;
                Ok(Some(text))
            }
        }
    }

    fn select_backend_for_authority(
        authority: &StorageAuthority,
        env_override: Option<&str>,
        sqlite_compiled: bool,
        redb_compiled: bool,
    ) -> StorageResult<Self> {
        let parsed_override = match env_override {
            Some(raw) => Some(Self::parse_env_value(raw)?),
            None => None,
        };

        match authority {
            StorageAuthority::Manifest(layout) => {
                let auth_kind = layout.backend;
                if let Some(requested) = parsed_override
                    && requested != auth_kind
                {
                    let migrate_cmd = match requested {
                        Self::Redb => "libra-migrate --to redb",
                        Self::Sqlite => "libra-migrate --to sqlite",
                    };
                    return Err(StorageBackendError::BackendMismatch {
                        detail: format!(
                            "repository `storage-format.json` declares authoritative backend `{auth_kind}`, which cannot be overridden by `{LIBRA_STORAGE_BACKEND_ENV}={requested}`; run `{migrate_cmd}` to migrate first"
                        ),
                    });
                }
                Ok(auth_kind)
            }
            StorageAuthority::LegacySqlite { .. } => {
                if let Some(requested) = parsed_override
                    && requested != Self::Sqlite
                {
                    return Err(StorageBackendError::BackendMismatch {
                        detail: format!(
                            "repository already uses legacy SQLite storage (`libra.db`), which cannot be overridden by `{LIBRA_STORAGE_BACKEND_ENV}={requested}`; run `libra-migrate --to redb` to migrate first"
                        ),
                    });
                }
                Ok(Self::Sqlite)
            }
            StorageAuthority::FreshEmpty => match parsed_override {
                Some(requested) => Ok(requested),
                None => Ok(Self::compiled_default_for(sqlite_compiled, redb_compiled)),
            },
        }
    }

    fn check_compiled_capabilities(
        selected: Self,
        sqlite_compiled: bool,
        redb_compiled: bool,
    ) -> StorageResult<()> {
        match selected {
            Self::Sqlite if !sqlite_compiled => Err(StorageBackendError::UnsupportedBackend {
                requested: "sqlite".to_string(),
                detail: "this Libra binary was compiled without `storage-sqlite`; rebuild with `--features storage-sqlite` or migrate the repository to `redb` with `libra-migrate --to redb`".to_string(),
            }),
            Self::Redb if !redb_compiled => Err(StorageBackendError::UnsupportedBackend {
                requested: "redb".to_string(),
                detail: "this Libra binary was compiled without `storage-redb`; rebuild with `--features storage-redb` or migrate back with `libra-migrate --to sqlite`".to_string(),
            }),
            _ => Ok(()),
        }
    }

    fn resolve_authority_with_capabilities(
        authority: &StorageAuthority,
        env_override: Option<&str>,
        sqlite_compiled: bool,
        redb_compiled: bool,
    ) -> StorageResult<Self> {
        let selected = Self::select_backend_for_authority(
            authority,
            env_override,
            sqlite_compiled,
            redb_compiled,
        )?;
        Self::check_compiled_capabilities(selected, sqlite_compiled, redb_compiled)?;
        if selected == Self::Redb {
            assert_redb_runtime_unlocked()?;
        }
        Ok(selected)
    }

    #[cfg(all(test, feature = "storage-sqlite", feature = "storage-redb"))]
    fn resolve_post_unlock_for_test(
        common_storage: &Path,
        env_override: Option<&str>,
        sqlite_compiled: bool,
        redb_compiled: bool,
    ) -> StorageResult<Self> {
        let authority = detect_storage_authority(common_storage)?;
        let selected = Self::select_backend_for_authority(
            &authority,
            env_override,
            sqlite_compiled,
            redb_compiled,
        )?;
        Self::check_compiled_capabilities(selected, sqlite_compiled, redb_compiled)?;
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
    use std::{
        ffi::OsString,
        fs,
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use tempfile::tempdir;

    use super::*;
    use crate::storage::manifest::{
        INITIAL_STORAGE_GENERATION, PRIVATE_SUBDIR, Phase2DbState, Phase2Progress, REDB_SUBDIR,
        SourceDigests, StorageFormatManifest, publish_storage_manifest,
    };

    #[test]
    fn dual_feature_resolution_and_pre_release_cli_gate() {
        // Isolated child-process mode exercises the real public `StorageBackendKind::resolve(dir)`
        // entry point directly against live process env vars (`LIBRA_STORAGE_BACKEND=redb`, `LIBRA_TEST=1`).
        if let Some(mode) = env::var_os("LIBRA_RB01C_CHILD_MODE") {
            let storage_dir = PathBuf::from(
                env::var_os("LIBRA_RB01C_STORAGE_DIR")
                    .expect("LIBRA_RB01C_STORAGE_DIR must be set for child mode"),
            );
            match mode.to_str() {
                Some("locked_redb") => {
                    let err = StorageBackendKind::resolve(&storage_dir).expect_err(
                        "public resolve() must fail closed for redb even with LIBRA_TEST=1",
                    );
                    assert!(
                        matches!(err, StorageBackendError::PreReleaseGateLocked { .. }),
                        "expected PreReleaseGateLocked from resolve(), got {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-CLI-003");
                    println!("RB01C_CHILD_OK:locked_redb");
                    return;
                }
                Some("default_unset") => {
                    let kind = StorageBackendKind::resolve(&storage_dir).expect(
                        "public resolve() with unset env must select sqlite before cutover",
                    );
                    assert_eq!(kind, StorageBackendKind::Sqlite);
                    println!("RB01C_CHILD_OK:default_unset");
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

        let fresh = tempdir().expect("create fresh storage dir");

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
        let locked_output = Command::new(&exe)
            .arg("--exact")
            .arg("storage::backend::tests::dual_feature_resolution_and_pre_release_cli_gate")
            .arg("--nocapture")
            .env("LIBRA_RB01C_CHILD_MODE", "locked_redb")
            .env("LIBRA_RB01C_STORAGE_DIR", fresh.path())
            .env(LIBRA_STORAGE_BACKEND_ENV, "redb")
            .env("LIBRA_TEST", "1")
            .output()
            .expect("spawn isolated locked_redb child");
        let locked_stdout = String::from_utf8_lossy(&locked_output.stdout);
        let locked_stderr = String::from_utf8_lossy(&locked_output.stderr);
        assert!(
            locked_output.status.success()
                && locked_stdout.contains("RB01C_CHILD_OK:locked_redb")
                && locked_stdout.contains("1 passed; 0 failed"),
            "isolated locked_redb child did not execute expected test (status={:?}):\nstdout:\n{locked_stdout}\nstderr:\n{locked_stderr}",
            locked_output.status
        );

        let default_output = Command::new(&exe)
            .arg("--exact")
            .arg("storage::backend::tests::dual_feature_resolution_and_pre_release_cli_gate")
            .arg("--nocapture")
            .env("LIBRA_RB01C_CHILD_MODE", "default_unset")
            .env("LIBRA_RB01C_STORAGE_DIR", fresh.path())
            .env_remove(LIBRA_STORAGE_BACKEND_ENV)
            .output()
            .expect("spawn isolated default_unset child");
        let default_stdout = String::from_utf8_lossy(&default_output.stdout);
        let default_stderr = String::from_utf8_lossy(&default_output.stderr);
        assert!(
            default_output.status.success()
                && default_stdout.contains("RB01C_CHILD_OK:default_unset")
                && default_stdout.contains("1 passed; 0 failed"),
            "isolated default_unset child did not execute expected test (status={:?}):\nstdout:\n{default_stdout}\nstderr:\n{default_stderr}",
            default_output.status
        );

        // Explicit sqlite override succeeds in dual-feature builds.
        assert_eq!(
            StorageBackendKind::resolve_with_env_lookup(fresh.path(), |key| match key {
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
            let err = StorageBackendKind::resolve_from_override(fresh.path(), Some(bad))
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
            let err = StorageBackendKind::resolve_with_env_lookup(fresh.path(), |key| {
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

    fn forward_in_progress_manifest() -> StorageFormatManifest {
        StorageFormatManifest::new_redb_in_progress(
            INITIAL_STORAGE_GENERATION,
            Some("sqlite-backup/00000001".to_string()),
            Phase2Progress {
                git_db: Some(Phase2DbState::Pending),
                capture_db: Some(Phase2DbState::Absent),
                git_redb: None,
                cap_redb: None,
                spool_fenced: false,
                queue_fenced: false,
            },
            SourceDigests {
                git_logical_sha256: Some("a".repeat(64)),
                git_backup_sha256: Some("b".repeat(64)),
                spool_manifest_sha256: Some("c".repeat(64)),
                queue_manifest_sha256: Some("d".repeat(64)),
                ..SourceDigests::default()
            },
        )
        .expect("construct valid forward in-progress manifest")
    }

    fn reverse_in_progress_manifest() -> StorageFormatManifest {
        StorageFormatManifest::new_sqlite_in_progress(
            INITIAL_STORAGE_GENERATION + 1,
            0x5242_0001,
            Some("redb-backup/00000002".to_string()),
            Phase2Progress {
                git_db: None,
                capture_db: None,
                git_redb: Some(Phase2DbState::Pending),
                cap_redb: Some(Phase2DbState::Pending),
                spool_fenced: true,
                queue_fenced: true,
            },
            SourceDigests {
                git_redb_sha256: Some("1".repeat(64)),
                cap_redb_sha256: Some("2".repeat(64)),
                spool_manifest_sha256: Some("3".repeat(64)),
                queue_manifest_sha256: Some("4".repeat(64)),
                ..SourceDigests::default()
            },
        )
        .expect("construct valid reverse in-progress manifest")
    }

    fn finalized_sqlite_manifest() -> StorageFormatManifest {
        let mut manifest = StorageFormatManifest::new_sqlite_in_progress(
            INITIAL_STORAGE_GENERATION + 1,
            0x5242_0001,
            Some("redb-backup/00000002".to_string()),
            Phase2Progress {
                git_db: None,
                capture_db: None,
                git_redb: Some(Phase2DbState::Unlinked),
                cap_redb: Some(Phase2DbState::Unlinked),
                spool_fenced: true,
                queue_fenced: true,
            },
            SourceDigests {
                git_redb_sha256: Some("1".repeat(64)),
                cap_redb_sha256: Some("2".repeat(64)),
                spool_manifest_sha256: Some("3".repeat(64)),
                queue_manifest_sha256: Some("4".repeat(64)),
                ..SourceDigests::default()
            },
        )
        .expect("construct staged reverse sqlite manifest");
        manifest.targets_staged_for_rename = true;
        manifest
            .into_finalized()
            .expect("finalize staged reverse sqlite manifest")
    }

    fn assert_error_does_not_leak_secret(err: &StorageBackendError, secret_marker: &str) {
        let display = err.to_string();
        assert!(
            !display.contains(secret_marker),
            "error Display must not leak private marker `{secret_marker}`: {display}"
        );
    }

    #[test]
    fn manifest_first_precedence_blocks_conflicting_env_overrides() {
        const SECRET_MARKER: &str = "private-tenant-secret-repo-7741";
        const SECRET_TOKEN: &str = "sk-live-secret-998877";

        // Isolated child-process mode exercises the public `StorageBackendKind::resolve()`
        // entry point directly against live process env vars (`LIBRA_STORAGE_BACKEND`, `LIBRA_TEST=1`).
        if let Some(mode) = env::var_os("LIBRA_RB02E_CHILD_MODE") {
            let storage_dir = PathBuf::from(
                env::var_os("LIBRA_RB02E_STORAGE_DIR")
                    .expect("LIBRA_RB02E_STORAGE_DIR must be set for child mode"),
            );
            let mode_str = mode.to_str().expect("LIBRA_RB02E_CHILD_MODE must be UTF-8");
            match mode_str {
                "unfinalized_blocks_env" => {
                    let err = StorageBackendKind::resolve(&storage_dir)
                        .expect_err("finalized=false manifest must fail closed regardless of env");
                    assert!(
                        matches!(err, StorageBackendError::ManifestNotFinalized { .. }),
                        "expected ManifestNotFinalized, got {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-IO-001");
                    assert_error_does_not_leak_secret(&err, SECRET_MARKER);
                    assert_error_does_not_leak_secret(&err, SECRET_TOKEN);
                    let rendered = err.into_cli_error().render();
                    assert!(!rendered.contains(SECRET_MARKER) && !rendered.contains(SECRET_TOKEN));
                }
                "finalized_conflict" => {
                    let err = StorageBackendKind::resolve(&storage_dir).expect_err(
                        "conflicting LIBRA_STORAGE_BACKEND against finalized manifest must fail closed",
                    );
                    assert!(
                        matches!(err, StorageBackendError::BackendMismatch { .. }),
                        "expected BackendMismatch (not PreReleaseGateLocked), got {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-CLI-003");
                    assert_error_does_not_leak_secret(&err, SECRET_MARKER);
                }
                "invalid_env_rejected" => {
                    let err = StorageBackendKind::resolve(&storage_dir)
                        .expect_err("invalid or non-utf8 LIBRA_STORAGE_BACKEND must fail closed");
                    assert!(
                        matches!(err, StorageBackendError::UnsupportedBackend { .. }),
                        "expected UnsupportedBackend for invalid/non-utf8 env, got {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-CLI-003");
                    assert_error_does_not_leak_secret(&err, SECRET_MARKER);
                    assert_error_does_not_leak_secret(&err, SECRET_TOKEN);
                    let rendered = err.into_cli_error().render();
                    assert!(!rendered.contains(SECRET_MARKER) && !rendered.contains(SECRET_TOKEN));
                }
                "legacy_db_conflict" => {
                    let err = StorageBackendKind::resolve(&storage_dir).expect_err(
                        "LIBRA_STORAGE_BACKEND=redb against legacy libra.db must fail closed",
                    );
                    assert!(
                        matches!(err, StorageBackendError::BackendMismatch { .. }),
                        "expected BackendMismatch for legacy libra.db, got {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-CLI-003");
                    assert_error_does_not_leak_secret(&err, SECRET_MARKER);
                }
                "fresh_env_sqlite" => {
                    let kind = StorageBackendKind::resolve(&storage_dir)
                        .expect("fresh empty repo with LIBRA_STORAGE_BACKEND=sqlite must succeed");
                    assert_eq!(kind, StorageBackendKind::Sqlite);
                }
                "fresh_env_redb_hits_gate" => {
                    let err = StorageBackendKind::resolve(&storage_dir).expect_err(
                        "fresh empty repo with LIBRA_STORAGE_BACKEND=redb must pass authority and hit pre-release gate",
                    );
                    assert!(
                        matches!(err, StorageBackendError::PreReleaseGateLocked { .. }),
                        "expected PreReleaseGateLocked (proving override was accepted by authority layer), got {err:?}"
                    );
                    assert_eq!(err.stable_code_str(), "LBR-CLI-003");
                }
                other => panic!("unexpected LIBRA_RB02E_CHILD_MODE: {other:?}"),
            }
            println!("RB02E_CHILD_OK:{mode_str}");
            return;
        }

        // Confirm dual-feature build (proof that failures are manifest-first
        // precedence, not missing compiled features).
        assert!(has_sqlite_backend() && has_redb_backend());

        let root = tempdir().expect("create temp workspace");
        let secret_base = root.path().join(SECRET_MARKER);
        fs::create_dir_all(&secret_base).expect("create secret base dir");

        let exe = env::current_exe().expect("locate current test executable");
        let run_child_os = |mode: &str, dir: &Path, env_val: Option<OsString>| {
            let mut cmd = Command::new(&exe);
            cmd.arg("--exact")
                .arg("storage::backend::tests::manifest_first_precedence_blocks_conflicting_env_overrides")
                .arg("--nocapture")
                .env("LIBRA_RB02E_CHILD_MODE", mode)
                .env("LIBRA_RB02E_STORAGE_DIR", dir)
                .env("LIBRA_TEST", "1");
            match &env_val {
                Some(v) => {
                    cmd.env(LIBRA_STORAGE_BACKEND_ENV, v);
                }
                None => {
                    cmd.env_remove(LIBRA_STORAGE_BACKEND_ENV);
                }
            }
            let output = cmd.output().expect("spawn RB-02e child process");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let sentinel = format!("RB02E_CHILD_OK:{mode}");
            assert!(
                output.status.success()
                    && stdout.contains(&sentinel)
                    && stdout.contains("1 passed; 0 failed"),
                "RB-02e child mode `{mode}` (env={env_val:?}) did not execute expected test (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
        };
        let run_child = |mode: &str, dir: &Path, env_val: Option<&str>| {
            run_child_os(mode, dir, env_val.map(OsString::from));
        };

        let sensitive_invalid_env = format!("/Users/{SECRET_MARKER}/token={SECRET_TOKEN}");

        // -----------------------------------------------------------------
        // AC1 & AC2: `finalized == false` (both forward `redb` and reverse
        // `sqlite` in-progress manifests) MUST fail closed with LBR-IO-001
        // BEFORE reading `LIBRA_STORAGE_BACKEND` (whether redb, sqlite,
        // unset, invalid, or non-UTF-8).
        // -----------------------------------------------------------------
        let unfinalized_forward_dir = secret_base.join("unfinalized-forward");
        fs::create_dir_all(&unfinalized_forward_dir).expect("create unfinalized forward dir");
        publish_storage_manifest(&unfinalized_forward_dir, &forward_in_progress_manifest())
            .expect("publish forward finalized=false manifest");
        // Even if legacy `libra.db` still exists alongside the in-progress manifest,
        // the manifest takes strict precedence.
        fs::write(unfinalized_forward_dir.join("libra.db"), b"legacy")
            .expect("write coexisting legacy libra.db");

        let unfinalized_reverse_dir = secret_base.join("unfinalized-reverse");
        fs::create_dir_all(&unfinalized_reverse_dir).expect("create unfinalized reverse dir");
        publish_storage_manifest(&unfinalized_reverse_dir, &reverse_in_progress_manifest())
            .expect("publish reverse finalized=false manifest");

        for dir in [&unfinalized_forward_dir, &unfinalized_reverse_dir] {
            // Process-level public `resolve()` verification for AC1 (`redb`), AC2 (`sqlite`),
            // unset, invalid secret-bearing value, and real non-UTF-8 bytes.
            run_child("unfinalized_blocks_env", dir, Some("redb"));
            run_child("unfinalized_blocks_env", dir, Some("sqlite"));
            run_child("unfinalized_blocks_env", dir, None);
            run_child(
                "unfinalized_blocks_env",
                dir,
                Some(sensitive_invalid_env.as_str()),
            );
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                run_child_os(
                    "unfinalized_blocks_env",
                    dir,
                    Some(OsString::from_vec(vec![0xFF, 0xFE])),
                );
            }

            // Prove env lookup is NEVER even invoked when finalized=false!
            for raw_override in [Some("redb"), Some("sqlite"), Some("invalid-backend"), None] {
                let lookup_calls = AtomicUsize::new(0);
                let err = StorageBackendKind::resolve_with_env_lookup(dir, |_| {
                    lookup_calls.fetch_add(1, Ordering::SeqCst);
                    raw_override.map(OsString::from)
                })
                .expect_err("finalized=false must fail before env lookup");
                assert!(
                    matches!(err, StorageBackendError::ManifestNotFinalized { .. }),
                    "expected ManifestNotFinalized for override {raw_override:?}, got {err:?}"
                );
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
                assert_eq!(
                    lookup_calls.load(Ordering::SeqCst),
                    0,
                    "manifest must be checked BEFORE consulting LIBRA_STORAGE_BACKEND"
                );
                assert_error_does_not_leak_secret(&err, SECRET_MARKER);
            }

            // Even after unlock (post-RB-29a), finalized=false still fails closed.
            for raw_override in [Some("redb"), Some("sqlite"), None] {
                let err =
                    StorageBackendKind::resolve_post_unlock_for_test(dir, raw_override, true, true)
                        .expect_err("finalized=false must still fail closed after unlock");
                assert!(matches!(
                    err,
                    StorageBackendError::ManifestNotFinalized { .. }
                ));
                assert_eq!(err.stable_code_str(), "LBR-IO-001");
            }
        }

        // -----------------------------------------------------------------
        // AC3: `finalized == true` manifest establishes authoritative backend;
        // conflicting `LIBRA_STORAGE_BACKEND` fails closed with `LBR-CLI-003`
        // (`BackendMismatch`), NOT `PreReleaseGateLocked`.
        // -----------------------------------------------------------------
        let finalized_redb_dir = secret_base.join("finalized-redb");
        fs::create_dir_all(&finalized_redb_dir).expect("create finalized redb dir");
        let redb_manifest = StorageFormatManifest::new_redb_finalized(INITIAL_STORAGE_GENERATION)
            .expect("construct finalized redb manifest");
        publish_storage_manifest(&finalized_redb_dir, &redb_manifest)
            .expect("publish finalized redb manifest");

        let finalized_sqlite_dir = secret_base.join("finalized-sqlite");
        fs::create_dir_all(&finalized_sqlite_dir).expect("create finalized sqlite dir");
        publish_storage_manifest(&finalized_sqlite_dir, &finalized_sqlite_manifest())
            .expect("publish finalized sqlite manifest");

        // Process-level AC3 checks in both conflict directions, plus invalid and non-UTF-8 env.
        run_child("finalized_conflict", &finalized_redb_dir, Some("sqlite"));
        run_child("finalized_conflict", &finalized_sqlite_dir, Some("redb"));
        run_child(
            "invalid_env_rejected",
            &finalized_redb_dir,
            Some(sensitive_invalid_env.as_str()),
        );
        run_child(
            "invalid_env_rejected",
            &finalized_sqlite_dir,
            Some(sensitive_invalid_env.as_str()),
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            run_child_os(
                "invalid_env_rejected",
                &finalized_redb_dir,
                Some(OsString::from_vec(vec![0xFF, 0xFE])),
            );
            run_child_os(
                "invalid_env_rejected",
                &finalized_sqlite_dir,
                Some(OsString::from_vec(vec![0xFF, 0xFE])),
            );
        }

        // Direct assertions for finalized redb + conflicting sqlite override.
        let redb_conflict =
            StorageBackendKind::resolve_from_override(&finalized_redb_dir, Some("sqlite"))
                .expect_err("sqlite override must fail against finalized redb manifest");
        assert!(matches!(
            redb_conflict,
            StorageBackendError::BackendMismatch { .. }
        ));
        assert_eq!(redb_conflict.stable_code_str(), "LBR-CLI-003");
        assert!(
            redb_conflict
                .to_string()
                .contains("libra-migrate --to sqlite")
        );
        assert_error_does_not_leak_secret(&redb_conflict, SECRET_MARKER);

        // Matching redb override or unset on finalized redb passes authority check and hits
        // `PreReleaseGateLocked` while locked, and succeeds (`Redb`) once unlocked.
        for matching in [None, Some("redb"), Some("REDB")] {
            let locked = StorageBackendKind::resolve_from_override(&finalized_redb_dir, matching)
                .expect_err("finalized redb must hit pre-release gate while locked");
            assert!(
                matches!(locked, StorageBackendError::PreReleaseGateLocked { .. }),
                "expected PreReleaseGateLocked for matching {matching:?}, got {locked:?}"
            );
            assert_eq!(locked.stable_code_str(), "LBR-CLI-003");

            let unlocked = StorageBackendKind::resolve_post_unlock_for_test(
                &finalized_redb_dir,
                matching,
                true,
                true,
            )
            .expect("finalized redb must resolve to Redb once unlocked");
            assert_eq!(unlocked, StorageBackendKind::Redb);
        }

        // Direct assertions for finalized sqlite + conflicting redb override.
        let sqlite_conflict =
            StorageBackendKind::resolve_from_override(&finalized_sqlite_dir, Some("redb"))
                .expect_err("redb override must fail against finalized sqlite manifest");
        assert!(matches!(
            sqlite_conflict,
            StorageBackendError::BackendMismatch { .. }
        ));
        assert_eq!(sqlite_conflict.stable_code_str(), "LBR-CLI-003");
        assert!(
            sqlite_conflict
                .to_string()
                .contains("libra-migrate --to redb")
        );
        assert_error_does_not_leak_secret(&sqlite_conflict, SECRET_MARKER);

        // Matching sqlite override or unset on finalized sqlite succeeds.
        for matching in [None, Some("sqlite"), Some("SQLITE")] {
            assert_eq!(
                StorageBackendKind::resolve_from_override(&finalized_sqlite_dir, matching)
                    .expect("matching sqlite override on finalized sqlite must succeed"),
                StorageBackendKind::Sqlite
            );
        }

        // Even post-unlock, conflicting override against finalized manifest still fails closed.
        let unlocked_conflict = StorageBackendKind::resolve_post_unlock_for_test(
            &finalized_sqlite_dir,
            Some("redb"),
            true,
            true,
        )
        .expect_err("post-unlock must still reject redb override on finalized sqlite");
        assert!(matches!(
            unlocked_conflict,
            StorageBackendError::BackendMismatch { .. }
        ));
        assert_eq!(unlocked_conflict.stable_code_str(), "LBR-CLI-003");

        // -----------------------------------------------------------------
        // AC4: Absent manifest + existing `.libra/libra.db` establishes
        // `Sqlite` authority; `LIBRA_STORAGE_BACKEND=redb` fails closed with
        // `LBR-CLI-003` (`BackendMismatch`).
        // -----------------------------------------------------------------
        let legacy_dir = secret_base.join("legacy-sqlite-repo");
        fs::create_dir_all(&legacy_dir).expect("create legacy repo dir");
        fs::write(legacy_dir.join("libra.db"), b"SQLite format 3\0")
            .expect("create legacy libra.db");

        run_child("legacy_db_conflict", &legacy_dir, Some("redb"));

        let legacy_err = StorageBackendKind::resolve_from_override(&legacy_dir, Some("redb"))
            .expect_err("redb override must fail against legacy libra.db");
        assert!(matches!(
            legacy_err,
            StorageBackendError::BackendMismatch { .. }
        ));
        assert_eq!(legacy_err.stable_code_str(), "LBR-CLI-003");
        assert!(legacy_err.to_string().contains("libra-migrate --to redb"));
        assert_error_does_not_leak_secret(&legacy_err, SECRET_MARKER);

        // Post-unlock must ALSO reject `redb` override against legacy `libra.db`.
        let legacy_unlocked_err =
            StorageBackendKind::resolve_post_unlock_for_test(&legacy_dir, Some("redb"), true, true)
                .expect_err("post-unlock must still reject redb override on legacy libra.db");
        assert!(matches!(
            legacy_unlocked_err,
            StorageBackendError::BackendMismatch { .. }
        ));
        assert_eq!(legacy_unlocked_err.stable_code_str(), "LBR-CLI-003");

        // Unset or `sqlite` override resolves cleanly to `Sqlite`.
        for matching in [None, Some("sqlite")] {
            assert_eq!(
                StorageBackendKind::resolve_from_override(&legacy_dir, matching)
                    .expect("legacy libra.db must resolve to Sqlite"),
                StorageBackendKind::Sqlite
            );
        }

        // Single-feature capability failures on authoritative repos fail closed with LBR-CLI-003.
        let redb_only_on_legacy =
            StorageBackendKind::resolve_with_capabilities(&legacy_dir, None, false, true)
                .expect_err("redb-only build must reject legacy libra.db");
        assert!(matches!(
            redb_only_on_legacy,
            StorageBackendError::UnsupportedBackend { .. }
        ));
        assert_eq!(redb_only_on_legacy.stable_code_str(), "LBR-CLI-003");

        let sqlite_only_on_redb =
            StorageBackendKind::resolve_with_capabilities(&finalized_redb_dir, None, true, false)
                .expect_err("sqlite-only build must reject finalized redb manifest");
        assert!(matches!(
            sqlite_only_on_redb,
            StorageBackendError::UnsupportedBackend { .. }
        ));
        assert_eq!(sqlite_only_on_redb.stable_code_str(), "LBR-CLI-003");

        // -----------------------------------------------------------------
        // AC5: Only fresh empty repositories (no manifest and no `libra.db`,
        // including non-existent `.libra/` and dirs containing uncommitted
        // `.libra/redb/` fragments) allow `LIBRA_STORAGE_BACKEND` to override
        // the compiled default (still enforcing compiled-feature and
        // `REDB_RUNTIME_UNLOCKED` gates).
        // -----------------------------------------------------------------
        let nonexistent_fresh_dir = secret_base.join("brand-new-not-yet-created");
        let empty_fresh_dir = secret_base.join("brand-new-empty");
        fs::create_dir_all(&empty_fresh_dir).expect("create empty fresh dir");

        let fragmented_fresh_dir = secret_base.join("brand-new-with-fragments");
        fs::create_dir_all(fragmented_fresh_dir.join(REDB_SUBDIR)).expect("create redb subdir");
        fs::create_dir_all(fragmented_fresh_dir.join(PRIVATE_SUBDIR))
            .expect("create private subdir");
        fs::write(
            fragmented_fresh_dir
                .join(REDB_SUBDIR)
                .join("git.gen-00000001.redb"),
            b"uncommitted-fragment",
        )
        .expect("write uncommitted git.redb fragment");
        fs::write(
            fragmented_fresh_dir
                .join(REDB_SUBDIR)
                .join("git.gen-00000001.redb.tmp"),
            b"uncommitted-tmp",
        )
        .expect("write uncommitted tmp fragment");

        for fresh_dir in [
            &nonexistent_fresh_dir,
            &empty_fresh_dir,
            &fragmented_fresh_dir,
        ] {
            // Unset env uses compiled default (`Sqlite` prior to RB-33).
            assert_eq!(
                StorageBackendKind::resolve_from_override(fresh_dir, None)
                    .expect("fresh empty repo with unset env must select compiled default"),
                StorageBackendKind::Sqlite
            );

            // Explicit `sqlite` override succeeds via live public `resolve()`.
            run_child("fresh_env_sqlite", fresh_dir, Some("sqlite"));

            // Explicit `redb` override is permitted by authority layer (NOT `BackendMismatch`),
            // and is governed by the compile-time `REDB_RUNTIME_UNLOCKED` gate while locked...
            run_child("fresh_env_redb_hits_gate", fresh_dir, Some("redb"));

            // Invalid and non-UTF-8 env values on fresh repos fail closed with LBR-CLI-003.
            run_child(
                "invalid_env_rejected",
                fresh_dir,
                Some(sensitive_invalid_env.as_str()),
            );
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                run_child_os(
                    "invalid_env_rejected",
                    fresh_dir,
                    Some(OsString::from_vec(vec![0xFF, 0xFE])),
                );
            }

            // ...and resolves to `Ok(StorageBackendKind::Redb)` once unlocked!
            assert_eq!(
                StorageBackendKind::resolve_post_unlock_for_test(
                    fresh_dir,
                    Some("redb"),
                    true,
                    true,
                )
                .expect("fresh empty repo must allow LIBRA_STORAGE_BACKEND=redb once unlocked"),
                StorageBackendKind::Redb
            );

            // Still enforces compiled feature check (sqlite-only build cannot override to redb).
            let missing_feature =
                StorageBackendKind::resolve_with_capabilities(fresh_dir, Some("redb"), true, false)
                    .expect_err(
                        "sqlite-only build must reject redb override even on fresh empty repo",
                    );
            assert!(matches!(
                missing_feature,
                StorageBackendError::UnsupportedBackend { .. }
            ));
            assert_eq!(missing_feature.stable_code_str(), "LBR-CLI-003");
        }

        // -----------------------------------------------------------------
        // Corrupt manifest sanitization regression (`GC-RB-03` / `ER-11`):
        // malformed enum values or corrupt JSON containing private paths or
        // tokens must NEVER echo the raw secret into Display or CliError.
        // -----------------------------------------------------------------
        let corrupt_enum_dir = secret_base.join("corrupt-enum-manifest");
        fs::create_dir_all(&corrupt_enum_dir).expect("create corrupt enum dir");
        let corrupt_enum_json = format!(
            r#"{{"schema_version":1,"origin":"fresh_init","backend":"/Users/{SECRET_MARKER}/{SECRET_TOKEN}","finalized":true,"generation":1,"phase2_progress":{{"spool_fenced":false,"queue_fenced":false}},"targets_staged_for_rename":false,"spool_unfenced":false,"source_digests":{{}}}}"#
        );
        fs::write(
            corrupt_enum_dir.join("storage-format.json"),
            corrupt_enum_json,
        )
        .expect("write corrupt enum manifest");
        let enum_err = StorageBackendKind::resolve_from_override(&corrupt_enum_dir, None)
            .expect_err("corrupt manifest backend enum must fail closed");
        assert!(matches!(
            enum_err,
            StorageBackendError::BackendInternal { .. }
        ));
        assert_eq!(enum_err.stable_code_str(), "LBR-IO-001");
        assert_error_does_not_leak_secret(&enum_err, SECRET_MARKER);
        assert_error_does_not_leak_secret(&enum_err, SECRET_TOKEN);
        let enum_cli = enum_err.into_cli_error().render();
        assert!(!enum_cli.contains(SECRET_MARKER) && !enum_cli.contains(SECRET_TOKEN));

        let corrupt_syntax_dir = secret_base.join("corrupt-syntax-manifest");
        fs::create_dir_all(&corrupt_syntax_dir).expect("create corrupt syntax dir");
        fs::write(
            corrupt_syntax_dir.join("storage-format.json"),
            format!("{{{SECRET_MARKER}:{SECRET_TOKEN}"),
        )
        .expect("write corrupt syntax manifest");
        let syntax_err = StorageBackendKind::resolve_from_override(&corrupt_syntax_dir, None)
            .expect_err("corrupt JSON syntax manifest must fail closed");
        assert!(matches!(
            syntax_err,
            StorageBackendError::BackendInternal { .. }
        ));
        assert_eq!(syntax_err.stable_code_str(), "LBR-IO-001");
        assert_error_does_not_leak_secret(&syntax_err, SECRET_MARKER);
        assert_error_does_not_leak_secret(&syntax_err, SECRET_TOKEN);
        let syntax_cli = syntax_err.into_cli_error().render();
        assert!(!syntax_cli.contains(SECRET_MARKER) && !syntax_cli.contains(SECRET_TOKEN));

        // -----------------------------------------------------------------
        // Filesystem boundary & symlink fail-closed matrix (`LBR-IO-001`).
        // -----------------------------------------------------------------
        let regular_file_root = secret_base.join("not-a-directory");
        fs::write(&regular_file_root, b"not a dir").expect("write regular file");
        let root_err = StorageBackendKind::resolve_from_override(&regular_file_root, None)
            .expect_err("regular file storage root must fail closed");
        assert!(matches!(
            root_err,
            StorageBackendError::BackendInternal { .. }
        ));
        assert_eq!(root_err.stable_code_str(), "LBR-IO-001");
        assert_error_does_not_leak_secret(&root_err, SECRET_MARKER);

        let bad_legacy_dir = secret_base.join("legacy-dir-is-subdir");
        fs::create_dir_all(bad_legacy_dir.join("libra.db"))
            .expect("create directory named libra.db");
        let bad_legacy_err = StorageBackendKind::resolve_from_override(&bad_legacy_dir, None)
            .expect_err("directory named libra.db must fail closed");
        assert!(matches!(
            bad_legacy_err,
            StorageBackendError::BackendInternal { .. }
        ));
        assert_eq!(bad_legacy_err.stable_code_str(), "LBR-IO-001");
        assert_error_does_not_leak_secret(&bad_legacy_err, SECRET_MARKER);

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            // Valid storage-root symlink pointing to a real directory enforces authority.
            let valid_dir_link = secret_base.join("symlink-to-legacy-dir");
            symlink(&legacy_dir, &valid_dir_link).expect("create symlink to legacy dir");
            assert_eq!(
                StorageBackendKind::resolve_from_override(&valid_dir_link, None)
                    .expect("valid symlink to legacy dir must resolve to Sqlite"),
                StorageBackendKind::Sqlite
            );
            let sym_conflict =
                StorageBackendKind::resolve_from_override(&valid_dir_link, Some("redb"))
                    .expect_err(
                        "valid symlink to legacy dir must reject conflicting redb override",
                    );
            assert!(matches!(
                sym_conflict,
                StorageBackendError::BackendMismatch { .. }
            ));
            assert_eq!(sym_conflict.stable_code_str(), "LBR-CLI-003");

            // Storage-root symlink pointing to a regular file fails closed (`BackendInternal`).
            let file_target_link = secret_base.join("symlink-to-regular-file");
            symlink(&regular_file_root, &file_target_link).expect("create symlink to regular file");
            let file_link_err = StorageBackendKind::resolve_from_override(&file_target_link, None)
                .expect_err("storage root symlink to regular file must fail closed");
            assert!(matches!(
                file_link_err,
                StorageBackendError::BackendInternal { .. }
            ));
            assert_eq!(file_link_err.stable_code_str(), "LBR-IO-001");
            assert_error_does_not_leak_secret(&file_link_err, SECRET_MARKER);

            // Dangling storage-root symlink fails closed (`Io`, NOT `FreshEmpty`).
            let dangling_root_link = secret_base.join("dangling-storage-root-link");
            symlink(secret_base.join("nonexistent-target"), &dangling_root_link)
                .expect("create dangling root symlink");
            let dangling_root_err =
                StorageBackendKind::resolve_from_override(&dangling_root_link, None)
                    .expect_err("dangling storage root symlink must fail closed, not FreshEmpty");
            assert!(matches!(dangling_root_err, StorageBackendError::Io { .. }));
            assert_eq!(dangling_root_err.stable_code_str(), "LBR-IO-001");
            assert_error_does_not_leak_secret(&dangling_root_err, SECRET_MARKER);

            // `libra.db` as a symlink to a real file fails closed (`BackendInternal`).
            let symlink_db_dir = secret_base.join("symlink-libra-db-valid-target");
            fs::create_dir_all(&symlink_db_dir).expect("create symlink_db_dir");
            symlink(legacy_dir.join("libra.db"), symlink_db_dir.join("libra.db"))
                .expect("create symlinked libra.db");
            let sym_db_err = StorageBackendKind::resolve_from_override(&symlink_db_dir, None)
                .expect_err("symlinked libra.db pointing to real file must fail closed");
            assert!(matches!(
                sym_db_err,
                StorageBackendError::BackendInternal { .. }
            ));
            assert_eq!(sym_db_err.stable_code_str(), "LBR-IO-001");
            assert_error_does_not_leak_secret(&sym_db_err, SECRET_MARKER);

            // `libra.db` as a dangling symlink fails closed (`BackendInternal`).
            let dangling_db_dir = secret_base.join("symlink-libra-db-dangling");
            fs::create_dir_all(&dangling_db_dir).expect("create dangling_db_dir");
            symlink(
                secret_base.join("missing-db-target"),
                dangling_db_dir.join("libra.db"),
            )
            .expect("create dangling symlink libra.db");
            let dangling_db_err = StorageBackendKind::resolve_from_override(&dangling_db_dir, None)
                .expect_err("dangling symlink libra.db must fail closed");
            assert!(matches!(
                dangling_db_err,
                StorageBackendError::BackendInternal { .. }
            ));
            assert_eq!(dangling_db_err.stable_code_str(), "LBR-IO-001");
            assert_error_does_not_leak_secret(&dangling_db_err, SECRET_MARKER);
        }
    }
}
