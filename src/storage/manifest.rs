//! Two-phase atomic repository storage manifest (`.libra/storage-format.json`)
//! and generation-scoped database path resolution (`ADR-RB-02` #1, `ADR-RB-07`).
//!
//! All repository backend switches and fresh `redb` initializations publish
//! `.libra/storage-format.json` atomically via `.tmp` → `fsync` → `rename` →
//! `fsync_dir`. Readers strictly consult the manifest alone and never scan
//! `.libra/redb/` for uncommitted `.redb` or `.tmp` fragments when the manifest
//! is absent.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use crate::{
    internal::db::schema::DatabaseRole,
    storage::{
        backend::StorageBackendKind,
        error::{StorageBackendError, StorageResult},
    },
};

/// Canonical filename for the repository storage format manifest under `.libra/`.
pub const STORAGE_FORMAT_MANIFEST_FILE: &str = "storage-format.json";

/// Canonical filename for the legacy SQLite repository database under `.libra/`.
pub const LEGACY_SQLITE_DB_FILE: &str = "libra.db";

/// Current JSON schema version for [`StorageFormatManifest`].
pub const STORAGE_FORMAT_MANIFEST_VERSION: u32 = 1;

/// Initial generation identifier for newly created or first-migrated stores.
pub const INITIAL_STORAGE_GENERATION: u64 = 1;

/// Dedicated subdirectory under `.libra/` hosting all current and future Git `.redb` generations.
pub const REDB_SUBDIR: &str = "redb";

/// Private subdirectory under `.libra/` hosting Capture `.redb` generations.
pub const PRIVATE_SUBDIR: &str = "private";

/// Defensive upper bound on manifest file size to avoid unbounded allocations on corrupt files.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Monotonic per-process counter for collision-free atomic temp filenames.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Per-database progress state persisted in `storage-format.json` across Phase 2 cutover (`ADR-RB-07`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase2DbState {
    /// Source database has not yet been fenced or unlinked.
    Pending,
    /// Source database fence has been committed durably and is ready for `unlink`.
    FencedReadyToUnlink,
    /// Source database path has been unlinked and its parent directory fsynced.
    Unlinked,
    /// Optional source database (e.g., `capture.db` in pre-split repos) was not present.
    Absent,
}

/// Phase 2 cutover progress ledger persisted inside [`StorageFormatManifest`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase2Progress {
    /// Forward migration (`SQLite -> redb`) state for `.libra/libra.db`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_db: Option<Phase2DbState>,
    /// Forward migration (`SQLite -> redb`) state for `.libra/private/capture.db`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_db: Option<Phase2DbState>,
    /// Reverse migration (`redb -> SQLite`) state for `git.gen-<id>.redb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_redb: Option<Phase2DbState>,
    /// Reverse migration (`redb -> SQLite`) state for `capture.gen-<id>.redb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_redb: Option<Phase2DbState>,
    /// Whether `.libra/private/capture-spool/{tmp,ready,quarantine}` tombstones are installed.
    pub spool_fenced: bool,
    /// Whether `.libra/private/capture-queue/{priority,normal,quarantine}` tombstones are installed.
    pub queue_fenced: bool,
}

/// Logical and backup SHA-256 digests captured during Phase 1 for crash-recovery verification (`ADR-RB-07`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDigests {
    /// Canonical logical digest of source `libra.db` computed over the locked migration connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_logical_sha256: Option<String>,
    /// File SHA-256 of the verified `VACUUM INTO` backup of `libra.db`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_backup_sha256: Option<String>,
    /// Canonical logical digest of source `capture.db`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_logical_sha256: Option<String>,
    /// File SHA-256 of the verified `VACUUM INTO` backup of `capture.db`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_backup_sha256: Option<String>,
    /// File SHA-256 of backed-up source `git.gen-<id>.redb` (reverse migration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_redb_sha256: Option<String>,
    /// File SHA-256 of backed-up source `capture.gen-<id>.redb` (reverse migration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_redb_sha256: Option<String>,
    /// Deterministic manifest SHA-256 of snapshotted `capture-spool` events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spool_manifest_sha256: Option<String>,
    /// Deterministic manifest SHA-256 of snapshotted `capture-queue` frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_manifest_sha256: Option<String>,
}

impl SourceDigests {
    /// Returns `true` if any source or backup digest field is populated.
    #[must_use]
    pub const fn has_any(&self) -> bool {
        self.git_logical_sha256.is_some()
            || self.git_backup_sha256.is_some()
            || self.cap_logical_sha256.is_some()
            || self.cap_backup_sha256.is_some()
            || self.git_redb_sha256.is_some()
            || self.cap_redb_sha256.is_some()
            || self.spool_manifest_sha256.is_some()
            || self.queue_manifest_sha256.is_some()
    }
}

/// Authoritative provenance distinguishing a freshly initialized `redb`
/// repository manifest from a two-phase migration cutover manifest.
///
/// Prevents a tampered `finalized = true` migration manifest from bypassing
/// terminal Phase 2 progress validation by omitting or nulling database
/// progress fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestOrigin {
    /// Brand-new `redb` repository created directly with no migration history.
    FreshInit,
    /// Two-phase `SQLite <-> redb` migration cutover manifest.
    Migration,
}

/// Authoritative two-phase repository storage manifest stored at `.libra/storage-format.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageFormatManifest {
    /// Manifest format version (currently [`STORAGE_FORMAT_MANIFEST_VERSION`] = `1`).
    pub schema_version: u32,
    /// Authoritative provenance (`fresh_init` vs `migration`).
    pub origin: ManifestOrigin,
    /// Authoritative storage backend (`sqlite` or `redb`).
    pub backend: StorageBackendKind,
    /// `false` during Phase 2 cutover (forces fail-closed `LBR-IO-001` for normal commands);
    /// `true` once cutover has durably completed.
    pub finalized: bool,
    /// Monotonically increasing generation identifier (`>= 1`, formatted as 8-digit zero-padded).
    pub generation: u64,
    /// Relative path under `<common_storage>` to the active Git store (`redb/git.gen-<id>.redb` or `libra.db`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_path: Option<String>,
    /// Relative path under `<common_storage>` to the active Capture store (`private/capture.gen-<id>.redb` or `private/capture.db`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_path: Option<String>,
    /// Relative path under `<common_storage>` to the migration backup directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
    /// Per-database and spool/queue fencing progress during Phase 2 cutover.
    pub phase2_progress: Phase2Progress,
    /// `true` once all source stores are fenced/unlinked and target files are staged for non-overwriting publish.
    pub targets_staged_for_rename: bool,
    /// Non-zero `PRAGMA application_id` tag written to reverse-migrated SQLite targets to distinguish them from racing legacy creates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_migration_tag: Option<u32>,
    /// `true` after post-finalization `capture-spool` tombstone cleanup has completed (reverse migration).
    pub spool_unfenced: bool,
    /// Frozen source digests used to detect crash-gap drift and verify backups during Phase 2 recovery.
    pub source_digests: SourceDigests,
}

/// Generation-scoped absolute database paths resolved from a finalized `redb` manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedbGenerationPaths {
    /// Active manifest generation (`>= 1`).
    pub generation: u64,
    /// Absolute path to `<common_storage>/redb/git.gen-<id>.redb`.
    pub git_db_path: PathBuf,
    /// Absolute path to `<common_storage>/private/capture.gen-<id>.redb`.
    pub capture_db_path: PathBuf,
}

/// Complete resolved repository storage layout from a finalized `.libra/storage-format.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStorageLayout {
    /// Authoritative storage backend declared by the finalized manifest.
    pub backend: StorageBackendKind,
    /// Active manifest generation (`>= 1`).
    pub generation: u64,
    /// Resolved absolute path to the Git database file.
    pub git_db_path: PathBuf,
    /// Resolved absolute path to the Capture database file.
    pub capture_db_path: PathBuf,
    /// Full validated manifest record.
    pub manifest: StorageFormatManifest,
}

/// Authoritative repository storage state detected directly from `<common_storage>`
/// BEFORE inspecting `LIBRA_STORAGE_BACKEND` (`ADR-RB-01` #3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageAuthority {
    /// A finalized `storage-format.json` manifest governs the repository.
    Manifest(Box<ResolvedStorageLayout>),
    /// No manifest exists, but legacy `<common_storage>/libra.db` exists.
    LegacySqlite {
        /// Absolute path to `<common_storage>/libra.db`.
        git_db_path: PathBuf,
    },
    /// Neither `storage-format.json` nor `libra.db` exists (brand-new repository).
    /// Any uncommitted `.libra/redb/*.redb` or `*.tmp` fragments are ignored.
    FreshEmpty,
}

impl StorageAuthority {
    /// Return the authoritative backend enforced by on-disk state, or `None` for
    /// a fresh empty repository where `LIBRA_STORAGE_BACKEND` may override the
    /// compiled default.
    #[must_use]
    pub const fn backend(&self) -> Option<StorageBackendKind> {
        match self {
            Self::Manifest(layout) => Some(layout.backend),
            Self::LegacySqlite { .. } => Some(StorageBackendKind::Sqlite),
            Self::FreshEmpty => None,
        }
    }

    /// Returns `true` only when the repository is brand-new (no manifest and no
    /// legacy `libra.db`).
    #[must_use]
    pub const fn is_fresh_empty(&self) -> bool {
        matches!(self, Self::FreshEmpty)
    }
}

/// Format a numeric generation as an 8-digit zero-padded string (e.g. `1` -> `"00000001"`).
#[must_use]
pub fn format_generation_id(generation: u64) -> String {
    format!("{generation:08}")
}

/// Canonical relative path under `<common_storage>` for a Git `redb` generation.
#[must_use]
pub fn canonical_git_redb_rel_path(generation: u64) -> String {
    format!("{REDB_SUBDIR}/git.gen-{generation:08}.redb")
}

/// Canonical relative path under `<common_storage>` for a Capture `redb` generation.
#[must_use]
pub fn canonical_capture_redb_rel_path(generation: u64) -> String {
    format!("{PRIVATE_SUBDIR}/capture.gen-{generation:08}.redb")
}

/// Return the canonical manifest path `<common_storage>/storage-format.json`.
#[must_use]
pub fn storage_manifest_path(common_storage: &Path) -> PathBuf {
    common_storage.join(STORAGE_FORMAT_MANIFEST_FILE)
}

impl StorageFormatManifest {
    /// Construct a Phase 2 in-progress (`finalized = false`) `redb` manifest with
    /// canonical generation paths and full recovery metadata.
    pub fn new_redb_in_progress(
        generation: u64,
        backup_dir: Option<String>,
        phase2_progress: Phase2Progress,
        source_digests: SourceDigests,
    ) -> StorageResult<Self> {
        let manifest = Self {
            schema_version: STORAGE_FORMAT_MANIFEST_VERSION,
            origin: ManifestOrigin::Migration,
            backend: StorageBackendKind::Redb,
            finalized: false,
            generation,
            git_path: Some(canonical_git_redb_rel_path(generation)),
            capture_path: Some(canonical_capture_redb_rel_path(generation)),
            backup_dir,
            phase2_progress,
            targets_staged_for_rename: false,
            target_migration_tag: None,
            spool_unfenced: false,
            source_digests,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Construct a finalized (`finalized = true`) `redb` manifest for a freshly
    /// initialized repository at `generation`.
    pub fn new_redb_finalized(generation: u64) -> StorageResult<Self> {
        let manifest = Self {
            schema_version: STORAGE_FORMAT_MANIFEST_VERSION,
            origin: ManifestOrigin::FreshInit,
            backend: StorageBackendKind::Redb,
            finalized: true,
            generation,
            git_path: Some(canonical_git_redb_rel_path(generation)),
            capture_path: Some(canonical_capture_redb_rel_path(generation)),
            backup_dir: None,
            phase2_progress: Phase2Progress::default(),
            targets_staged_for_rename: false,
            target_migration_tag: None,
            spool_unfenced: false,
            source_digests: SourceDigests::default(),
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Construct a Phase 2 in-progress (`finalized = false`) `sqlite` manifest
    /// for reverse migration (`redb -> SQLite`, `ADR-RB-07` #2).
    pub fn new_sqlite_in_progress(
        generation: u64,
        target_migration_tag: u32,
        backup_dir: Option<String>,
        phase2_progress: Phase2Progress,
        source_digests: SourceDigests,
    ) -> StorageResult<Self> {
        if target_migration_tag == 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: "reverse migration target_migration_tag must be non-zero".to_string(),
            });
        }
        let manifest = Self {
            schema_version: STORAGE_FORMAT_MANIFEST_VERSION,
            origin: ManifestOrigin::Migration,
            backend: StorageBackendKind::Sqlite,
            finalized: false,
            generation,
            git_path: Some("libra.db".to_string()),
            capture_path: Some("private/capture.db".to_string()),
            backup_dir,
            phase2_progress,
            targets_staged_for_rename: false,
            target_migration_tag: Some(target_migration_tag),
            spool_unfenced: false,
            source_digests,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Transition this manifest into its finalized (`finalized = true`) form after
    /// verifying that all direction-specific Phase 2 cutover states have reached
    /// terminal completion (`ADR-RB-07`).
    pub fn into_finalized(mut self) -> StorageResult<Self> {
        self.validate()?;
        self.validate_terminal_migration_progress()?;
        self.finalized = true;
        self.validate()?;
        Ok(self)
    }

    /// Returns `true` if any migration recovery metadata field is populated.
    fn has_any_migration_metadata(&self) -> bool {
        self.backup_dir.is_some()
            || self.target_migration_tag.is_some()
            || self.targets_staged_for_rename
            || self.spool_unfenced
            || self.source_digests.has_any()
            || self.phase2_progress.git_db.is_some()
            || self.phase2_progress.capture_db.is_some()
            || self.phase2_progress.git_redb.is_some()
            || self.phase2_progress.cap_redb.is_some()
            || self.phase2_progress.spool_fenced
            || self.phase2_progress.queue_fenced
    }

    /// Enforce strict separation between `ManifestOrigin::FreshInit` and
    /// `ManifestOrigin::Migration`, and require full direction-specific terminal
    /// Phase 2 completion whenever a migration manifest is finalized.
    fn validate_terminal_migration_progress(&self) -> StorageResult<()> {
        if self.origin == ManifestOrigin::FreshInit {
            if self.backend != StorageBackendKind::Redb {
                return Err(StorageBackendError::BackendInternal {
                    detail: "fresh_init storage-format.json is only valid for `redb` backend"
                        .to_string(),
                });
            }
            if !self.finalized {
                return Err(StorageBackendError::BackendInternal {
                    detail: "fresh_init storage-format.json must have finalized=true".to_string(),
                });
            }
            if self.has_any_migration_metadata() {
                return Err(StorageBackendError::BackendInternal {
                    detail:
                        "fresh_init storage-format.json must not carry migration recovery metadata"
                            .to_string(),
                });
            }
            return Ok(());
        }

        // `ManifestOrigin::Migration`: terminal progress is ALWAYS mandatory
        // when finalizing or validating a finalized migration manifest — removing
        // or nulling both DB progress fields can never bypass terminal checks.
        let has_forward_progress =
            self.phase2_progress.git_db.is_some() || self.phase2_progress.capture_db.is_some();
        let has_reverse_progress =
            self.phase2_progress.git_redb.is_some() || self.phase2_progress.cap_redb.is_some();

        match self.backend {
            StorageBackendKind::Redb => {
                if has_reverse_progress {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "finalized redb storage-format.json must not carry reverse redb progress fields"
                                .to_string(),
                    });
                }
                if self.phase2_progress.git_db != Some(Phase2DbState::Unlinked) {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "cannot finalize forward redb migration while phase2_progress.git_db is not `unlinked`"
                                .to_string(),
                    });
                }
                if !matches!(
                    self.phase2_progress.capture_db,
                    Some(Phase2DbState::Unlinked | Phase2DbState::Absent)
                ) {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "cannot finalize forward redb migration while phase2_progress.capture_db is not `unlinked` or `absent`"
                                .to_string(),
                    });
                }
                if !self.phase2_progress.spool_fenced {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "cannot finalize forward redb migration while phase2_progress.spool_fenced is false"
                                .to_string(),
                    });
                }
            }
            StorageBackendKind::Sqlite => {
                if has_forward_progress {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "finalized sqlite storage-format.json must not carry forward sqlite progress fields"
                                .to_string(),
                    });
                }
                if self.phase2_progress.git_redb != Some(Phase2DbState::Unlinked) {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "cannot finalize reverse sqlite migration while phase2_progress.git_redb is not `unlinked`"
                                .to_string(),
                    });
                }
                if self.phase2_progress.cap_redb != Some(Phase2DbState::Unlinked) {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "cannot finalize reverse sqlite migration while phase2_progress.cap_redb is not `unlinked`"
                                .to_string(),
                    });
                }
                if !self.phase2_progress.queue_fenced {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "cannot finalize reverse sqlite migration while phase2_progress.queue_fenced is false"
                                .to_string(),
                    });
                }
                if !self.phase2_progress.spool_fenced {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "cannot finalize reverse sqlite migration while phase2_progress.spool_fenced is false"
                                .to_string(),
                    });
                }
                if !self.targets_staged_for_rename {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "cannot finalize reverse sqlite migration while targets_staged_for_rename is false"
                                .to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Enforce that `targets_staged_for_rename = true` on a reverse (`sqlite`)
    /// migration is only valid once both redb sources are `unlinked` and both
    /// queue and spool fences are installed (`ADR-RB-07` #2 Step 2.4).
    fn validate_reverse_staging_preconditions(&self) -> StorageResult<()> {
        if self.phase2_progress.git_redb != Some(Phase2DbState::Unlinked)
            || self.phase2_progress.cap_redb != Some(Phase2DbState::Unlinked)
            || !self.phase2_progress.queue_fenced
            || !self.phase2_progress.spool_fenced
        {
            return Err(StorageBackendError::BackendInternal {
                detail:
                    "reverse sqlite storage-format.json cannot set targets_staged_for_rename=true before git_redb/cap_redb are unlinked and queue/spool are fenced"
                        .to_string(),
            });
        }
        Ok(())
    }

    /// Validate manifest schema version, generation bounds, and canonical relative paths.
    pub fn validate(&self) -> StorageResult<()> {
        if self.schema_version == 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: "storage-format.json has invalid schema_version 0".to_string(),
            });
        }
        if self.schema_version > STORAGE_FORMAT_MANIFEST_VERSION {
            return Err(StorageBackendError::SchemaTooNew {
                role: DatabaseRole::Repository,
                found: self.schema_version,
                max_supported: STORAGE_FORMAT_MANIFEST_VERSION,
            });
        }
        if self.generation == 0 {
            return Err(StorageBackendError::BackendInternal {
                detail: "storage-format.json generation must be >= 1".to_string(),
            });
        }
        if self.origin == ManifestOrigin::FreshInit && !self.finalized {
            return Err(StorageBackendError::BackendInternal {
                detail: "fresh_init storage-format.json must have finalized=true".to_string(),
            });
        }
        if let Some(tag) = self.target_migration_tag
            && tag == 0
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "storage-format.json target_migration_tag must be non-zero when present"
                    .to_string(),
            });
        }

        if let Some(backup) = &self.backup_dir {
            validate_safe_relative_path(backup, "backup_dir")?;
        }

        // Validate that any populated digest fields are non-empty.
        for (name, digest) in [
            (
                "git_logical_sha256",
                &self.source_digests.git_logical_sha256,
            ),
            ("git_backup_sha256", &self.source_digests.git_backup_sha256),
            (
                "cap_logical_sha256",
                &self.source_digests.cap_logical_sha256,
            ),
            ("cap_backup_sha256", &self.source_digests.cap_backup_sha256),
            ("git_redb_sha256", &self.source_digests.git_redb_sha256),
            ("cap_redb_sha256", &self.source_digests.cap_redb_sha256),
            (
                "spool_manifest_sha256",
                &self.source_digests.spool_manifest_sha256,
            ),
            (
                "queue_manifest_sha256",
                &self.source_digests.queue_manifest_sha256,
            ),
        ] {
            if let Some(val) = digest
                && val.trim().is_empty()
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "storage-format.json `source_digests.{name}` must not be empty"
                    ),
                });
            }
        }

        match self.backend {
            StorageBackendKind::Redb => {
                let expected_git = canonical_git_redb_rel_path(self.generation);
                let expected_capture = canonical_capture_redb_rel_path(self.generation);

                let git = self.git_path.as_deref().ok_or_else(|| {
                    StorageBackendError::BackendInternal {
                        detail: "redb storage-format.json is missing git_path".to_string(),
                    }
                })?;
                validate_safe_relative_path(git, "git_path")?;
                if git != expected_git {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "redb storage-format.json git_path does not match generation {}",
                            format_generation_id(self.generation)
                        ),
                    });
                }

                let capture = self.capture_path.as_deref().ok_or_else(|| {
                    StorageBackendError::BackendInternal {
                        detail: "redb storage-format.json is missing capture_path".to_string(),
                    }
                })?;
                validate_safe_relative_path(capture, "capture_path")?;
                if capture != expected_capture {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "redb storage-format.json capture_path does not match generation {}",
                            format_generation_id(self.generation)
                        ),
                    });
                }

                if !self.finalized {
                    // Forward Phase 2 (`SQLite -> redb`) recovery invariants (`ADR-RB-07` #1).
                    if self.backup_dir.is_none() {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "in-progress redb storage-format.json is missing backup_dir"
                                .to_string(),
                        });
                    }
                    if self.phase2_progress.git_db.is_none() {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress redb storage-format.json is missing phase2_progress.git_db"
                                    .to_string(),
                        });
                    }
                    let cap_state = self.phase2_progress.capture_db.ok_or_else(|| {
                        StorageBackendError::BackendInternal {
                            detail:
                                "in-progress redb storage-format.json is missing phase2_progress.capture_db"
                                    .to_string(),
                        }
                    })?;
                    if self.phase2_progress.git_redb.is_some()
                        || self.phase2_progress.cap_redb.is_some()
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress forward redb storage-format.json must not contain reverse redb progress fields"
                                    .to_string(),
                        });
                    }
                    if self.source_digests.git_logical_sha256.is_none()
                        || self.source_digests.git_backup_sha256.is_none()
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress redb storage-format.json is missing required git source digests"
                                    .to_string(),
                        });
                    }
                    if cap_state != Phase2DbState::Absent
                        && (self.source_digests.cap_logical_sha256.is_none()
                            || self.source_digests.cap_backup_sha256.is_none())
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress redb storage-format.json is missing required capture source digests"
                                    .to_string(),
                        });
                    }
                    if self.source_digests.spool_manifest_sha256.is_none()
                        || self.source_digests.queue_manifest_sha256.is_none()
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress redb storage-format.json is missing required spool/queue manifest digests"
                                    .to_string(),
                        });
                    }
                } else {
                    self.validate_terminal_migration_progress()?;
                }
            }
            StorageBackendKind::Sqlite => {
                if let Some(git) = &self.git_path {
                    validate_safe_relative_path(git, "git_path")?;
                    if git != "libra.db" {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "sqlite storage-format.json git_path must be `libra.db`"
                                .to_string(),
                        });
                    }
                }
                if let Some(capture) = &self.capture_path {
                    validate_safe_relative_path(capture, "capture_path")?;
                    if capture != "private/capture.db" {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "sqlite storage-format.json capture_path must be `private/capture.db`"
                                    .to_string(),
                        });
                    }
                }

                if !self.finalized {
                    // Reverse Phase 2 (`redb -> SQLite`) recovery invariants (`ADR-RB-07` #2).
                    if self.target_migration_tag.is_none() {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress sqlite storage-format.json is missing non-zero target_migration_tag"
                                    .to_string(),
                        });
                    }
                    if self.backup_dir.is_none() {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "in-progress sqlite storage-format.json is missing backup_dir"
                                .to_string(),
                        });
                    }
                    if self.spool_unfenced {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress sqlite storage-format.json must keep spool_unfenced=false until finalized=true"
                                    .to_string(),
                        });
                    }
                    if self.phase2_progress.git_redb.is_none()
                        || self.phase2_progress.cap_redb.is_none()
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress sqlite storage-format.json is missing phase2_progress.git_redb/cap_redb"
                                    .to_string(),
                        });
                    }
                    if self.phase2_progress.git_db.is_some()
                        || self.phase2_progress.capture_db.is_some()
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress reverse sqlite storage-format.json must not contain forward sqlite progress fields"
                                    .to_string(),
                        });
                    }
                    if self.source_digests.git_redb_sha256.is_none()
                        || self.source_digests.cap_redb_sha256.is_none()
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress sqlite storage-format.json is missing required redb backup digests"
                                    .to_string(),
                        });
                    }
                    if self.source_digests.spool_manifest_sha256.is_none()
                        || self.source_digests.queue_manifest_sha256.is_none()
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "in-progress sqlite storage-format.json is missing required spool/queue manifest digests"
                                    .to_string(),
                        });
                    }
                    if self.targets_staged_for_rename {
                        self.validate_reverse_staging_preconditions()?;
                    }
                } else {
                    self.validate_terminal_migration_progress()?;
                }
            }
        }

        Ok(())
    }

    /// Resolve absolute Git and Capture database paths under `common_storage`.
    ///
    /// Fails closed if the manifest is not finalized or fails validation.
    pub fn resolve_paths(&self, common_storage: &Path) -> StorageResult<(PathBuf, PathBuf)> {
        self.validate()?;
        if !self.finalized {
            return Err(StorageBackendError::ManifestNotFinalized {
                detail: format!(
                    "storage-format.json for backend `{}` generation {} has finalized=false",
                    self.backend,
                    format_generation_id(self.generation)
                ),
            });
        }
        let (git_rel, capture_rel) = match self.backend {
            StorageBackendKind::Redb => (
                canonical_git_redb_rel_path(self.generation),
                canonical_capture_redb_rel_path(self.generation),
            ),
            StorageBackendKind::Sqlite => {
                ("libra.db".to_string(), "private/capture.db".to_string())
            }
        };
        Ok((
            common_storage.join(git_rel),
            common_storage.join(capture_rel),
        ))
    }
}

fn validate_safe_relative_path(rel: &str, field: &'static str) -> StorageResult<()> {
    if rel.is_empty() || rel.contains('\\') || rel.contains('\0') {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("storage-format.json `{field}` contains invalid path syntax"),
        });
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("storage-format.json `{field}` must be a relative path"),
        });
    }
    let mut saw_component = false;
    for comp in path.components() {
        match comp {
            Component::Normal(_) => saw_component = true,
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "storage-format.json `{field}` contains disallowed path component"
                    ),
                });
            }
        }
    }
    if !saw_component {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("storage-format.json `{field}` is empty"),
        });
    }
    Ok(())
}

/// Maximum number of retries if a generated temp filename collides with a
/// pre-existing file after PID reuse.
const MAX_TEMP_CREATE_ATTEMPTS: usize = 64;

/// Internal filesystem operations abstraction for deterministic testing of the
/// pre-publish durability check and `.tmp` → `fsync` → `rename` → `fsync_dir` sequence.
trait AtomicPublishOps {
    fn ensure_durable_sync_supported(&self) -> StorageResult<()>;
    fn create_temp(&self, tmp_path: &Path) -> StorageResult<File>;
    fn write_temp_bytes(&self, file: &mut File, bytes: &[u8]) -> StorageResult<()>;
    fn fsync_temp(&self, file: &File) -> StorageResult<()>;
    fn rename_temp_to_final(&self, tmp_path: &Path, final_path: &Path) -> StorageResult<()>;
    fn fsync_parent_dir(&self, dir: &Path) -> StorageResult<()>;
}

struct OsPublishOps;

impl AtomicPublishOps for OsPublishOps {
    fn ensure_durable_sync_supported(&self) -> StorageResult<()> {
        ensure_durable_directory_sync_supported()
    }

    fn create_temp(&self, tmp_path: &Path) -> StorageResult<File> {
        // `create_new(true)` uses `O_CREAT | O_EXCL` on all platforms, never
        // following pre-existing symlinks or clobbering pre-existing files.
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(tmp_path)
            .map_err(|err| StorageBackendError::io("create storage-format.json temp file", err))
    }

    fn write_temp_bytes(&self, file: &mut File, bytes: &[u8]) -> StorageResult<()> {
        file.write_all(bytes)
            .map_err(|err| StorageBackendError::io("write storage-format.json temp file", err))
    }

    fn fsync_temp(&self, file: &File) -> StorageResult<()> {
        file.sync_all()
            .map_err(|err| StorageBackendError::io("fsync storage-format.json temp file", err))
    }

    fn rename_temp_to_final(&self, tmp_path: &Path, final_path: &Path) -> StorageResult<()> {
        durable_atomic_rename(tmp_path, final_path)
    }

    fn fsync_parent_dir(&self, dir: &Path) -> StorageResult<()> {
        fsync_dir(dir)
    }
}

/// Fail closed before writing or renaming on targets that lack both POSIX
/// directory `fsync` and Windows `MOVEFILE_WRITE_THROUGH`.
fn ensure_durable_directory_sync_supported() -> StorageResult<()> {
    #[cfg(any(unix, windows))]
    {
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(StorageBackendError::BackendInternal {
            detail: "durable directory metadata sync is unsupported on this target platform"
                .to_string(),
        })
    }
}

/// Atomically replace `final_path` with `tmp_path`.
///
/// On Windows, uses `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
/// MOVEFILE_WRITE_THROUGH` (mirroring `src/utils/atomic_stream.rs`) so the
/// directory entry replacement reaches durable storage before returning.
#[cfg(windows)]
fn durable_atomic_rename(tmp_path: &Path, final_path: &Path) -> StorageResult<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = tmp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = final_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: Both `source` and `destination` are valid NUL-terminated UTF-16
    // buffers alive for the duration of the synchronous Win32 syscall.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(StorageBackendError::io(
            "atomically rename storage-format.json temp file with write-through",
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

/// Atomically replace `final_path` with `tmp_path` on non-Windows platforms.
#[cfg(not(windows))]
fn durable_atomic_rename(tmp_path: &Path, final_path: &Path) -> StorageResult<()> {
    fs::rename(tmp_path, final_path).map_err(|err| {
        StorageBackendError::io("atomically rename storage-format.json temp file", err)
    })
}

/// Best-effort cleanup guard for a temp file created by THIS publish call.
struct TempCleanupGuard {
    path: Option<PathBuf>,
}

impl TempCleanupGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TempCleanupGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Flush parent directory metadata durably on Unix (`fsync_dir`).
#[cfg(unix)]
pub(crate) fn fsync_dir(dir: &Path) -> StorageResult<()> {
    let handle =
        File::open(dir).map_err(|err| StorageBackendError::io("open directory for fsync", err))?;
    handle
        .sync_all()
        .map_err(|err| StorageBackendError::io("fsync directory metadata", err))
}

/// Verify directory presence on Windows where [`durable_atomic_rename`] already
/// flushed the rename metadata via `MOVEFILE_WRITE_THROUGH`.
#[cfg(windows)]
pub(crate) fn fsync_dir(dir: &Path) -> StorageResult<()> {
    let meta = fs::metadata(dir).map_err(|err| {
        StorageBackendError::io("verify directory after write-through rename", err)
    })?;
    if !meta.is_dir() {
        return Err(StorageBackendError::BackendInternal {
            detail: "parent directory disappeared after write-through rename".to_string(),
        });
    }
    Ok(())
}

/// Fail closed on platforms without a verified durable directory-flush or
/// write-through rename primitive rather than silently reporting success.
#[cfg(not(any(unix, windows)))]
pub(crate) fn fsync_dir(_dir: &Path) -> StorageResult<()> {
    Err(StorageBackendError::BackendInternal {
        detail: "durable directory metadata sync is unsupported on this target platform"
            .to_string(),
    })
}

fn publish_storage_manifest_with_ops(
    common_storage: &Path,
    manifest: &StorageFormatManifest,
    ops: &impl AtomicPublishOps,
) -> StorageResult<PathBuf> {
    manifest.validate()?;
    // Fail closed BEFORE creating or renaming any file if durable directory
    // metadata sync is unavailable on this platform.
    ops.ensure_durable_sync_supported()?;

    let dir_meta = fs::metadata(common_storage).map_err(|err| {
        StorageBackendError::io("verify storage directory before manifest publish", err)
    })?;
    if !dir_meta.is_dir() {
        return Err(StorageBackendError::BackendInternal {
            detail: "cannot publish storage-format.json: target storage path is not a directory"
                .to_string(),
        });
    }

    let final_path = storage_manifest_path(common_storage);
    // Reject a pre-existing symlink at the target manifest path so atomic rename
    // cannot mask a tampered directory layout.
    if let Ok(meta) = fs::symlink_metadata(&final_path)
        && (meta.file_type().is_symlink() || !meta.is_file())
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "refusing to overwrite non-regular storage-format.json target".to_string(),
        });
    }

    let mut payload = serde_json::to_vec_pretty(manifest).map_err(|err| {
        StorageBackendError::BackendInternal {
            detail: format!(
                "failed to serialize storage-format.json ({:?})",
                err.classify()
            ),
        }
    })?;
    payload.push(b'\n');

    // Create a fresh temporary file with `O_CREAT | O_EXCL`, retrying on
    // `AlreadyExists` without ever deleting a pre-existing colliding file.
    // Arm cleanup ONLY after `create_temp` succeeds for the newly created file.
    let mut created = None;
    for _ in 0..MAX_TEMP_CREATE_ATTEMPTS {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(
            "{STORAGE_FORMAT_MANIFEST_FILE}.{}.{}.tmp",
            std::process::id(),
            seq
        );
        let candidate = common_storage.join(tmp_name);
        match ops.create_temp(&candidate) {
            Ok(file) => {
                created = Some((candidate, file));
                break;
            }
            Err(err) if err.io_kind() == Some(io::ErrorKind::AlreadyExists) => continue,
            Err(err) => return Err(err),
        }
    }
    let (tmp_path, mut file) = created.ok_or_else(|| StorageBackendError::BackendInternal {
        detail: "exhausted temporary filename attempts while publishing storage-format.json"
            .to_string(),
    })?;

    let mut cleanup = TempCleanupGuard::new(tmp_path.clone());
    ops.write_temp_bytes(&mut file, &payload)?;
    ops.fsync_temp(&file)?;
    drop(file);
    ops.rename_temp_to_final(&tmp_path, &final_path)?;
    cleanup.disarm();
    ops.fsync_parent_dir(common_storage)?;

    Ok(final_path)
}

/// Atomically publish `<common_storage>/storage-format.json` via
/// `.tmp` → `fsync` → `rename` → `fsync_dir` (`ADR-RB-02` #1, `ADR-RB-07`).
pub fn publish_storage_manifest(
    common_storage: &Path,
    manifest: &StorageFormatManifest,
) -> StorageResult<PathBuf> {
    publish_storage_manifest_with_ops(common_storage, manifest, &OsPublishOps)
}

/// Read and validate `<common_storage>/storage-format.json` if present.
///
/// Returns `Ok(None)` when `storage-format.json` does not exist on disk.
/// Never scans `.libra/redb/` or `.libra/private/` for uncommitted fragments.
/// Fails closed if the manifest is a symlink, exceeds size limits, contains
/// invalid JSON, carries an unsupported future `schema_version`, or fails
/// structural validation.
pub fn read_storage_manifest(
    common_storage: &Path,
) -> StorageResult<Option<StorageFormatManifest>> {
    let path = storage_manifest_path(common_storage);
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(StorageBackendError::io("stat storage-format.json", err)),
    };

    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail:
                "storage-format.json must be a regular file (symlinks and directories are rejected)"
                    .to_string(),
        });
    }

    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "storage-format.json exceeds maximum allowed size ({} > {MAX_MANIFEST_BYTES} bytes)",
                meta.len()
            ),
        });
    }

    let file = File::open(&path)
        .map_err(|err| StorageBackendError::io("open storage-format.json", err))?;
    let mut raw = Vec::with_capacity(meta.len().min(MAX_MANIFEST_BYTES) as usize);
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|err| StorageBackendError::io("read storage-format.json", err))?;
    if raw.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "storage-format.json exceeds maximum allowed size ({MAX_MANIFEST_BYTES} bytes)"
            ),
        });
    }

    // Parse generic JSON value first so future `schema_version` values report
    // `SchemaTooNew` even if the future schema changed field shapes.
    // Sanitize serde diagnostics so corrupt field values (such as private paths
    // or tokens inside enum fields) can never leak into user-facing errors.
    let json_val: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|err| StorageBackendError::BackendInternal {
            detail: format!(
                "storage-format.json is corrupt or invalid JSON ({:?} at line {}, column {})",
                err.classify(),
                err.line(),
                err.column()
            ),
        })?;
    if let Some(ver_u64) = json_val
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        && ver_u64 > u64::from(STORAGE_FORMAT_MANIFEST_VERSION)
    {
        let found = u32::try_from(ver_u64).unwrap_or(u32::MAX);
        return Err(StorageBackendError::SchemaTooNew {
            role: DatabaseRole::Repository,
            found,
            max_supported: STORAGE_FORMAT_MANIFEST_VERSION,
        });
    }

    let manifest: StorageFormatManifest =
        serde_json::from_value(json_val).map_err(|err| StorageBackendError::BackendInternal {
            detail: format!(
                "storage-format.json failed schema deserialization ({:?})",
                err.classify()
            ),
        })?;
    manifest.validate()?;
    Ok(Some(manifest))
}

/// Resolve the authoritative finalized storage layout from `<common_storage>/storage-format.json`.
///
/// - Absent manifest → `Ok(None)` (never selects `.libra/redb/*.redb` or `*.tmp` fragments).
/// - `finalized == false` → `Err(StorageBackendError::ManifestNotFinalized)` (`LBR-IO-001`).
/// - `finalized == true` → `Ok(Some(ResolvedStorageLayout))` with canonical generation paths.
pub fn resolve_storage_layout(
    common_storage: &Path,
) -> StorageResult<Option<ResolvedStorageLayout>> {
    let Some(manifest) = read_storage_manifest(common_storage)? else {
        return Ok(None);
    };
    let (git_db_path, capture_db_path) = manifest.resolve_paths(common_storage)?;
    Ok(Some(ResolvedStorageLayout {
        backend: manifest.backend,
        generation: manifest.generation,
        git_db_path,
        capture_db_path,
        manifest,
    }))
}

/// Resolve generation-scoped `redb` database paths strictly from a finalized
/// `redb` manifest at `<common_storage>/storage-format.json`.
///
/// - Absent manifest (even when uncommitted `.redb`/`.tmp` fragments exist in
///   `.libra/redb/` or `.libra/private/`) → `Ok(None)`.
/// - Finalized `sqlite` manifest → `Ok(None)`.
/// - Unfinalized (`finalized == false`) manifest → `Err(ManifestNotFinalized)` (`LBR-IO-001`).
/// - Finalized `redb` manifest → `Ok(Some(RedbGenerationPaths))` pointing to
///   `.libra/redb/git.gen-<id>.redb` and `.libra/private/capture.gen-<id>.redb`.
pub fn resolve_redb_generation_paths(
    common_storage: &Path,
) -> StorageResult<Option<RedbGenerationPaths>> {
    let Some(layout) = resolve_storage_layout(common_storage)? else {
        return Ok(None);
    };
    if layout.backend != StorageBackendKind::Redb {
        return Ok(None);
    }
    Ok(Some(RedbGenerationPaths {
        generation: layout.generation,
        git_db_path: layout.git_db_path,
        capture_db_path: layout.capture_db_path,
    }))
}

/// Detect the authoritative on-disk storage state for `<common_storage>` BEFORE
/// inspecting `LIBRA_STORAGE_BACKEND` (`ADR-RB-01` #3).
///
/// Resolution precedence:
/// 1. Missing `<common_storage>` directory → [`StorageAuthority::FreshEmpty`].
///    (A regular file or dangling symlink at `<common_storage>` fails closed with `LBR-IO-001`.)
/// 2. `<common_storage>/storage-format.json`:
///    - `finalized == false` → `Err(ManifestNotFinalized)` (`LBR-IO-001`).
///    - corrupt/symlink/future-schema → fail closed (`LBR-IO-001`).
///    - `finalized == true` → [`StorageAuthority::Manifest`].
/// 3. Absent manifest + regular `<common_storage>/libra.db` →
///    [`StorageAuthority::LegacySqlite`] (symlinks/directories at `libra.db` fail closed).
/// 4. Absent manifest + absent `libra.db` → [`StorageAuthority::FreshEmpty`]
///    (uncommitted `.libra/redb/*.redb` or `*.tmp` fragments are ignored).
pub fn detect_storage_authority(common_storage: &Path) -> StorageResult<StorageAuthority> {
    match fs::symlink_metadata(common_storage) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(StorageAuthority::FreshEmpty);
        }
        Err(err) => {
            return Err(StorageBackendError::io("stat storage directory", err));
        }
        Ok(sym_meta) => {
            if sym_meta.file_type().is_symlink() {
                let target_meta = fs::metadata(common_storage).map_err(|err| {
                    StorageBackendError::io("resolve storage directory symlink", err)
                })?;
                if !target_meta.is_dir() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "storage root path symlink does not point to a directory"
                            .to_string(),
                    });
                }
            } else if !sym_meta.is_dir() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "storage root path is not a directory".to_string(),
                });
            }
        }
    }

    if let Some(layout) = resolve_storage_layout(common_storage)? {
        return Ok(StorageAuthority::Manifest(Box::new(layout)));
    }

    let legacy_db_path = common_storage.join(LEGACY_SQLITE_DB_FILE);
    match fs::symlink_metadata(&legacy_db_path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(StorageBackendError::BackendInternal {
                    detail:
                        "legacy libra.db must be a regular file (symlinks and directories are rejected)"
                            .to_string(),
                });
            }
            Ok(StorageAuthority::LegacySqlite {
                git_db_path: legacy_db_path,
            })
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(StorageAuthority::FreshEmpty),
        Err(err) => Err(StorageBackendError::io("stat legacy libra.db", err)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tempfile::tempdir;

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PublishStep {
        CheckDurabilitySupport,
        WriteTemp,
        FsyncTemp,
        RenameToFinal,
        FsyncDir,
    }

    struct RecordingPublishOps {
        steps: Mutex<Vec<PublishStep>>,
        fail_at: Option<PublishStep>,
    }

    impl RecordingPublishOps {
        fn new(fail_at: Option<PublishStep>) -> Self {
            Self {
                steps: Mutex::new(Vec::new()),
                fail_at,
            }
        }

        fn recorded(&self) -> Vec<PublishStep> {
            self.steps.lock().expect("lock steps").clone()
        }
    }

    impl AtomicPublishOps for RecordingPublishOps {
        fn ensure_durable_sync_supported(&self) -> StorageResult<()> {
            self.steps
                .lock()
                .expect("lock steps")
                .push(PublishStep::CheckDurabilitySupport);
            if self.fail_at == Some(PublishStep::CheckDurabilitySupport) {
                return Err(StorageBackendError::BackendInternal {
                    detail:
                        "durable directory metadata sync is unsupported on this target platform"
                            .to_string(),
                });
            }
            OsPublishOps.ensure_durable_sync_supported()
        }

        fn create_temp(&self, tmp_path: &Path) -> StorageResult<File> {
            OsPublishOps.create_temp(tmp_path)
        }

        fn write_temp_bytes(&self, file: &mut File, bytes: &[u8]) -> StorageResult<()> {
            self.steps
                .lock()
                .expect("lock steps")
                .push(PublishStep::WriteTemp);
            if self.fail_at == Some(PublishStep::WriteTemp) {
                return Err(StorageBackendError::io(
                    "injected write_temp failure",
                    io::Error::other("simulated disk full"),
                ));
            }
            OsPublishOps.write_temp_bytes(file, bytes)
        }

        fn fsync_temp(&self, file: &File) -> StorageResult<()> {
            self.steps
                .lock()
                .expect("lock steps")
                .push(PublishStep::FsyncTemp);
            if self.fail_at == Some(PublishStep::FsyncTemp) {
                return Err(StorageBackendError::io(
                    "injected fsync_temp failure",
                    io::Error::other("simulated fsync failure"),
                ));
            }
            OsPublishOps.fsync_temp(file)
        }

        fn rename_temp_to_final(&self, tmp_path: &Path, final_path: &Path) -> StorageResult<()> {
            self.steps
                .lock()
                .expect("lock steps")
                .push(PublishStep::RenameToFinal);
            if self.fail_at == Some(PublishStep::RenameToFinal) {
                return Err(StorageBackendError::io(
                    "injected rename failure",
                    io::Error::other("simulated rename failure"),
                ));
            }
            OsPublishOps.rename_temp_to_final(tmp_path, final_path)
        }

        fn fsync_parent_dir(&self, dir: &Path) -> StorageResult<()> {
            self.steps
                .lock()
                .expect("lock steps")
                .push(PublishStep::FsyncDir);
            if self.fail_at == Some(PublishStep::FsyncDir) {
                return Err(StorageBackendError::io(
                    "injected fsync_dir failure",
                    io::Error::other("simulated dir fsync failure"),
                ));
            }
            OsPublishOps.fsync_parent_dir(dir)
        }
    }

    fn count_tmp_files(dir: &Path) -> usize {
        fs::read_dir(dir)
            .expect("read storage dir")
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".tmp"))
            })
            .count()
    }

    #[test]
    fn two_phase_finalized_manifest_and_absent_fragment_ignore() {
        let dir = tempdir().expect("create tempdir");
        let common_storage = dir.path().join(".libra");
        let redb_dir = common_storage.join(REDB_SUBDIR);
        let private_dir = common_storage.join(PRIVATE_SUBDIR);
        fs::create_dir_all(&redb_dir).expect("create .libra/redb");
        fs::create_dir_all(&private_dir).expect("create .libra/private");

        // AC5: When `storage-format.json` is absent, resolver MUST ignore
        // uncommitted `.redb` and `.tmp` fragments under `.libra/redb/` and `.libra/private/`.
        fs::write(
            redb_dir.join("git.gen-00000001.redb"),
            b"uncommitted-git-redb",
        )
        .expect("seed uncommitted git.redb fragment");
        fs::write(
            redb_dir.join("git.gen-00000002.redb.tmp"),
            b"uncommitted-git-tmp",
        )
        .expect("seed uncommitted git.tmp fragment");
        fs::write(
            private_dir.join("capture.gen-00000001.redb"),
            b"uncommitted-cap-redb",
        )
        .expect("seed uncommitted capture.redb fragment");
        fs::write(
            private_dir.join("capture.gen-00000002.redb.tmp"),
            b"uncommitted-cap-tmp",
        )
        .expect("seed uncommitted capture.tmp fragment");

        assert!(
            read_storage_manifest(&common_storage)
                .expect("absent manifest read must succeed with None")
                .is_none()
        );
        assert!(
            resolve_storage_layout(&common_storage)
                .expect("absent manifest layout must be None")
                .is_none()
        );
        assert!(
            resolve_redb_generation_paths(&common_storage)
                .expect("absent manifest must never pick uncommitted .redb or .tmp fragments")
                .is_none()
        );

        // AC1: Publish `finalized = false` manifest atomically via
        // `.tmp` -> `fsync` -> `rename` -> `fsync_dir` carrying all Phase 2 fields
        // (`phase2_progress`, `targets_staged_for_rename`, `target_migration_tag`,
        // `spool_unfenced`, `source_digests`).
        let mut phase2_manifest = StorageFormatManifest::new_redb_in_progress(
            7,
            Some("sqlite-backup/00000007".to_string()),
            Phase2Progress {
                git_db: Some(Phase2DbState::FencedReadyToUnlink),
                capture_db: Some(Phase2DbState::Pending),
                git_redb: None,
                cap_redb: None,
                spool_fenced: true,
                queue_fenced: false,
            },
            SourceDigests {
                git_logical_sha256: Some("aaa111".to_string()),
                git_backup_sha256: Some("bbb222".to_string()),
                cap_logical_sha256: Some("ccc333".to_string()),
                cap_backup_sha256: Some("ddd444".to_string()),
                git_redb_sha256: None,
                cap_redb_sha256: None,
                spool_manifest_sha256: Some("eee555".to_string()),
                queue_manifest_sha256: Some("fff666".to_string()),
            },
        )
        .expect("construct phase2 in-progress manifest");
        phase2_manifest.targets_staged_for_rename = true;
        phase2_manifest.target_migration_tag = Some(0x5242_0007);
        phase2_manifest.spool_unfenced = false;

        let ops_phase1 = RecordingPublishOps::new(None);
        let published_path =
            publish_storage_manifest_with_ops(&common_storage, &phase2_manifest, &ops_phase1)
                .expect("atomically publish finalized=false manifest");
        assert_eq!(published_path, storage_manifest_path(&common_storage));
        assert_eq!(
            ops_phase1.recorded(),
            vec![
                PublishStep::CheckDurabilitySupport,
                PublishStep::WriteTemp,
                PublishStep::FsyncTemp,
                PublishStep::RenameToFinal,
                PublishStep::FsyncDir,
            ],
            "Phase 2 in-progress publish must check durability support then follow .tmp -> fsync -> rename -> fsync_dir"
        );
        assert_eq!(
            count_tmp_files(&common_storage),
            0,
            "no leftover .tmp files after atomic publish"
        );

        let loaded_in_progress = read_storage_manifest(&common_storage)
            .expect("read in-progress manifest")
            .expect("manifest must exist on disk");
        assert_eq!(loaded_in_progress, phase2_manifest);
        assert!(!loaded_in_progress.finalized);
        assert_eq!(
            loaded_in_progress.phase2_progress.git_db,
            Some(Phase2DbState::FencedReadyToUnlink)
        );
        assert!(loaded_in_progress.phase2_progress.spool_fenced);
        assert!(loaded_in_progress.targets_staged_for_rename);
        assert_eq!(loaded_in_progress.target_migration_tag, Some(0x5242_0007));
        assert!(!loaded_in_progress.spool_unfenced);
        assert_eq!(
            loaded_in_progress
                .source_digests
                .git_logical_sha256
                .as_deref(),
            Some("aaa111")
        );

        // Verify raw serialized JSON on disk includes every required field name.
        let raw_json = fs::read_to_string(&published_path).expect("read raw manifest JSON");
        for required_key in [
            "\"finalized\": false",
            "\"phase2_progress\"",
            "\"targets_staged_for_rename\": true",
            "\"target_migration_tag\"",
            "\"spool_unfenced\": false",
            "\"source_digests\"",
        ] {
            assert!(
                raw_json.contains(required_key),
                "manifest JSON missing {required_key}: {raw_json}"
            );
        }

        // Unfinalized manifest MUST fail closed with `ManifestNotFinalized` (`LBR-IO-001`).
        let unfinalized_err = resolve_redb_generation_paths(&common_storage)
            .expect_err("finalized=false manifest must fail closed on path resolution");
        assert!(matches!(
            unfinalized_err,
            StorageBackendError::ManifestNotFinalized { .. }
        ));
        assert_eq!(unfinalized_err.stable_code_str(), "LBR-IO-001");

        // Finalization MUST reject unfinished forward migration progress
        // (`git_db = FencedReadyToUnlink`, `capture_db = Pending`, or `spool_fenced = false`).
        assert_eq!(
            phase2_manifest
                .clone()
                .into_finalized()
                .expect_err("cannot finalize forward migration with unfinished git_db/capture_db")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut unfinished_cap = phase2_manifest.clone();
        unfinished_cap.phase2_progress.git_db = Some(Phase2DbState::Unlinked);
        assert_eq!(
            unfinished_cap
                .clone()
                .into_finalized()
                .expect_err("cannot finalize forward migration while capture_db is Pending")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut unfinished_spool = unfinished_cap.clone();
        unfinished_spool.phase2_progress.capture_db = Some(Phase2DbState::Unlinked);
        unfinished_spool.phase2_progress.spool_fenced = false;
        assert_eq!(
            unfinished_spool
                .clone()
                .into_finalized()
                .expect_err("cannot finalize forward migration while spool_fenced is false")
                .stable_code_str(),
            "LBR-IO-001"
        );
        // Even a raw tampered JSON with finalized=true and incomplete progress must fail read validation.
        let mut tampered_final = unfinished_spool.clone();
        tampered_final.finalized = true;
        assert_eq!(
            tampered_final
                .validate()
                .expect_err("tampered finalized=true manifest with unfinished progress must fail")
                .stable_code_str(),
            "LBR-IO-001"
        );

        // Advance forward Phase 2 progress to terminal state (`unlinked` + `spool_fenced = true`)
        // before finalizing.
        let mut completed_forward = phase2_manifest.clone();
        completed_forward.phase2_progress.git_db = Some(Phase2DbState::Unlinked);
        completed_forward.phase2_progress.capture_db = Some(Phase2DbState::Unlinked);
        completed_forward.phase2_progress.spool_fenced = true;
        let finalized_candidate = completed_forward
            .into_finalized()
            .expect("completed forward migration finalizes cleanly");

        // Unsupported durability platform MUST fail closed BEFORE writing or renaming,
        // leaving zero temp files and preserving the prior manifest untouched.
        let before_failed_publish = fs::read(&published_path).expect("read manifest before fail");
        let unsupported_ops = RecordingPublishOps::new(Some(PublishStep::CheckDurabilitySupport));
        let unsupported_err = publish_storage_manifest_with_ops(
            &common_storage,
            &finalized_candidate,
            &unsupported_ops,
        )
        .expect_err("unsupported durability target must fail closed before publish");
        assert_eq!(unsupported_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            unsupported_ops.recorded(),
            vec![PublishStep::CheckDurabilitySupport],
            "unsupported durability target must abort before writing or renaming"
        );
        assert_eq!(count_tmp_files(&common_storage), 0);
        assert_eq!(
            fs::read(&published_path).expect("read manifest after unsupported-durability abort"),
            before_failed_publish,
            "unsupported durability abort must preserve prior manifest bytes verbatim"
        );

        // Failure during `fsync_temp` cleans up `.tmp`, skips `rename` and `fsync_dir`,
        // and leaves the previous `storage-format.json` completely intact.
        let failing_ops = RecordingPublishOps::new(Some(PublishStep::FsyncTemp));
        let fail_err =
            publish_storage_manifest_with_ops(&common_storage, &finalized_candidate, &failing_ops)
                .expect_err("fsync failure must abort publish");
        assert_eq!(fail_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            failing_ops.recorded(),
            vec![
                PublishStep::CheckDurabilitySupport,
                PublishStep::WriteTemp,
                PublishStep::FsyncTemp,
            ],
            "failed fsync must never proceed to rename or fsync_dir"
        );
        assert_eq!(
            count_tmp_files(&common_storage),
            0,
            "failed publish must clean up its .tmp file"
        );
        assert_eq!(
            fs::read(&published_path).expect("read manifest after failed publish"),
            before_failed_publish,
            "failed publish must leave prior manifest bytes untouched"
        );

        // AC2: Atomically publish `finalized = true` terminal manifest via
        // `.tmp` -> `fsync` -> `rename` -> `fsync_dir` (and verify public `publish_storage_manifest`).
        let ops_phase2 = RecordingPublishOps::new(None);
        publish_storage_manifest_with_ops(&common_storage, &finalized_candidate, &ops_phase2)
            .expect("atomically publish finalized=true manifest");
        assert_eq!(
            ops_phase2.recorded(),
            vec![
                PublishStep::CheckDurabilitySupport,
                PublishStep::WriteTemp,
                PublishStep::FsyncTemp,
                PublishStep::RenameToFinal,
                PublishStep::FsyncDir,
            ],
            "finalized=true publish must check durability then follow .tmp -> fsync -> rename -> fsync_dir"
        );
        assert_eq!(count_tmp_files(&common_storage), 0);

        // Also exercise the public `publish_storage_manifest` entry directly for generation 42.
        let gen42_manifest = StorageFormatManifest::new_redb_finalized(42)
            .expect("construct finalized gen 42 manifest");
        publish_storage_manifest(&common_storage, &gen42_manifest)
            .expect("public publish_storage_manifest succeeds");
        assert_eq!(count_tmp_files(&common_storage), 0);

        // AC3 & AC4: Resolver returns exact generation-scoped paths
        // `.libra/redb/git.gen-00000042.redb` and `.libra/private/capture.gen-00000042.redb`.
        let resolved = resolve_redb_generation_paths(&common_storage)
            .expect("resolve finalized redb manifest")
            .expect("finalized redb manifest must yield paths");
        assert_eq!(resolved.generation, 42);
        assert_eq!(
            resolved.git_db_path,
            common_storage.join("redb/git.gen-00000042.redb")
        );
        assert_eq!(
            resolved.capture_db_path,
            common_storage.join("private/capture.gen-00000042.redb")
        );

        let full_layout = resolve_storage_layout(&common_storage)
            .expect("resolve full storage layout")
            .expect("layout present");
        assert_eq!(full_layout.backend, StorageBackendKind::Redb);
        assert_eq!(full_layout.generation, 42);
        assert_eq!(full_layout.git_db_path, resolved.git_db_path);
        assert_eq!(full_layout.capture_db_path, resolved.capture_db_path);

        // Finalized SQLite manifest resolves via `resolve_storage_layout` to
        // `libra.db` / `private/capture.db` and returns `None` from `resolve_redb_generation_paths`.
        let mut sqlite_manifest_in_progress = StorageFormatManifest::new_sqlite_in_progress(
            43,
            0x5351_0043,
            Some("redb-backup/00000043".to_string()),
            Phase2Progress {
                git_redb: Some(Phase2DbState::Unlinked),
                cap_redb: Some(Phase2DbState::Unlinked),
                queue_fenced: true,
                spool_fenced: true,
                ..Phase2Progress::default()
            },
            SourceDigests {
                git_redb_sha256: Some("111aaa".to_string()),
                cap_redb_sha256: Some("222bbb".to_string()),
                spool_manifest_sha256: Some("333ccc".to_string()),
                queue_manifest_sha256: Some("444ddd".to_string()),
                ..SourceDigests::default()
            },
        )
        .expect("construct sqlite reverse manifest");
        sqlite_manifest_in_progress.targets_staged_for_rename = true;
        let sqlite_manifest = sqlite_manifest_in_progress
            .into_finalized()
            .expect("finalize sqlite manifest");
        publish_storage_manifest(&common_storage, &sqlite_manifest)
            .expect("publish finalized sqlite manifest");
        assert!(
            resolve_redb_generation_paths(&common_storage)
                .expect("sqlite manifest returns None for redb paths")
                .is_none()
        );
        let sqlite_layout = resolve_storage_layout(&common_storage)
            .expect("resolve sqlite layout")
            .expect("sqlite layout present");
        assert_eq!(sqlite_layout.backend, StorageBackendKind::Sqlite);
        assert_eq!(sqlite_layout.git_db_path, common_storage.join("libra.db"));
        assert_eq!(
            sqlite_layout.capture_db_path,
            common_storage.join("private/capture.db")
        );

        // Validation guards: reject generation 0, path traversal, mismatched generation paths,
        // and future schema_version.
        assert_eq!(
            StorageFormatManifest::new_redb_finalized(0)
                .expect_err("generation 0 must be rejected")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut traversal = StorageFormatManifest::new_redb_finalized(1).expect("valid gen 1");
        traversal.git_path = Some("../escape.redb".to_string());
        assert_eq!(
            traversal
                .validate()
                .expect_err("path traversal must be rejected")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut wrong_gen = StorageFormatManifest::new_redb_finalized(1).expect("valid gen 1");
        wrong_gen.capture_path = Some("private/capture.gen-00000009.redb".to_string());
        assert_eq!(
            wrong_gen
                .validate()
                .expect_err("mismatched generation path must be rejected")
                .stable_code_str(),
            "LBR-IO-001"
        );

        let future_dir = tempdir().expect("future tempdir");
        fs::write(
            storage_manifest_path(future_dir.path()),
            r#"{"schema_version": 99, "backend": "redb", "future_only_shape": []}"#,
        )
        .expect("write future schema manifest");
        let future_err = read_storage_manifest(future_dir.path())
            .expect_err("future schema_version must fail closed");
        assert!(matches!(
            future_err,
            StorageBackendError::SchemaTooNew {
                role: DatabaseRole::Repository,
                found: 99,
                max_supported: STORAGE_FORMAT_MANIFEST_VERSION,
            }
        ));
        assert_eq!(future_err.stable_code_str(), "LBR-IO-001");

        // Reject symlinked manifest files on Unix.
        #[cfg(unix)]
        {
            let sym_dir = tempdir().expect("symlink tempdir");
            let real_target = sym_dir.path().join("real.json");
            fs::write(&real_target, b"{}").expect("write symlink target");
            std::os::unix::fs::symlink(&real_target, storage_manifest_path(sym_dir.path()))
                .expect("create manifest symlink");
            let sym_read_err = read_storage_manifest(sym_dir.path())
                .expect_err("symlinked storage-format.json must be rejected on read");
            assert_eq!(sym_read_err.stable_code_str(), "LBR-IO-001");
            let sym_pub_err = publish_storage_manifest(sym_dir.path(), &gen42_manifest)
                .expect_err("symlinked storage-format.json must be rejected on publish");
            assert_eq!(sym_pub_err.stable_code_str(), "LBR-IO-001");
        }

        // Strict deserialization: missing OR explicit-null common recovery fields
        // and fencing flags MUST fail closed for BOTH forward (`redb`) and reverse
        // (`sqlite`) manifests instead of silently defaulting.
        let missing_dir = tempdir().expect("missing-fields tempdir");
        let reverse_valid = StorageFormatManifest::new_sqlite_in_progress(
            44,
            0x5351_0044,
            Some("redb-backup/00000044".to_string()),
            Phase2Progress {
                git_redb: Some(Phase2DbState::Pending),
                cap_redb: Some(Phase2DbState::Pending),
                spool_fenced: true,
                queue_fenced: true,
                ..Phase2Progress::default()
            },
            SourceDigests {
                git_redb_sha256: Some("abc".to_string()),
                cap_redb_sha256: Some("def".to_string()),
                spool_manifest_sha256: Some("ghi".to_string()),
                queue_manifest_sha256: Some("jkl".to_string()),
                ..SourceDigests::default()
            },
        )
        .expect("valid in-progress reverse sqlite manifest");

        let forward_json = serde_json::to_value(&phase2_manifest).expect("serialize forward");
        let reverse_json = serde_json::to_value(&reverse_valid).expect("serialize reverse");

        for (dir_label, base_json) in [("forward", &forward_json), ("reverse", &reverse_json)] {
            for common_key in [
                "origin",
                "phase2_progress",
                "targets_staged_for_rename",
                "spool_unfenced",
                "source_digests",
            ] {
                for mode in ["missing", "null"] {
                    let mut mutated = base_json.clone();
                    let obj = mutated.as_object_mut().expect("manifest JSON object");
                    if mode == "missing" {
                        obj.remove(common_key);
                    } else {
                        obj.insert(common_key.to_string(), serde_json::Value::Null);
                    }
                    fs::write(
                        storage_manifest_path(missing_dir.path()),
                        serde_json::to_vec(&mutated).expect("encode mutated common JSON"),
                    )
                    .expect("write mutated common manifest");
                    let err = read_storage_manifest(missing_dir.path())
                        .expect_err("manifest with missing/null common field must fail closed");
                    assert_eq!(
                        err.stable_code_str(),
                        "LBR-IO-001",
                        "{dir_label} common `{common_key}` ({mode}) must fail closed"
                    );
                }
            }
            for flag_key in ["spool_fenced", "queue_fenced"] {
                for mode in ["missing", "null"] {
                    let mut mutated = base_json.clone();
                    let obj = mutated["phase2_progress"]
                        .as_object_mut()
                        .expect("phase2_progress object");
                    if mode == "missing" {
                        obj.remove(flag_key);
                    } else {
                        obj.insert(flag_key.to_string(), serde_json::Value::Null);
                    }
                    fs::write(
                        storage_manifest_path(missing_dir.path()),
                        serde_json::to_vec(&mutated).expect("encode mutated phase2_progress"),
                    )
                    .expect("write mutated phase2_progress manifest");
                    let err = read_storage_manifest(missing_dir.path())
                        .expect_err("phase2_progress missing/null flag must fail closed");
                    assert_eq!(
                        err.stable_code_str(),
                        "LBR-IO-001",
                        "{dir_label} phase2_progress.`{flag_key}` ({mode}) must fail closed"
                    );
                }
            }
        }

        // Exhaustive direction-specific recovery invariants for `finalized = false`
        // forward (`redb`) manifests: test missing AND explicit-null JSON as well as
        // empty digests and opposite-direction fields for EVERY required field.
        for (parent_key, field_key) in [
            (None, "backup_dir"),
            (Some("phase2_progress"), "git_db"),
            (Some("phase2_progress"), "capture_db"),
            (Some("source_digests"), "git_logical_sha256"),
            (Some("source_digests"), "git_backup_sha256"),
            (Some("source_digests"), "cap_logical_sha256"),
            (Some("source_digests"), "cap_backup_sha256"),
            (Some("source_digests"), "spool_manifest_sha256"),
            (Some("source_digests"), "queue_manifest_sha256"),
        ] {
            for mode in ["missing", "null"] {
                let mut mutated = forward_json.clone();
                let target_obj = match parent_key {
                    Some(k) => mutated[k].as_object_mut().expect("nested object"),
                    None => mutated.as_object_mut().expect("root object"),
                };
                if mode == "missing" {
                    target_obj.remove(field_key);
                } else {
                    target_obj.insert(field_key.to_string(), serde_json::Value::Null);
                }
                fs::write(
                    storage_manifest_path(missing_dir.path()),
                    serde_json::to_vec(&mutated).expect("encode mutated forward manifest"),
                )
                .expect("write mutated forward manifest");
                let err = read_storage_manifest(missing_dir.path())
                    .expect_err("mutated forward manifest must fail closed");
                assert_eq!(
                    err.stable_code_str(),
                    "LBR-IO-001",
                    "forward `{field_key}` ({mode}) must fail closed"
                );
            }
        }
        // Empty digest string fails closed.
        let mut empty_digest = phase2_manifest.clone();
        empty_digest.source_digests.spool_manifest_sha256 = Some("   ".to_string());
        assert_eq!(
            empty_digest
                .validate()
                .expect_err("whitespace-only digest must fail closed")
                .stable_code_str(),
            "LBR-IO-001"
        );
        // Opposite-direction reverse fields inside forward manifest fail closed.
        let mut forward_mixed = phase2_manifest.clone();
        forward_mixed.phase2_progress.git_redb = Some(Phase2DbState::Pending);
        assert_eq!(
            forward_mixed
                .validate()
                .expect_err("forward manifest with reverse git_redb must fail closed")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut forward_mixed_cap = phase2_manifest.clone();
        forward_mixed_cap.phase2_progress.cap_redb = Some(Phase2DbState::Pending);
        assert_eq!(
            forward_mixed_cap
                .validate()
                .expect_err("forward manifest with reverse cap_redb must fail closed")
                .stable_code_str(),
            "LBR-IO-001"
        );
        // When `capture_db == Absent` (pre-split repo), absent capture digests are allowed,
        // while spool/queue digests remain mandatory.
        let mut absent_cap_ok = phase2_manifest.clone();
        absent_cap_ok.phase2_progress.capture_db = Some(Phase2DbState::Absent);
        absent_cap_ok.source_digests.cap_logical_sha256 = None;
        absent_cap_ok.source_digests.cap_backup_sha256 = None;
        absent_cap_ok
            .validate()
            .expect("in-progress redb manifest with capture_db=Absent may omit capture digests");
        let mut absent_cap_no_queue = absent_cap_ok.clone();
        absent_cap_no_queue.source_digests.queue_manifest_sha256 = None;
        assert_eq!(
            absent_cap_no_queue
                .validate()
                .expect_err("capture_db=Absent still requires queue_manifest_sha256")
                .stable_code_str(),
            "LBR-IO-001"
        );
        // Finalization of forward manifest with `capture_db = Absent` succeeds once `git_db = Unlinked` + `spool_fenced = true`.
        absent_cap_ok.phase2_progress.git_db = Some(Phase2DbState::Unlinked);
        absent_cap_ok.phase2_progress.spool_fenced = true;
        absent_cap_ok
            .into_finalized()
            .expect("forward manifest with capture_db=Absent finalizes cleanly");

        // Exhaustive direction-specific recovery invariants for `finalized = false`
        // reverse (`sqlite`) manifests.
        for (parent_key, field_key) in [
            (None, "target_migration_tag"),
            (None, "backup_dir"),
            (Some("phase2_progress"), "git_redb"),
            (Some("phase2_progress"), "cap_redb"),
            (Some("source_digests"), "git_redb_sha256"),
            (Some("source_digests"), "cap_redb_sha256"),
            (Some("source_digests"), "spool_manifest_sha256"),
            (Some("source_digests"), "queue_manifest_sha256"),
        ] {
            for mode in ["missing", "null"] {
                let mut mutated = reverse_json.clone();
                let target_obj = match parent_key {
                    Some(k) => mutated[k].as_object_mut().expect("nested object"),
                    None => mutated.as_object_mut().expect("root object"),
                };
                if mode == "missing" {
                    target_obj.remove(field_key);
                } else {
                    target_obj.insert(field_key.to_string(), serde_json::Value::Null);
                }
                fs::write(
                    storage_manifest_path(missing_dir.path()),
                    serde_json::to_vec(&mutated).expect("encode mutated reverse manifest"),
                )
                .expect("write mutated reverse manifest");
                let err = read_storage_manifest(missing_dir.path())
                    .expect_err("mutated reverse manifest must fail closed");
                assert_eq!(
                    err.stable_code_str(),
                    "LBR-IO-001",
                    "reverse `{field_key}` ({mode}) must fail closed"
                );
            }
        }
        // `target_migration_tag == 0`, `spool_unfenced == true` before finalize,
        // and opposite-direction forward fields in reverse manifest all fail closed.
        let mut reverse_zero_tag = reverse_valid.clone();
        reverse_zero_tag.target_migration_tag = Some(0);
        assert_eq!(
            reverse_zero_tag
                .validate()
                .expect_err("target_migration_tag=0 must fail closed")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut reverse_unfenced = reverse_valid.clone();
        reverse_unfenced.spool_unfenced = true;
        assert_eq!(
            reverse_unfenced
                .validate()
                .expect_err("in-progress reverse sqlite manifest must reject spool_unfenced=true")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut reverse_mixed_git = reverse_valid.clone();
        reverse_mixed_git.phase2_progress.git_db = Some(Phase2DbState::Pending);
        assert_eq!(
            reverse_mixed_git
                .validate()
                .expect_err("reverse manifest with forward git_db must fail closed")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut reverse_mixed_cap = reverse_valid.clone();
        reverse_mixed_cap.phase2_progress.capture_db = Some(Phase2DbState::Pending);
        assert_eq!(
            reverse_mixed_cap
                .validate()
                .expect_err("reverse manifest with forward capture_db must fail closed")
                .stable_code_str(),
            "LBR-IO-001"
        );

        // Finalization MUST reject unfinished reverse migration progress.
        assert_eq!(
            reverse_valid
                .clone()
                .into_finalized()
                .expect_err("cannot finalize reverse migration while git_redb/cap_redb are Pending")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut rev_half = reverse_valid.clone();
        rev_half.phase2_progress.git_redb = Some(Phase2DbState::Unlinked);
        assert_eq!(
            rev_half
                .clone()
                .into_finalized()
                .expect_err("cannot finalize reverse migration while cap_redb is Pending")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut rev_no_queue_fence = rev_half.clone();
        rev_no_queue_fence.phase2_progress.cap_redb = Some(Phase2DbState::Unlinked);
        rev_no_queue_fence.phase2_progress.queue_fenced = false;
        assert_eq!(
            rev_no_queue_fence
                .clone()
                .into_finalized()
                .expect_err("cannot finalize reverse migration while queue_fenced is false")
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut rev_no_spool_fence = rev_no_queue_fence;
        rev_no_spool_fence.phase2_progress.queue_fenced = true;
        rev_no_spool_fence.phase2_progress.spool_fenced = false;
        assert_eq!(
            rev_no_spool_fence
                .clone()
                .into_finalized()
                .expect_err("cannot finalize reverse migration while spool_fenced is false")
                .stable_code_str(),
            "LBR-IO-001"
        );

        // Reverse staging invariants (`ADR-RB-07` #2 Step 2.4 -> Step 2.5):
        // (1) `targets_staged_for_rename = true` is forbidden on read AND publish
        //     whenever either redb source is in ANY non-`Unlinked` state (`Pending`,
        //     `FencedReadyToUnlink`, `Absent`) or either queue/spool fence is false.
        let staging_dir = tempdir().expect("reverse staging tempdir");
        let mut bad_staged_cases: Vec<(String, StorageFormatManifest)> = Vec::new();
        for non_terminal in [
            Phase2DbState::Pending,
            Phase2DbState::FencedReadyToUnlink,
            Phase2DbState::Absent,
        ] {
            let mut bad_git = reverse_valid.clone();
            bad_git.phase2_progress.git_redb = Some(non_terminal);
            bad_git.phase2_progress.cap_redb = Some(Phase2DbState::Unlinked);
            bad_git.phase2_progress.queue_fenced = true;
            bad_git.phase2_progress.spool_fenced = true;
            bad_git.targets_staged_for_rename = true;
            bad_staged_cases.push((format!("git_redb={non_terminal:?}"), bad_git));

            let mut bad_cap = reverse_valid.clone();
            bad_cap.phase2_progress.git_redb = Some(Phase2DbState::Unlinked);
            bad_cap.phase2_progress.cap_redb = Some(non_terminal);
            bad_cap.phase2_progress.queue_fenced = true;
            bad_cap.phase2_progress.spool_fenced = true;
            bad_cap.targets_staged_for_rename = true;
            bad_staged_cases.push((format!("cap_redb={non_terminal:?}"), bad_cap));
        }
        {
            let mut m = reverse_valid.clone();
            m.phase2_progress.git_redb = Some(Phase2DbState::Unlinked);
            m.phase2_progress.cap_redb = Some(Phase2DbState::Unlinked);
            m.phase2_progress.queue_fenced = false;
            m.phase2_progress.spool_fenced = true;
            m.targets_staged_for_rename = true;
            bad_staged_cases.push(("queue_fenced=false".to_string(), m));
        }
        {
            let mut m = reverse_valid.clone();
            m.phase2_progress.git_redb = Some(Phase2DbState::Unlinked);
            m.phase2_progress.cap_redb = Some(Phase2DbState::Unlinked);
            m.phase2_progress.queue_fenced = true;
            m.phase2_progress.spool_fenced = false;
            m.targets_staged_for_rename = true;
            bad_staged_cases.push(("spool_fenced=false".to_string(), m));
        }
        for (label, bad_staged) in &bad_staged_cases {
            assert_eq!(
                publish_storage_manifest(staging_dir.path(), bad_staged)
                    .expect_err("publishing impossible reverse staging state must fail closed")
                    .stable_code_str(),
                "LBR-IO-001",
                "publish with targets_staged_for_rename=true while {label} must fail closed"
            );
            fs::write(
                storage_manifest_path(staging_dir.path()),
                serde_json::to_vec(bad_staged).expect("serialize bad staged manifest"),
            )
            .expect("write bad staged manifest");
            assert_eq!(
                read_storage_manifest(staging_dir.path())
                    .expect_err("reading impossible reverse staging state must fail closed")
                    .stable_code_str(),
                "LBR-IO-001",
                "read with targets_staged_for_rename=true while {label} must fail closed"
            );
        }

        // (2) Finalizing a reverse migration while `targets_staged_for_rename = false`
        //     (even with both sources unlinked and both fences set) MUST fail closed
        //     on `into_finalized()`, `publish_storage_manifest()`, and `read_storage_manifest()`.
        let mut rev_unstaged = reverse_valid.clone();
        rev_unstaged.phase2_progress.git_redb = Some(Phase2DbState::Unlinked);
        rev_unstaged.phase2_progress.cap_redb = Some(Phase2DbState::Unlinked);
        rev_unstaged.phase2_progress.queue_fenced = true;
        rev_unstaged.phase2_progress.spool_fenced = true;
        rev_unstaged.targets_staged_for_rename = false;
        assert_eq!(
            rev_unstaged
                .clone()
                .into_finalized()
                .expect_err(
                    "cannot finalize reverse migration while targets_staged_for_rename=false"
                )
                .stable_code_str(),
            "LBR-IO-001"
        );
        let mut rev_final_unstaged = rev_unstaged.clone();
        rev_final_unstaged.finalized = true;
        assert_eq!(
            publish_storage_manifest(staging_dir.path(), &rev_final_unstaged)
                .expect_err("publishing finalized reverse manifest without targets_staged_for_rename must fail")
                .stable_code_str(),
            "LBR-IO-001"
        );
        fs::write(
            storage_manifest_path(staging_dir.path()),
            serde_json::to_vec(&rev_final_unstaged).expect("serialize unstaged finalized reverse"),
        )
        .expect("write unstaged finalized reverse");
        assert_eq!(
            read_storage_manifest(staging_dir.path())
                .expect_err(
                    "reading finalized reverse manifest without targets_staged_for_rename must fail"
                )
                .stable_code_str(),
            "LBR-IO-001"
        );
        // Setting `targets_staged_for_rename = true` allows clean finalization and publish.
        rev_unstaged.targets_staged_for_rename = true;
        let rev_final_ok = rev_unstaged
            .into_finalized()
            .expect("staged terminal reverse manifest finalizes cleanly");
        publish_storage_manifest(staging_dir.path(), &rev_final_ok)
            .expect("staged terminal reverse manifest publishes cleanly");

        // Tampered `finalized = true` JSON read from disk MUST fail closed for
        // BOTH forward and reverse manifests:
        // (a) unfinished progress fields,
        // (b) BOTH relevant DB progress fields omitted or explicitly set to `null`
        //     (prevents bypassing terminal checks), and
        // (c) tampering `origin` to `fresh_init` while migration metadata remains
        //     or when `backend = sqlite`.
        for (dir_label, base_json, progress_pair) in [
            ("forward", &forward_json, ("git_db", "capture_db")),
            ("reverse", &reverse_json, ("git_redb", "cap_redb")),
        ] {
            // (a) Flip `finalized` to `true` on an unfinished manifest JSON.
            let mut tampered_unfinished = base_json.clone();
            tampered_unfinished["finalized"] = serde_json::Value::Bool(true);
            fs::write(
                storage_manifest_path(missing_dir.path()),
                serde_json::to_vec(&tampered_unfinished).expect("encode tampered unfinished JSON"),
            )
            .expect("write tampered unfinished JSON");
            assert_eq!(
                read_storage_manifest(missing_dir.path())
                    .expect_err("tampered finalized=true unfinished JSON must fail closed")
                    .stable_code_str(),
                "LBR-IO-001",
                "{dir_label} tampered finalized=true unfinished JSON must fail closed"
            );

            // (b) Flip `finalized = true` AND remove/null BOTH DB progress fields.
            for mode in ["missing", "null"] {
                let mut bypassed = base_json.clone();
                bypassed["finalized"] = serde_json::Value::Bool(true);
                let p2 = bypassed["phase2_progress"]
                    .as_object_mut()
                    .expect("phase2_progress object");
                for key in [progress_pair.0, progress_pair.1] {
                    if mode == "missing" {
                        p2.remove(key);
                    } else {
                        p2.insert(key.to_string(), serde_json::Value::Null);
                    }
                }
                fs::write(
                    storage_manifest_path(missing_dir.path()),
                    serde_json::to_vec(&bypassed).expect("encode bypassed progress JSON"),
                )
                .expect("write bypassed progress JSON");
                assert_eq!(
                    read_storage_manifest(missing_dir.path())
                        .expect_err("finalized migration manifest with omitted/null DB progress must fail closed")
                        .stable_code_str(),
                    "LBR-IO-001",
                    "{dir_label} finalized=true with both DB progress fields {mode} must fail closed"
                );
            }

            // (c) Attempt to disguise a migration manifest as `origin = "fresh_init"`.
            let mut disguised = base_json.clone();
            disguised["finalized"] = serde_json::Value::Bool(true);
            disguised["origin"] = serde_json::Value::String("fresh_init".to_string());
            fs::write(
                storage_manifest_path(missing_dir.path()),
                serde_json::to_vec(&disguised).expect("encode disguised fresh_init JSON"),
            )
            .expect("write disguised fresh_init JSON");
            assert_eq!(
                read_storage_manifest(missing_dir.path())
                    .expect_err("migration manifest disguised as fresh_init must fail closed")
                    .stable_code_str(),
                "LBR-IO-001",
                "{dir_label} migration manifest disguised as fresh_init must fail closed"
            );
        }

        // Even a clean `sqlite` manifest with zero migration metadata cannot claim `fresh_init`.
        let mut sqlite_fresh = serde_json::to_value(&gen42_manifest).expect("serialize gen42");
        sqlite_fresh["backend"] = serde_json::Value::String("sqlite".to_string());
        sqlite_fresh["git_path"] = serde_json::Value::String("libra.db".to_string());
        sqlite_fresh["capture_path"] = serde_json::Value::String("private/capture.db".to_string());
        fs::write(
            storage_manifest_path(missing_dir.path()),
            serde_json::to_vec(&sqlite_fresh).expect("encode sqlite fresh_init JSON"),
        )
        .expect("write sqlite fresh_init JSON");
        assert_eq!(
            read_storage_manifest(missing_dir.path())
                .expect_err("sqlite manifest claiming fresh_init must fail closed")
                .stable_code_str(),
            "LBR-IO-001"
        );

        // Temp filename collision (e.g., PID reuse) retries with a fresh sequence
        // and NEVER deletes the pre-existing colliding file.
        let collision_dir = tempdir().expect("collision tempdir");
        let next_seq = TEMP_SEQ.load(Ordering::Relaxed);
        let colliding_path = collision_dir.path().join(format!(
            "{STORAGE_FORMAT_MANIFEST_FILE}.{}.{}.tmp",
            std::process::id(),
            next_seq
        ));
        fs::write(&colliding_path, b"pre-existing-do-not-delete")
            .expect("seed pre-existing colliding temp file");
        publish_storage_manifest(collision_dir.path(), &gen42_manifest)
            .expect("publish must retry past colliding temp filename");
        assert_eq!(
            fs::read(&colliding_path).expect("colliding temp file must remain untouched"),
            b"pre-existing-do-not-delete"
        );
        assert_eq!(
            read_storage_manifest(collision_dir.path())
                .expect("read manifest after collision retry")
                .expect("manifest exists"),
            gen42_manifest
        );

        // Platform `fsync_dir` fails closed on a missing directory.
        let missing_fsync_err = fsync_dir(&dir.path().join("does-not-exist-dir"))
            .expect_err("fsync_dir on nonexistent directory must fail closed");
        assert_eq!(missing_fsync_err.stable_code_str(), "LBR-IO-001");
    }
}
