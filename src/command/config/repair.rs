//! Explicit, format-attested global configuration recovery. No configuration
//! values leave SQLite; diagnostics never forward untrusted engine messages.

use std::path::Path;

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    output::OutputConfig,
};

pub(super) async fn execute(confirm: &Path, output: &OutputConfig) -> CliResult<()> {
    #[cfg(unix)]
    {
        supported::execute(confirm, output).await
    }
    #[cfg(not(unix))]
    {
        let _ = (confirm, output);
        Err(refused(
            "global schema repair is supported only on verified private Unix local files; use read-only doctor or a producer-compatible binary",
        ))
    }
}

fn refused(message: &'static str) -> CliError {
    CliError::failure(message).with_stable_code(StableErrorCode::ConfigSchemaFuture)
}

#[cfg(unix)]
mod supported {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use std::os::fd::AsRawFd;
    use std::{
        fs::{self, File, Metadata, OpenOptions},
        io::{self, Write},
        os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
        time::Duration,
    };

    use sea_orm::{
        ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement, TransactionTrait,
    };
    use serde::Serialize;
    use sha2::{Digest, Sha256};

    use super::{CliError, CliResult, OutputConfig, StableErrorCode, refused};
    use crate::{
        command::config::ConfigScope,
        internal::db::{self, DatabaseRole, schema},
        utils::output::emit_json_data,
    };

    const ROLE: DatabaseRole = DatabaseRole::GlobalConfig;
    const PRODUCER: &str = "v0.22.19-linux-amd64";
    const SOURCE_REVISION: &str = "b94bfe12f2ec2f039b88ddb5c6f8871787d60f17";
    const BINARY_SHA256: &str = "03447eb983178433425b5afffba351edae4044e7ded5b9e35c956dddb2bb68a6";
    const SCHEMA_SHA256: &str = "aaf969a014d1690f58cc76d2df0d71a55cb871cf6f70bfcc9bd1c6ec3f48714e";
    const RECEIPT_SHA256: &str = "d0078ce68596fe6c4e9f1d5c8e9db36181ca1fceac0c1cca3776bdd5e007a443";
    const SCHEMA_OBJECTS: usize = 293;
    const RECEIPTS: usize = 60;

    fn failed(message: &'static str) -> CliError {
        CliError::failure(message).with_stable_code(StableErrorCode::IoWriteFailed)
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Identity {
        device: u64,
        inode: u64,
    }

    impl Identity {
        fn of(metadata: &Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
    }

    #[derive(PartialEq, Eq)]
    struct Stamp {
        identity: Identity,
        len: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }

    impl Stamp {
        fn of(metadata: &Metadata) -> Self {
            Self {
                identity: Identity::of(metadata),
                len: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
            }
        }
    }

    struct Target {
        configured: PathBuf,
        path: PathBuf,
        parent: PathBuf,
        file: File,
        directory: File,
        uid: u32,
    }

    fn suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        PathBuf::from(value)
    }

    fn metadata(path: &Path) -> CliResult<Metadata> {
        fs::symlink_metadata(path).map_err(|_| refused("cannot verify the repair target or its permissions; check the path and rerun doctor"))
    }

    fn private_file(metadata: &Metadata, uid: u32) -> bool {
        metadata.is_file()
            && metadata.uid() == uid
            && metadata.nlink() == 1
            && metadata.mode() & 0o022 == 0
    }

    fn verified_local_filesystem(file: &File) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mut status = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: fstatfs initializes the supplied structure on success;
            // the borrowed file descriptor remains open throughout this call.
            if unsafe { libc::fstatfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
                return false;
            }
            // SAFETY: the successful fstatfs call initialized status above.
            let status = unsafe { status.assume_init() };
            #[cfg(target_os = "linux")]
            {
                matches!(
                    status.f_type as u64 & 0xffff_ffff,
                    0xEF53 | 0x5846_5342 | 0x9123_683E | 0x0102_1994 | 0x794C_7630
                )
            }
            #[cfg(target_os = "macos")]
            {
                let name: Vec<u8> = status
                    .f_fstypename
                    .iter()
                    .copied()
                    .take_while(|byte| *byte != 0)
                    .map(|byte| byte as u8)
                    .collect();
                matches!(name.as_slice(), b"apfs" | b"hfs")
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = file;
            false
        }
    }

    impl Target {
        fn resolve(confirm: &Path) -> CliResult<Self> {
            let configured = ConfigScope::Global.get_config_path()
                .ok_or_else(|| refused("cannot resolve the global configuration path; set LIBRA_CONFIG_GLOBAL_DB explicitly"))?;
            let path = fs::canonicalize(&configured)
                .map_err(|_| refused("global configuration target is missing or unreadable; run doctor before requesting repair"))?;
            if !confirm.is_absolute() || confirm.as_os_str() != path.as_os_str() {
                return Err(CliError::command_usage("--confirm must exactly match the absolute canonical global database path reported by doctor")
                    .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            let absolute = std::path::absolute(&configured).map_err(|_| {
                refused("cannot resolve the configured repair path; use an absolute path")
            })?;
            if absolute.as_os_str() != path.as_os_str() || path.to_str().is_none() {
                return Err(refused(
                    "repair refuses symlink, noncanonical or non-UTF-8 target paths; configure the actual canonical file and confirm it explicitly",
                ));
            }
            let parent = path
                .parent()
                .ok_or_else(|| refused("repair target has no parent directory"))?
                .to_path_buf();
            // An override must not disguise a Repository database as global.
            if path.file_name().is_some_and(|name| name == "libra.db")
                || parent.join("HEAD").exists()
                || parent.join("commondir").exists()
            {
                return Err(refused(
                    "repair target appears to be Repository storage; only a global configuration database may be repaired",
                ));
            }
            // SAFETY: geteuid takes no pointers and has no failure return.
            let uid = unsafe { libc::geteuid() };
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(&path)
                .map_err(|_| refused("cannot safely open the confirmed configuration file"))?;
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&parent)
                .map_err(|_| refused("cannot safely pin the configuration directory"))?;
            let target = Self {
                configured,
                path,
                parent,
                file,
                directory,
                uid,
            };
            target.verify_identity()?;
            if !verified_local_filesystem(&target.file) {
                return Err(refused(
                    "repair cannot verify this filesystem's local locking semantics; network and unsupported filesystems are not eligible",
                ));
            }
            target.validate_header()?;
            Ok(target)
        }

        fn verify_identity(&self) -> CliResult<()> {
            if fs::canonicalize(&self.configured).ok().as_deref() != Some(&self.path) {
                return Err(refused(
                    "repair target path changed; stop file replacement tools and rerun doctor",
                ));
            }
            let current = metadata(&self.path)?;
            let opened = self
                .file
                .metadata()
                .map_err(|_| refused("cannot recheck the pinned configuration file"))?;
            if !private_file(&current, self.uid) || Identity::of(&current) != Identity::of(&opened)
            {
                return Err(refused(
                    "repair requires an unchanged, current-user-owned, single-link regular file that is not group/world-writable",
                ));
            }
            let direct = metadata(&self.parent)?;
            let pinned = self
                .directory
                .metadata()
                .map_err(|_| refused("cannot recheck the pinned configuration directory"))?;
            if !direct.is_dir()
                || direct.uid() != self.uid
                || direct.mode() & 0o022 != 0
                || Identity::of(&direct) != Identity::of(&pinned)
            {
                return Err(refused(
                    "repair requires an unchanged, current-user-owned directory that is not group/world-writable",
                ));
            }
            for ancestor in self.parent.ancestors().skip(1) {
                let info = metadata(ancestor)?;
                // The filesystem root is a mount boundary and cannot be
                // replaced by renaming an unprivileged directory. Some
                // user namespaces expose it with a synthetic owner instead
                // of uid 0, so ownership is not meaningful for this one
                // immutable ancestor; every ordinary non-sticky ancestor
                // remains current-user- or root-owned below.
                let is_filesystem_root = ancestor.parent().is_none();
                // MetadataExt::mode is u32 even where libc::mode_t is u16
                // (macOS). Use the portable Unix sticky permission bit.
                let sticky = info.mode() & 0o1000 != 0;
                // A sticky directory protects the private child from rename
                // or removal by another unprivileged owner, even when a
                // user-namespace gives the directory a synthetic UID (as it
                // commonly does for /tmp).
                let trusted_owner =
                    is_filesystem_root || info.uid() == 0 || info.uid() == self.uid || sticky;
                let safe_mode = info.mode() & 0o022 == 0 || sticky;
                if !info.is_dir() || !trusted_owner || !safe_mode {
                    return Err(refused(
                        "repair target has an unsafe ancestor directory; use private local storage",
                    ));
                }
            }
            for ending in ["-wal", "-shm", "-journal"] {
                match fs::symlink_metadata(suffix(&self.path, ending)) {
                    Ok(info) if private_file(&info, self.uid) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    _ => {
                        return Err(refused(
                            "repair refuses unsafe SQLite sidecars; stop writers and obtain a verified consistent snapshot",
                        ));
                    }
                }
            }
            Ok(())
        }

        fn validate_header(&self) -> CliResult<()> {
            let mut header = [0u8; 100];
            self.file.read_exact_at(&mut header, 0).map_err(|_| {
                refused("repair target is not a complete SQLite database; run doctor")
            })?;
            let rollback = header[18] == 1 && header[19] == 1;
            let wal = header[18] == 2
                && header[19] == 2
                && suffix(&self.path, "-wal").is_file()
                && suffix(&self.path, "-shm").is_file();
            if &header[..16] != b"SQLite format 3\0" || (!rollback && !wal) {
                return Err(refused(
                    "repair target has an invalid SQLite header or incomplete WAL sidecars; use a verified consistent snapshot",
                ));
            }
            Ok(())
        }

        fn stamp(&self) -> CliResult<(Stamp, Option<Stamp>)> {
            let main = Stamp::of(&metadata(&self.path)?);
            let wal = match fs::symlink_metadata(suffix(&self.path, "-wal")) {
                Ok(info) => Some(Stamp::of(&info)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(_) => {
                    return Err(refused(
                        "cannot inspect SQLite WAL identity; stop writers and retry",
                    ));
                }
            };
            Ok((main, wal))
        }

        fn lock(&self) -> CliResult<File> {
            self.verify_identity()?;
            let lock_path = suffix(&self.path, ".schema-repair.lock");
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(&lock_path)
                .map_err(|_| {
                    failed("cannot open the private repair lock; check directory permissions")
                })?;
            let info = file
                .metadata()
                .map_err(|_| failed("cannot verify the repair lock"))?;
            if !private_file(&info, self.uid)
                || info.mode() & 0o077 != 0
                || Identity::of(&info) != Identity::of(&metadata(&lock_path)?)
            {
                return Err(refused(
                    "repair lock is not a private, single-link regular file owned by this user",
                ));
            }
            file.try_lock().map_err(|_| failed("another repair holds the target lock, or locking is unavailable; wait for that operation to finish and retry"))?;
            self.verify_identity()?;
            Ok(file)
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Eligibility {
        Legacy,
        Protected,
    }

    async fn scalar<C: ConnectionTrait>(conn: &C, sql: &str) -> CliResult<i64> {
        conn.query_one_raw(Statement::from_string(conn.get_database_backend(), sql))
            .await
            .map_err(|_| {
                refused("cannot verify configuration schema metadata; no repair is authorized")
            })?
            .ok_or_else(|| refused("configuration schema verification returned no result"))?
            .try_get_by_index(0)
            .map_err(|_| refused("configuration schema verification returned invalid metadata"))
    }

    async fn attest<C: ConnectionTrait>(conn: &C) -> CliResult<Eligibility> {
        let inspection = schema::inspect_configuration_schema(conn, ROLE).await
            .map_err(|_| refused("configuration receipt metadata is unreadable; restore a verified backup or use a compatible producer"))?;
        if inspection.issue.is_some() {
            return Err(refused(
                "configuration receipt is not supported by this build; install a producer-compatible newer Libra binary",
            ));
        }
        if inspection.base_receipt_present && inspection.barrier_present {
            return Ok(Eligibility::Protected);
        }
        if inspection.base_receipt_present
            || inspection.barrier_present
            || schema::latest_schema_version_for_role(ROLE).ok().flatten() != Some(2026090601)
        {
            return Err(refused(
                "configuration ledger is outside the attested legacy repair cohort; no conversion is authorized",
            ));
        }
        let rows = conn.query_all_raw(Statement::from_string(conn.get_database_backend(),
            "SELECT CAST(substr(CAST(type AS BLOB),1,33) AS TEXT), CAST(substr(CAST(name AS BLOB),1,257) AS TEXT), CAST(substr(CAST(tbl_name AS BLOB),1,257) AS TEXT), CAST(substr(CAST(sql AS BLOB),1,4097) AS TEXT) FROM main.sqlite_master ORDER BY type,name LIMIT 294"))
            .await.map_err(|_| refused("cannot inspect the complete schema fingerprint; no repair is authorized"))?;
        let mut objects: Vec<(String, String, String, Option<String>)> =
            Vec::with_capacity(rows.len());
        for row in rows {
            objects.push((
                row.try_get_by_index(0)
                    .map_err(|_| refused("invalid schema object metadata"))?,
                row.try_get_by_index(1)
                    .map_err(|_| refused("invalid schema object metadata"))?,
                row.try_get_by_index(2)
                    .map_err(|_| refused("invalid schema object metadata"))?,
                row.try_get_by_index(3)
                    .map_err(|_| refused("invalid schema object metadata"))?,
            ));
        }
        // Fetch one byte beyond each bound and reject, never attest a truncated
        // prefix. BLOB slicing also preserves embedded NULs for the digest.
        if objects.iter().any(|(kind, name, table, sql)| {
            kind.len() > 32
                || name.len() > 256
                || table.len() > 256
                || sql.as_ref().is_some_and(|sql| sql.len() > 4096)
        }) {
            return Err(refused(
                "schema metadata exceeds the registered producer bounds; no repair is authorized",
            ));
        }
        let bytes = serde_json::to_vec(&objects)
            .map_err(|_| failed("cannot encode schema attestation metadata"))?;
        if objects.len() != SCHEMA_OBJECTS || hex::encode(Sha256::digest(bytes)) != SCHEMA_SHA256 {
            return Err(refused(
                "schema fingerprint does not match the registered producer format; no repair is authorized",
            ));
        }
        let rows = conn.query_all_raw(Statement::from_string(conn.get_database_backend(),
            "SELECT version, CAST(substr(CAST(name AS BLOB),1,257) AS TEXT) FROM schema_versions ORDER BY version,name LIMIT 61"))
            .await.map_err(|_| refused("cannot verify the producer receipt fingerprint"))?;
        let mut receipts: Vec<(i64, String)> = Vec::with_capacity(rows.len());
        for row in rows {
            receipts.push((
                row.try_get_by_index(0)
                    .map_err(|_| refused("invalid producer receipt version"))?,
                row.try_get_by_index(1)
                    .map_err(|_| refused("invalid producer receipt name"))?,
            ));
        }
        if receipts.iter().any(|(_, name)| name.len() > 256) {
            return Err(refused(
                "receipt metadata exceeds the registered producer bounds; no repair is authorized",
            ));
        }
        let bytes = serde_json::to_vec(&receipts)
            .map_err(|_| failed("cannot encode receipt attestation metadata"))?;
        if receipts.len() != RECEIPTS || hex::encode(Sha256::digest(bytes)) != RECEIPT_SHA256 {
            return Err(refused(
                "receipt fingerprint does not match the registered producer format; no repair is authorized",
            ));
        }
        for (kind, name, _, _) in objects {
            if kind != "table"
                || matches!(
                    name.as_str(),
                    "config"
                        | "config_kv"
                        | "schema_versions"
                        | "sqlite_sequence"
                        | "metadata_kv"
                        | "worktree_registry_capability"
                )
            {
                continue;
            }
            // Names are accepted only after the full pinned digest matches;
            // quote them anyway so this remains an identifier-only query.
            let name = name.replace('"', "\"\"");
            if scalar(
                conn,
                &format!("SELECT EXISTS(SELECT 1 FROM \"{name}\" LIMIT 1)"),
            )
            .await?
                != 0
            {
                return Err(refused(
                    "target contains Repository data and is not eligible for global configuration repair",
                ));
            }
        }
        let seeds = scalar(conn, "SELECT
            (SELECT count(*) FROM (SELECT 1 FROM metadata_kv LIMIT 2))=1
            AND EXISTS(SELECT 1 FROM metadata_kv WHERE scope='repository' AND target='' AND key='stash.reflog.generation' AND value='1' AND value_type='text')
            AND (SELECT count(*) FROM (SELECT 1 FROM worktree_registry_capability LIMIT 3))=2
            AND EXISTS(SELECT 1 FROM worktree_registry_capability WHERE version=2)
            AND EXISTS(SELECT 1 FROM worktree_registry_capability WHERE version=3)").await?;
        if seeds != 1 {
            return Err(refused(
                "non-configuration metadata differs from the producer bootstrap; Repository data cannot be repaired as global configuration",
            ));
        }
        Ok(Eligibility::Legacy)
    }

    async fn inspect_readonly(target: &Target) -> CliResult<Eligibility> {
        let before = target.stamp()?;
        let conn = schema::open_readonly_connection_for_role(
            &target.path,
            Duration::from_millis(200),
            ROLE,
        )
        .await
        .map_err(|_| {
            refused("cannot safely inspect the existing SQLite target; stop writers and run doctor")
        })?;
        let result = async {
            let txn = conn
                .begin()
                .await
                .map_err(|_| refused("cannot acquire a read-only schema snapshot"))?;
            let result = attest(&txn).await;
            txn.rollback()
                .await
                .map_err(|_| refused("cannot close the read-only schema snapshot"))?;
            result
        }
        .await;
        let closed = conn.close().await;
        target.verify_identity()?;
        if before != target.stamp()? {
            return Err(refused(
                "target changed during repair eligibility inspection; stop writers and retry",
            ));
        }
        closed.map_err(|_| refused("cannot close the schema inspection connection"))?;
        result
    }

    #[derive(Serialize)]
    struct Report {
        report_version: u8,
        action: &'static str,
        scope: &'static str,
        canonical_path: PathBuf,
        outcome: &'static str,
        producer_format: Option<&'static str>,
        source_revision: Option<&'static str>,
        producer_binary_sha256: Option<&'static str>,
        schema_sha256: Option<&'static str>,
        receipt_sha256: Option<&'static str>,
        backup_path: Option<PathBuf>,
        backup_verified: bool,
        committed: bool,
    }

    impl Report {
        fn new(target: &Target, attested: bool) -> Self {
            Self {
                report_version: 1,
                action: "repair",
                scope: "global",
                canonical_path: target.path.clone(),
                outcome: "not_started",
                producer_format: attested.then_some(PRODUCER),
                source_revision: attested.then_some(SOURCE_REVISION),
                producer_binary_sha256: attested.then_some(BINARY_SHA256),
                schema_sha256: attested.then_some(SCHEMA_SHA256),
                receipt_sha256: attested.then_some(RECEIPT_SHA256),
                backup_path: None,
                backup_verified: false,
                committed: false,
            }
        }
    }

    fn persist_report(directory: &Path, report: &Report) -> CliResult<()> {
        let mut file = tempfile::NamedTempFile::new_in(directory)
            .map_err(|_| failed("cannot create private recovery status metadata"))?;
        serde_json::to_writer_pretty(file.as_file_mut(), report)
            .map_err(|_| failed("cannot write recovery status metadata"))?;
        file.as_file_mut()
            .write_all(b"\n")
            .map_err(|_| failed("cannot finish recovery status metadata"))?;
        file.as_file()
            .sync_all()
            .map_err(|_| failed("cannot durably save recovery status metadata"))?;
        file.persist(directory.join("recovery.json"))
            .map_err(|_| failed("cannot publish recovery status metadata"))?;
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|_| failed("cannot durably save the recovery directory"))
    }

    async fn connection_version<C: ConnectionTrait>(conn: &C, nonce: &str) -> CliResult<i64> {
        let row = conn.query_one_raw(Statement::from_string(conn.get_database_backend(),
            "SELECT data_version, nonce FROM pragma_data_version, temp.libra_config_repair_connection"))
            .await.map_err(|_| refused("repair connection continuity could not be verified; no snapshot comparison is safe"))?
            .ok_or_else(|| refused("repair connection guard disappeared; retry with a fresh verified backup"))?;
        let observed: String = row
            .try_get_by_index(1)
            .map_err(|_| refused("repair connection guard is invalid"))?;
        if observed != nonce {
            return Err(refused(
                "repair connection changed; the backup cannot authorize mutation",
            ));
        }
        row.try_get_by_index(0)
            .map_err(|_| refused("cannot inspect SQLite data version"))
    }

    async fn apply_configuration_repair(txn: &DatabaseTransaction) -> CliResult<()> {
        schema::initialize_configuration_ledger_for_repair(txn).await
            .map_err(|_| failed("cannot initialize configuration receipt metadata; preserve the verified backup and retry diagnosis"))?;
        checkpoint("after_ledger")?;
        db::write_configuration_barrier(txn, ROLE).await
            .map_err(|_| failed("cannot protect the repaired configuration from older writers; preserve the verified backup"))?;
        checkpoint("after_barrier")?;
        Ok(())
    }

    async fn backup_and_repair(
        target: &Target,
        conn: &DatabaseConnection,
        directory: &Path,
        report: &mut Report,
    ) -> CliResult<()> {
        let nonce = uuid::Uuid::new_v4().to_string();
        conn.execute_unprepared(
            "CREATE TEMP TABLE libra_config_repair_connection (nonce TEXT NOT NULL)",
        )
        .await
        .map_err(|_| failed("cannot establish the repair connection guard"))?;
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO temp.libra_config_repair_connection VALUES (?)",
            [nonce.clone().into()],
        ))
        .await
        .map_err(|_| failed("cannot initialize the repair connection guard"))?;
        let before = connection_version(conn, &nonce).await?;
        let stamp = target.stamp()?;
        if attest(conn).await? != Eligibility::Legacy {
            return Err(refused(
                "repair eligibility changed before backup; rerun doctor",
            ));
        }
        target.verify_identity()?;
        let backup = directory.join("backup.sqlite");
        let backup_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&backup)
            .map_err(|_| {
                failed("cannot create a new private SQLite backup; no repair was started")
            })?;
        report.backup_path = Some(backup.clone());
        report.outcome = "backup_in_progress";
        persist_report(directory, report)?;
        set_backup_progress_hook(conn, true).await?;
        let copied = conn
            .execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "VACUUM INTO ?",
                [backup.to_string_lossy().into_owned().into()],
            ))
            .await;
        let removed_hook = set_backup_progress_hook(conn, false).await;
        copied.map_err(|_| failed("SQLite-consistent backup failed or was interrupted; the retained backup is unverified and must not be restored"))?;
        removed_hook?;
        backup_file
            .sync_all()
            .map_err(|_| failed("cannot durably flush the SQLite backup; it is not verified"))?;
        let reader =
            schema::open_readonly_connection_for_role(&backup, Duration::from_millis(200), ROLE)
                .await
                .map_err(|_| failed("cannot reopen the SQLite backup; no repair is authorized"))?;
        let verified = async {
            let row = reader
                .query_one_raw(Statement::from_string(
                    reader.get_database_backend(),
                    "PRAGMA quick_check(1)",
                ))
                .await
                .map_err(|_| failed("SQLite backup integrity verification failed"))?
                .ok_or_else(|| failed("SQLite backup integrity verification returned no result"))?;
            let integrity: String = row
                .try_get_by_index(0)
                .map_err(|_| failed("SQLite backup integrity result is invalid"))?;
            if integrity != "ok" || attest(&reader).await? != Eligibility::Legacy {
                return Err(failed(
                    "SQLite backup failed integrity or attestation checks; do not restore it",
                ));
            }
            Ok(())
        }
        .await;
        let closed = reader.close().await;
        verified?;
        closed.map_err(|_| failed("cannot close the backup verification connection"))?;
        report.backup_verified = true;
        report.outcome = "backup_verified";
        persist_report(directory, report)?;
        // The recovery directory's entry must be durable in its parent before
        // any source transaction can commit. Syncing the child alone is not
        // a durability guarantee for a newly created parent entry.
        target.directory.sync_all().map_err(|_| {
            failed(
                "cannot durably preserve the recovery directory; no repair transaction was started",
            )
        })?;
        checkpoint("after_backup")?;
        let txn = db::begin_write_transaction(conn).await
            .map_err(|_| failed("cannot acquire the configuration write lock; stop writers and retry with a fresh backup"))?;
        let result = async {
            target.verify_identity()?;
            if connection_version(&txn, &nonce).await? != before || stamp != target.stamp()? {
                return Err(refused("configuration changed during backup; the retained backup does not authorize repair of the current data"));
            }
            if attest(&txn).await? != Eligibility::Legacy {
                return Err(refused("configuration attestation changed under the write lock; no repair is authorized"));
            }
            apply_configuration_repair(&txn).await?;
            target.verify_identity()?;
            Ok(())
        }.await;
        if let Err(error) = result {
            txn.rollback().await.map_err(|_| failed("repair rollback could not be confirmed; preserve the database and verified backup for recovery"))?;
            return Err(error);
        }
        txn.commit().await.map_err(|_| failed("repair commit could not be confirmed; preserve the database and verified backup and rerun diagnosis"))?;
        report.committed = true;
        report.outcome = "repaired";
        target.verify_identity()?;
        persist_report(directory, report)?;
        target.directory.sync_all().map_err(|_| failed("repair committed but directory durability could not be confirmed; preserve the verified backup"))?;
        Ok(())
    }

    pub(super) async fn execute(confirm: &Path, output: &OutputConfig) -> CliResult<()> {
        let target = Target::resolve(confirm)?;
        let eligibility = inspect_readonly(&target).await?;
        let mut report = Report::new(&target, eligibility == Eligibility::Legacy);
        if eligibility == Eligibility::Protected {
            report.outcome = "already_protected";
        } else {
            let _lock = target.lock()?;
            checkpoint("locked")?;
            if inspect_readonly(&target).await? != Eligibility::Legacy {
                return Err(refused(
                    "configuration eligibility changed after acquiring the repair lock; rerun doctor",
                ));
            }
            let conn = schema::open_configuration_repair_connection(&target.path).await
                .map_err(|_| failed("cannot open the existing configuration for confirmed repair; no migration was run"))?;
            target.verify_identity()?;
            let directory = tempfile::Builder::new()
                .prefix(".libra-config-repair-")
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in(&target.parent)
                .map_err(|_| {
                    failed("cannot create a private recovery directory; no repair was started")
                })?
                .keep();
            let result = backup_and_repair(&target, &conn, &directory, &mut report).await;
            let closed = conn.close().await;
            if let Err(error) = result {
                // An observed path replacement may be the failure itself.
                // Do not follow that path again for best-effort cleanup writes.
                return Err(error.with_hint(format!("Preserve recovery directory '{}'. Backup verified: {}; commit confirmed: {}. Do not restore an unverified backup or edit SQLite receipts manually.", directory.display(), report.backup_verified, report.committed)));
            }
            closed.map_err(|_| failed("repair committed but its connection could not be closed; preserve the verified backup"))?;
        }
        if output.is_json() {
            emit_json_data("config", &report, output)?;
        } else if !output.quiet {
            println!("Global configuration schema repair: {}", report.outcome);
            println!("  canonical_path: {:?}", report.canonical_path);
            if let Some(path) = &report.backup_path {
                println!("  verified_backup: {path:?}");
            }
            println!(
                "  Format attestation does not identify the historical writer. Keep the verified backup; do not downgrade or edit receipts manually."
            );
        }
        Ok(())
    }

    #[cfg(not(feature = "test-upgrade"))]
    fn checkpoint(_stage: &str) -> CliResult<()> {
        Ok(())
    }

    #[cfg(feature = "test-upgrade")]
    fn checkpoint(stage: &str) -> CliResult<()> {
        if std::env::var("LIBRA_TEST").as_deref() != Ok("1")
            || std::env::var("LIBRA_TEST_CONFIG_REPAIR_PAUSE_AT").as_deref() != Ok(stage)
        {
            return Ok(());
        }
        let path = std::env::var_os("LIBRA_TEST_CONFIG_REPAIR_CHECKPOINT")
            .map(PathBuf::from)
            .ok_or_else(|| failed("test repair checkpoint path missing"))?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(suffix(&path, ".ready"))
            .map_err(|_| failed("test repair checkpoint could not be created"))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(action) = fs::read(suffix(&path, ".continue")) {
                return if action == b"continue" {
                    Ok(())
                } else {
                    Err(failed("injected repair failure; preserve the backup"))
                };
            }
            if std::time::Instant::now() >= deadline {
                return Err(failed("test repair checkpoint timed out"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    async fn set_backup_progress_hook(conn: &DatabaseConnection, enabled: bool) -> CliResult<()> {
        #[cfg(feature = "test-upgrade")]
        {
            if std::env::var("LIBRA_TEST").as_deref() == Ok("1")
                && std::env::var("LIBRA_TEST_CONFIG_REPAIR_PAUSE_AT").as_deref()
                    == Ok("during_backup")
            {
                let mut connection = conn
                    .get_sqlite_connection_pool()
                    .acquire()
                    .await
                    .map_err(|_| failed("cannot acquire test backup connection"))?;
                let mut handle = connection
                    .lock_handle()
                    .await
                    .map_err(|_| failed("cannot pin test backup handle"))?;
                let mut visited = false;
                handle.set_progress_handler(if enabled { 100 } else { 0 }, move || {
                    if visited {
                        return true;
                    }
                    visited = true;
                    checkpoint("during_backup").is_ok()
                });
            }
        }
        let _ = (conn, enabled);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn connection_guard_rejects_a_replacement_handle() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("config.db");
            let created = db::create_database(path.to_str().unwrap()).await.unwrap();
            created.close().await.unwrap();
            let before = fs::read(&path).unwrap();
            let first = schema::open_configuration_repair_connection(&path)
                .await
                .unwrap();
            first.execute_unprepared("CREATE TEMP TABLE libra_config_repair_connection(nonce TEXT NOT NULL); INSERT INTO temp.libra_config_repair_connection VALUES('first-handle')").await.unwrap();
            assert!(connection_version(&first, "first-handle").await.is_ok());
            assert!(connection_version(&first, "different-nonce").await.is_err());
            first.close().await.unwrap();
            let replacement = schema::open_configuration_repair_connection(&path)
                .await
                .unwrap();
            assert!(
                connection_version(&replacement, "first-handle")
                    .await
                    .is_err()
            );
            replacement.close().await.unwrap();
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }
}
