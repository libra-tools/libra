//! Secret-free schema diagnosis. No configuration-value or mutation API belongs here.

use std::{
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::TransactionTrait;
use serde::Serialize;

use crate::{
    internal::db::{
        DatabaseRole, SchemaCompatibility,
        schema::{self, ConfigurationSchemaInspection, ConfigurationSchemaIssueKind, SchemaLedger},
    },
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
    },
};

const ROLE: DatabaseRole = DatabaseRole::GlobalConfig;
const UNREADABLE_HINT: &str = "Cannot safely inspect this file. Check the configured path and permissions; use a SQLite-consistent snapshot if necessary. Do not edit migration receipts manually.";
const LEGACY_MIGRATION_HINT: &str = "Global configuration is still at the legacy path; the next command that reads or writes it moves it to the XDG config directory automatically. The legacy file is kept as a downgrade backup and can be removed once the new path is in use.";
const LEGACY_BACKUP_HINT: &str = "The legacy global configuration file is still present next to the active one. It is an unused backup kept for downgrades; Libra never reads or writes it once the new path exists, and you can delete it when you no longer need to downgrade.";

#[derive(Serialize)]
struct LedgerReport {
    ledger: &'static str,
    observed_version: Option<String>,
    latest_version: Option<String>,
    readable: bool,
    verified_name: Option<&'static str>,
}

impl LedgerReport {
    fn new(role: DatabaseRole, ledger: SchemaLedger) -> CliResult<Self> {
        let latest = schema::latest_schema_version_for_role(role).map_err(|_| {
            CliError::internal("cannot load the built-in schema manifest; reinstall Libra")
        })?;
        Ok(Self {
            ledger: ledger.table_name(),
            observed_version: None,
            latest_version: latest.map(|version| version.to_string()),
            readable: false,
            verified_name: None,
        })
    }
}

#[derive(Serialize)]
struct IssueReport {
    ledger: &'static str,
    version: String,
    reason: &'static str,
}

#[derive(Serialize)]
struct Report {
    report_version: u8,
    action: &'static str,
    scope: &'static str,
    role: &'static str,
    path_source: &'static str,
    configured_path: Option<String>,
    canonical_path: Option<String>,
    exists: Option<bool>,
    size_bytes: Option<u64>,
    modified_at_utc: Option<String>,
    legacy_path: Option<String>,
    legacy_exists: Option<bool>,
    migration_pending: bool,
    configuration: LedgerReport,
    legacy: LedgerReport,
    classification: &'static str,
    issue: Option<IssueReport>,
    producer_disposition: &'static str,
    repair_eligible: bool,
    hints: Vec<&'static str>,
}

impl Report {
    fn new(
        resolution: Option<&crate::internal::config::GlobalConfigResolution>,
    ) -> CliResult<Self> {
        Ok(Self {
            report_version: 1,
            action: "doctor",
            scope: "global",
            role: "global_config",
            path_source: resolution
                .map(|resolution| resolution.source.as_str())
                .unwrap_or("home"),
            configured_path: resolution
                .map(|resolution| resolution.path.to_string_lossy().into_owned()),
            canonical_path: None,
            exists: None,
            size_bytes: None,
            modified_at_utc: None,
            legacy_path: resolution
                .and_then(|resolution| resolution.legacy_path.as_ref())
                .map(|path| path.to_string_lossy().into_owned()),
            legacy_exists: resolution.map(|resolution| resolution.legacy_exists),
            migration_pending: resolution.is_some_and(|resolution| resolution.migration_pending),
            configuration: LedgerReport::new(ROLE, SchemaLedger::Configuration)?,
            legacy: LedgerReport::new(DatabaseRole::Repository, SchemaLedger::Repository)?,
            classification: "unreadable",
            issue: None,
            producer_disposition: "unattributed",
            repair_eligible: false,
            hints: vec![UNREADABLE_HINT],
        })
    }

    fn record_inspection(
        &mut self,
        inspection: ConfigurationSchemaInspection,
        legacy: Option<i64>,
        legacy_readable: bool,
    ) {
        let (current, classification) = match inspection.compatibility {
            SchemaCompatibility::Compatible {
                current_version, ..
            } => (current_version, "compatible"),
            SchemaCompatibility::UpgradeRequired {
                current_version, ..
            } => (current_version, "upgrade_required"),
            SchemaCompatibility::UnsupportedFuture {
                current_version, ..
            } => (Some(current_version), "unsupported_future"),
        };
        self.configuration.observed_version = current.map(|version| version.to_string());
        self.configuration.readable = true;
        self.legacy.observed_version = legacy.map(|version| version.to_string());
        self.legacy.readable = legacy_readable;
        self.classification = classification;
        self.hints = vec![
            "Schema metadata and mtime do not attest a producer. Explicit --repair --confirm requires separate format, path and backup checks; do not edit SQLite receipts manually.",
        ];
        if let Some(issue) = inspection.issue {
            self.classification = match issue.kind {
                ConfigurationSchemaIssueKind::Future => "unsupported_future",
                ConfigurationSchemaIssueKind::UnregisteredReceipt => "unsupported_receipt",
            };
            self.issue = Some(IssueReport {
                ledger: issue.ledger.table_name(),
                version: issue.current_version.to_string(),
                reason: issue.reason(),
            });
            self.hints.push("Install a producer-compatible newer Libra build before using unsupported defaults.");
        } else if !legacy_readable {
            self.classification = "unreadable";
        } else {
            // Names come only from the trusted manifest after the centralized
            // classifier has validated every receipt, never from database text.
            self.configuration.verified_name = registered_name(ROLE, current);
            self.legacy.verified_name = registered_name(DatabaseRole::Repository, legacy);
            self.producer_disposition = if inspection.barrier_present {
                self.legacy.verified_name =
                    Some(schema::schema_manifest().configuration_barrier.name);
                "configuration_barrier_unattested"
            } else if legacy.is_some() {
                "known_repository_receipt_unattested"
            } else if inspection.base_receipt_present {
                "configuration_receipt_unattested"
            } else {
                "unattributed"
            };
        }
        if !legacy_readable {
            self.hints.push("Legacy ledger metadata is unreadable; no legacy receipt or producer can be inferred.");
        }
    }

    fn changed(&mut self) {
        self.classification = "changed_during_inspection";
        self.producer_disposition = "unattributed";
        self.configuration.verified_name = None;
        self.legacy.verified_name = None;
        // A pre-change issue is not a diagnosis of the newly observed file.
        self.issue = None;
        self.hints = vec![
            "The target changed during inspection. Stop concurrent writers or inspect a SQLite-consistent snapshot; do not infer repair eligibility from this report.",
        ];
    }
}

fn registered_name(role: DatabaseRole, version: Option<i64>) -> Option<&'static str> {
    schema::migrations_for_role(role)
        .into_iter()
        .find(|migration| Some(migration.version) == version)
        .map(|migration| migration.name)
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileStamp {
    fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        })
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn regular_file_stamp(path: &Path) -> io::Result<Option<FileStamp>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => FileStamp::from_metadata(&metadata).map(Some),
        Ok(_) => Err(io::Error::other("target is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn utc_time(time: SystemTime) -> Option<String> {
    let nanos = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::try_from(duration.as_nanos()).ok()?,
        Err(error) => -i128::try_from(error.duration().as_nanos()).ok()?,
    };
    let seconds = i64::try_from(nanos.div_euclid(1_000_000_000)).ok()?;
    let subsecond = u32::try_from(nanos.rem_euclid(1_000_000_000)).ok()?;
    DateTime::<Utc>::from_timestamp(seconds, subsecond)
        .map(|date| date.to_rfc3339_opts(SecondsFormat::AutoSi, true))
}

/// Only the fixed SQLite header is read outside SQLite. Missing sidecars must
/// not be created by a diagnostic; `immutable` is unsafe for a changing file.
fn safe_to_open(path: &Path, wal: &Option<FileStamp>) -> io::Result<bool> {
    let mut header = [0_u8; 100];
    File::open(path)?.read_exact(&mut header)?;
    if &header[..16] != b"SQLite format 3\0" {
        return Ok(false);
    }
    if header[18] == 2 && header[19] == 2 {
        return Ok(wal.is_some() && regular_file_stamp(&sidecar(path, "-shm"))?.is_some());
    }
    Ok(header[18] == 1 && header[19] == 1)
}

async fn inspect_snapshot(path: &Path, report: &mut Report) -> io::Result<()> {
    use crate::storage::{
        StorageBackendKind,
        config_store::{ConfigScopeAuthority, inspect_config_scope_authority},
    };

    let backend = match path
        .parent()
        .and_then(|dir| inspect_config_scope_authority(dir, ROLE).ok())
    {
        Some(ConfigScopeAuthority::Manifest { manifest, .. }) => manifest.backend,
        Some(ConfigScopeAuthority::UnmanifestedRedb { .. }) => StorageBackendKind::Redb,
        _ => StorageBackendKind::Sqlite,
    };

    match backend {
        StorageBackendKind::Redb => {
            if let Some(scope_dir) = path.parent() {
                let _ = inspect_redb_scope(scope_dir, report).await?;
            }
            Ok(())
        }
        StorageBackendKind::Sqlite => {
            let connection =
                schema::open_readonly_connection_for_role(path, Duration::from_millis(200), ROLE)
                    .await?;
            let result: io::Result<_> = async {
                let transaction = connection.begin().await.map_err(io::Error::other)?;
                let inspection = schema::inspect_configuration_schema(&transaction, ROLE).await?;
                let legacy =
                    schema::current_schema_version_for_role(&transaction, DatabaseRole::Repository)
                        .await;
                // A proven configuration future must not be hidden by a malformed
                // legacy ledger. The report marks that auxiliary metadata unavailable.
                let legacy_readable = legacy.is_ok();
                let legacy = legacy.ok().flatten();
                transaction.rollback().await.map_err(io::Error::other)?;
                Ok((inspection, legacy, legacy_readable))
            }
            .await;
            let closed = connection.close().await.map_err(io::Error::other);
            let (inspection, legacy, legacy_readable) = result?;
            closed?;
            report.record_inspection(inspection, legacy, legacy_readable);
            Ok(())
        }
    }
}

async fn inspect_redb_scope(scope_dir: &Path, report: &mut Report) -> io::Result<bool> {
    use crate::storage::{
        StorageBackendKind,
        config_store::{
            ConfigScopeAuthority, inspect_config_scope_authority, open_config_scope_for_read,
        },
        error::StorageBackendError,
    };

    let authority = match inspect_config_scope_authority(scope_dir, ROLE) {
        Ok(auth) => auth,
        Err(StorageBackendError::ManifestNotFinalized { .. }) => {
            report.exists = Some(true);
            report.classification = "unreadable";
            report.hints = vec![
                "Global configuration manifest (config-format.json) is unfinalized; resume migration with `libra-migrate --global`.",
            ];
            return Ok(true);
        }
        Err(_) => return Ok(false),
    };

    match authority {
        ConfigScopeAuthority::Manifest { manifest, db_path }
            if manifest.backend == StorageBackendKind::Redb =>
        {
            report.exists = Some(true);
            if let Ok(canonical) = db_path.canonicalize() {
                report.canonical_path = Some(canonical.to_string_lossy().into_owned());
                if let Ok(Some(stamp)) = regular_file_stamp(&canonical) {
                    report.size_bytes = Some(stamp.len);
                    report.modified_at_utc = utc_time(stamp.modified);
                }
            }
            match open_config_scope_for_read(scope_dir, ROLE, StorageBackendKind::Redb).await {
                Ok(_store) => {
                    report.classification = "compatible";
                    report.configuration.readable = true;
                    report.producer_disposition = "redb_manifest_finalized";
                    report.hints = vec![
                        "Global configuration uses finalized redb storage (config.redb); SQLite schema repair does not apply.",
                    ];
                }
                Err(_) => {
                    report.classification = "unreadable";
                    report.hints = vec![UNREADABLE_HINT];
                }
            }
            Ok(true)
        }
        ConfigScopeAuthority::Empty
            if StorageBackendKind::compiled_default() == StorageBackendKind::Redb =>
        {
            report.exists = Some(false);
            report.classification = "absent";
            report.hints =
                vec!["No global configuration file exists at this path; nothing was created."];
            Ok(true)
        }
        _ => Ok(false),
    }
}

async fn inspect(path: &Path, report: &mut Report) -> io::Result<()> {
    if let Some(scope_dir) = path.parent()
        && inspect_redb_scope(scope_dir, report).await?
    {
        return Ok(());
    }
    match crate::storage::StorageBackendKind::compiled_default() {
        crate::storage::StorageBackendKind::Redb if !crate::storage::has_sqlite_backend() => {
            report.classification = "unreadable";
            report.hints = vec![
                "Legacy SQLite global configuration detected under redb-only runtime; run `libra-migrate --global` to migrate to config.redb.",
            ];
            return Ok(());
        }
        crate::storage::StorageBackendKind::Redb | crate::storage::StorageBackendKind::Sqlite => {}
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            report.exists = Some(false);
            report.classification = "absent";
            report.hints =
                vec!["No global configuration file exists at this path; nothing was created."];
            return Ok(());
        }
        Err(error) => return Err(error),
        Ok(_) => report.exists = Some(true),
    }
    let canonical = path.canonicalize()?;
    report.canonical_path = Some(canonical.to_string_lossy().into_owned());
    let before =
        regular_file_stamp(&canonical)?.ok_or_else(|| io::Error::other("target disappeared"))?;
    report.size_bytes = Some(before.len);
    report.modified_at_utc = utc_time(before.modified);
    let wal_path = sidecar(&canonical, "-wal");
    let before_wal = regular_file_stamp(&wal_path)?;
    if !safe_to_open(&canonical, &before_wal)? {
        report.hints.push("Invalid SQLite header or missing WAL/SHM sidecars. Do not create sidecars manually; retry with the owning application or a SQLite-consistent snapshot.");
        return Ok(());
    }
    let result = inspect_snapshot(&canonical, report).await;
    if !target_unchanged(path, &canonical, &before, &before_wal) {
        report.changed();
        return Ok(());
    }
    result
}

fn target_unchanged(
    path: &Path,
    canonical: &Path,
    before: &FileStamp,
    before_wal: &Option<FileStamp>,
) -> bool {
    path.canonicalize().ok().as_deref() == Some(canonical)
        && regular_file_stamp(canonical).ok().flatten().as_ref() == Some(before)
        && regular_file_stamp(&sidecar(canonical, "-wal")).is_ok_and(|after| &after == before_wal)
}

pub(super) async fn execute(output: &OutputConfig) -> CliResult<()> {
    let resolution = crate::internal::config::global_config_resolution();
    let path = resolution
        .as_ref()
        .map(|resolution| resolution.path.clone());
    let mut report = Report::new(resolution.as_ref())?;
    if let Some(path) = path {
        // SQLite errors may contain untrusted schema text. Report a controlled
        // diagnostic instead of forwarding engine messages or config values.
        if inspect(&path, &mut report).await.is_err() {
            report.classification = "unreadable";
            report.hints = vec![UNREADABLE_HINT];
        }
    }
    if report.migration_pending && !report.hints.contains(&LEGACY_MIGRATION_HINT) {
        report.hints.push(LEGACY_MIGRATION_HINT);
    }
    // Both layouts on disk after the migration: the legacy file is a backup,
    // never a second source of truth, so say so and leave its removal to the
    // user (plan-20260919 GCX-02; DEFER-GCX-02 keeps automatic cleanup out).
    if !report.migration_pending
        && report.legacy_exists == Some(true)
        && !report.hints.contains(&LEGACY_BACKUP_HINT)
    {
        report.hints.push(LEGACY_BACKUP_HINT);
    }
    if output.is_json() {
        emit_json_data("config", &report, output)?;
    } else if !output.quiet {
        println!("Global configuration schema doctor");
        println!("  scope: {} ({})", report.scope, report.role);
        println!("  path_source: {}", report.path_source);
        println!("  migration_pending: {}", report.migration_pending);
        println!(
            "  legacy: {:?} (exists: {:?})",
            report.legacy_path.as_deref().unwrap_or("unavailable"),
            report.legacy_exists
        );
        println!(
            "  exists: {:?}, size_bytes: {:?}",
            report.exists, report.size_bytes
        );
        println!(
            "  path: {:?}",
            report.configured_path.as_deref().unwrap_or("unavailable")
        );
        println!(
            "  canonical_path: {:?}",
            report.canonical_path.as_deref().unwrap_or("unavailable")
        );
        println!("  classification: {}", report.classification);
        for ledger in [&report.configuration, &report.legacy] {
            println!(
                "  {}: observed={}, latest={}, readable={}",
                ledger.ledger,
                ledger.observed_version.as_deref().unwrap_or("unavailable"),
                ledger.latest_version.as_deref().unwrap_or("unavailable"),
                ledger.readable
            );
            if let Some(name) = ledger.verified_name {
                println!("    verified_name: {name}");
            }
        }
        println!(
            "  modified_at_utc: {}",
            report.modified_at_utc.as_deref().unwrap_or("unavailable")
        );
        println!("  producer_disposition: {}", report.producer_disposition);
        println!("  repair_eligible: {}", report.repair_eligible);
        if let Some(issue) = &report.issue {
            println!(
                "  issue: {} version {}: {}",
                issue.ledger, issue.version, issue.reason
            );
        }
        for hint in &report.hints {
            println!("  hint: {hint}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_gate_refuses_short_bad_magic_and_mixed_versions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("header");
        for bytes in [&b""[..], &b"short"[..]] {
            fs::write(&path, bytes).unwrap();
            assert!(safe_to_open(&path, &None).is_err());
        }
        fs::write(&path, [0_u8; 100]).unwrap();
        assert!(!safe_to_open(&path, &None).unwrap());
        let mut header = [0_u8; 100];
        header[..16].copy_from_slice(b"SQLite format 3\0");
        for versions in [[1, 2], [2, 1], [3, 3]] {
            header[18..20].copy_from_slice(&versions);
            fs::write(&path, header).unwrap();
            assert!(!safe_to_open(&path, &None).unwrap());
        }
    }

    #[test]
    fn mtime_handles_epoch_and_pre_epoch_without_panicking() {
        assert_eq!(
            utc_time(UNIX_EPOCH).as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert_eq!(
            utc_time(UNIX_EPOCH - Duration::from_secs(1)).as_deref(),
            Some("1969-12-31T23:59:59Z")
        );
    }

    #[test]
    fn file_fence_detects_database_and_wal_changes_and_discards_attribution() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("database");
        fs::write(&path, b"before").unwrap();
        let canonical = path.canonicalize().unwrap();
        let before = regular_file_stamp(&canonical).unwrap().unwrap();
        assert!(target_unchanged(&path, &canonical, &before, &None));
        fs::write(sidecar(&canonical, "-wal"), b"new WAL").unwrap();
        assert!(!target_unchanged(&path, &canonical, &before, &None));
        let wal = regular_file_stamp(&sidecar(&canonical, "-wal")).unwrap();
        fs::write(&path, b"changed database").unwrap();
        assert!(!target_unchanged(&path, &canonical, &before, &wal));
        let mut report = Report::new(None).unwrap();
        report.producer_disposition = "known_repository_receipt_unattested";
        report.legacy.verified_name = Some("operation_v2_branch_convergence");
        report.issue = Some(IssueReport {
            ledger: "schema_versions",
            version: "1".into(),
            reason: "unsupported receipt",
        });
        report.changed();
        assert_eq!(report.classification, "changed_during_inspection");
        assert_eq!(report.producer_disposition, "unattributed");
        assert!(report.legacy.verified_name.is_none());
        assert!(report.issue.is_none());
        assert!(!report.repair_eligible);
    }
}
