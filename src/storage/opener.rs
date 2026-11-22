//! `redb 5.0` `MultiWriter` database opener, tiered cache limits, and bounded
//! async write-transaction acquisition (`ADR-RB-03`).
//!
//! `redb::Builder::new()` defaults to `ExclusiveWriter` and a `1 GiB` page
//! cache. Every Libra database opener goes through this module so all handles
//! uniformly use [`ConcurrencyMode::MultiWriter`], enforce role-appropriate
//! cache caps (`16 MiB` / `32 MiB` / `64 MiB`), and isolate write-lock retries
//! plus `commit()` inside `tokio::task::spawn_blocking`.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use redb::{
    Builder, CommitError, ConcurrencyMode, Database, DatabaseError, ReadOnlyDatabase, StorageError,
    TransactionError, WriteTransaction,
};

use crate::{
    internal::db::schema::DatabaseRole,
    storage::error::{StorageBackendError, StorageResult},
};

/// Page cache cap for read-only CLI commands and desktop GUI readers (`16 MiB`).
pub const CACHE_SIZE_READ_ONLY: usize = 16 * 1024 * 1024;
/// Page cache cap for standard short-lived CLI write commands (`32 MiB`).
pub const CACHE_SIZE_STANDARD_WRITE: usize = 32 * 1024 * 1024;
/// Page cache cap for long-lived `agent bridge`, daemon, and queue consumers (`64 MiB`).
pub const CACHE_SIZE_LONG_LIVED: usize = 64 * 1024 * 1024;

/// Initial retry delay when `try_begin_write` reports `WouldBlock`.
const WRITE_BACKOFF_MIN_MS: u64 = 1;
/// Upper bound on retry delay between `try_begin_write` polls.
const WRITE_BACKOFF_MAX_MS: u64 = 25;

/// Complete set of cache and concurrency profiles for opening a `.redb` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RedbOpenProfile {
    /// Read-only CLI commands and desktop GUI (`16 MiB` cache).
    ReadOnly,
    /// Standard CLI write commands (`32 MiB` cache).
    StandardWrite,
    /// Long-lived daemon, bridge, or capture-queue consumer (`64 MiB` cache).
    LongLived,
}

impl RedbOpenProfile {
    /// Return the mandatory cache cap in bytes for this profile.
    #[must_use]
    pub const fn cache_size_bytes(self) -> usize {
        match self {
            Self::ReadOnly => CACHE_SIZE_READ_ONLY,
            Self::StandardWrite => CACHE_SIZE_STANDARD_WRITE,
            Self::LongLived => CACHE_SIZE_LONG_LIVED,
        }
    }

    /// Return the mandatory `redb` concurrency mode (`MultiWriter` for all profiles).
    #[must_use]
    pub const fn concurrency_mode(self) -> ConcurrencyMode {
        ConcurrencyMode::MultiWriter
    }
}

/// Writable open profiles accepted by [`create_redb_writable`] and [`open_redb_writable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RedbWriteProfile {
    /// Standard short-lived CLI write commands (`32 MiB` cache).
    StandardWrite,
    /// Long-lived `agent bridge`, daemon, or capture-queue consumer (`64 MiB` cache).
    LongLived,
}

impl RedbWriteProfile {
    /// Convert to the corresponding [`RedbOpenProfile`].
    #[must_use]
    pub const fn into_open_profile(self) -> RedbOpenProfile {
        match self {
            Self::StandardWrite => RedbOpenProfile::StandardWrite,
            Self::LongLived => RedbOpenProfile::LongLived,
        }
    }

    /// Return the mandatory cache cap in bytes for this writable profile.
    #[must_use]
    pub const fn cache_size_bytes(self) -> usize {
        self.into_open_profile().cache_size_bytes()
    }
}

/// Internal sink abstraction allowing tests to verify that [`configure_redb_builder`]
/// invokes both `set_concurrency_mode` and `set_cache_size` with exact profile values.
trait RedbBuilderTarget {
    fn apply_concurrency_mode(&mut self, mode: ConcurrencyMode);
    fn apply_cache_size(&mut self, bytes: usize);
}

impl RedbBuilderTarget for Builder {
    fn apply_concurrency_mode(&mut self, mode: ConcurrencyMode) {
        self.set_concurrency_mode(mode);
    }

    fn apply_cache_size(&mut self, bytes: usize) {
        self.set_cache_size(bytes);
    }
}

fn configure_redb_builder(target: &mut impl RedbBuilderTarget, profile: RedbOpenProfile) {
    target.apply_concurrency_mode(profile.concurrency_mode());
    target.apply_cache_size(profile.cache_size_bytes());
}

/// Construct a [`redb::Builder`] configured with [`ConcurrencyMode::MultiWriter`]
/// and the exact cache limit for `profile`.
#[must_use]
pub fn redb_builder_for(profile: RedbOpenProfile) -> Builder {
    let mut builder = Builder::new();
    configure_redb_builder(&mut builder, profile);
    builder
}

/// Open an existing `.redb` file strictly read-only with [`ConcurrencyMode::MultiWriter`]
/// and the `16 MiB` read-only cache cap (`ADR-RB-03` #1/#2/#5).
pub fn open_redb_read_only(
    path: impl AsRef<Path>,
    role: DatabaseRole,
) -> StorageResult<ReadOnlyDatabase> {
    redb_builder_for(RedbOpenProfile::ReadOnly)
        .open_read_only(path)
        .map_err(|err| map_database_error(err, role, "open redb read-only"))
}

/// Create (or open existing) writable `.redb` file with [`ConcurrencyMode::MultiWriter`]
/// and the cache limit specified by `profile` (`32 MiB` or `64 MiB`).
pub fn create_redb_writable(
    path: impl AsRef<Path>,
    role: DatabaseRole,
    profile: RedbWriteProfile,
) -> StorageResult<Database> {
    redb_builder_for(profile.into_open_profile())
        .create(path)
        .map_err(|err| map_database_error(err, role, "create redb database"))
}

/// Open an existing writable `.redb` file with [`ConcurrencyMode::MultiWriter`]
/// and the cache limit specified by `profile` (`32 MiB` or `64 MiB`).
pub fn open_redb_writable(
    path: impl AsRef<Path>,
    role: DatabaseRole,
    profile: RedbWriteProfile,
) -> StorageResult<Database> {
    redb_builder_for(profile.into_open_profile())
        .open(path)
        .map_err(|err| map_database_error(err, role, "open redb writable"))
}

/// Acquire a `redb` [`WriteTransaction`] synchronously with bounded `1ms → 25ms`
/// jittered backoff over [`Database::try_begin_write`].
///
/// When `timeout` is [`Duration::ZERO`], performs a single immediate non-blocking
/// [`Database::try_begin_write`] attempt without sleeping.
///
/// Returns [`StorageBackendError::WriteLockBusy`] once `timeout` elapses while
/// another thread or OS process holds the database write lock.
pub fn begin_write_bounded_blocking(
    db: &Database,
    role: DatabaseRole,
    timeout: Duration,
) -> StorageResult<WriteTransaction> {
    if timeout.is_zero() {
        return match db.try_begin_write() {
            Ok(wtx) => Ok(wtx),
            Err(TransactionError::WouldBlock) => {
                Err(StorageBackendError::WriteLockBusy { role, waited_ms: 0 })
            }
            Err(other) => Err(map_transaction_error(other, "begin redb write transaction")),
        };
    }
    begin_write_bounded_since(db, role, Instant::now(), timeout, None)
}

fn begin_write_bounded_since(
    db: &Database,
    role: DatabaseRole,
    started: Instant,
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> StorageResult<WriteTransaction> {
    let mut backoff_ms = WRITE_BACKOFF_MIN_MS;

    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            let waited_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            return Err(StorageBackendError::WriteLockBusy { role, waited_ms });
        }

        let elapsed_before_try = started.elapsed();
        if elapsed_before_try >= timeout {
            let waited_ms = elapsed_before_try.as_millis().min(u64::MAX as u128) as u64;
            return Err(StorageBackendError::WriteLockBusy { role, waited_ms });
        }

        match db.try_begin_write() {
            Ok(wtx) => {
                // If an async timeout cancelled us concurrently while we were
                // acquiring the lock, drop the transaction immediately instead
                // of returning a leaked write handle.
                if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                    drop(wtx);
                    let waited_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                    return Err(StorageBackendError::WriteLockBusy { role, waited_ms });
                }
                return Ok(wtx);
            }
            Err(TransactionError::WouldBlock) => {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    let waited_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
                    return Err(StorageBackendError::WriteLockBusy { role, waited_ms });
                }
                let remaining = timeout.saturating_sub(elapsed);
                let jitter_ceiling = backoff_ms.clamp(WRITE_BACKOFF_MIN_MS, WRITE_BACKOFF_MAX_MS);
                let jittered_ms = fastrand::u64(WRITE_BACKOFF_MIN_MS..=jitter_ceiling);
                let sleep_dur = Duration::from_millis(jittered_ms).min(remaining);
                thread::sleep(sleep_dur);
                backoff_ms = (backoff_ms.saturating_mul(2)).min(WRITE_BACKOFF_MAX_MS);
            }
            Err(other) => return Err(map_transaction_error(other, "begin redb write transaction")),
        }
    }
}

/// Acquire a `redb` [`WriteTransaction`] off the Tokio executor via
/// `tokio::task::spawn_blocking`, using bounded `1ms → 25ms` jittered backoff.
///
/// The deadline is anchored **before** scheduling on the blocking pool and also
/// guarded by `tokio::time::timeout` + an atomic cancellation flag so a
/// saturated blocking pool never silently stretches past `timeout` or leaks a
/// write transaction after timeout.
///
/// A [`Duration::ZERO`] budget leaves zero time to schedule onto the blocking
/// pool or wait on lock contention and immediately returns
/// [`StorageBackendError::WriteLockBusy`] without waiting (use
/// [`begin_write_bounded_blocking`] on a blocking thread for a single immediate
/// synchronous try).
pub async fn begin_write_bounded(
    db: Arc<Database>,
    role: DatabaseRole,
    timeout: Duration,
) -> StorageResult<WriteTransaction> {
    if timeout.is_zero() {
        return Err(StorageBackendError::WriteLockBusy { role, waited_ms: 0 });
    }

    let started = Instant::now();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_worker = Arc::clone(&cancelled);

    let handle = tokio::task::spawn_blocking(move || {
        begin_write_bounded_since(&db, role, started, timeout, Some(&cancelled_worker))
    });

    match tokio::time::timeout(timeout, handle).await {
        Ok(join_res) => join_res.map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("redb write-lock acquisition task failed: {err}"),
        })?,
        Err(_) => {
            cancelled.store(true, Ordering::Release);
            let waited_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            Err(StorageBackendError::WriteLockBusy { role, waited_ms })
        }
    }
}

/// Commit a `redb` [`WriteTransaction`] off the Tokio executor via
/// `tokio::task::spawn_blocking`.
pub async fn commit_write_bounded(wtx: WriteTransaction) -> StorageResult<()> {
    tokio::task::spawn_blocking(move || {
        wtx.commit()
            .map_err(|err| map_commit_error(err, "commit redb write transaction"))
    })
    .await
    .map_err(|err| StorageBackendError::BackendInternal {
        detail: format!("redb commit task failed: {err}"),
    })?
}

fn map_storage_error(err: StorageError, context: &'static str) -> StorageBackendError {
    match err {
        StorageError::Io(io_err) => StorageBackendError::io(context, io_err),
        StorageError::Corrupted(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb storage reported corruption"),
        },
        StorageError::ValueTooLarge(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb value exceeded maximum supported size"),
        },
        StorageError::PreviousIo => StorageBackendError::BackendInternal {
            detail: format!(
                "{context}: redb database entered read-only error state after prior I/O failure"
            ),
        },
        StorageError::LockPoisoned(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: internal lock poisoned"),
        },
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: unexpected redb storage error"),
        },
    }
}

fn map_database_error(
    err: DatabaseError,
    role: DatabaseRole,
    context: &'static str,
) -> StorageBackendError {
    match err {
        DatabaseError::RepairAborted => StorageBackendError::RepairAborted {
            role,
            detail: "unrecovered redb file cannot be repaired during read-only open".to_string(),
        },
        DatabaseError::DatabaseAlreadyOpen => StorageBackendError::BackendInternal {
            detail: format!("{context}: redb database is exclusively locked by another process"),
        },
        DatabaseError::LockProtocolMismatch => StorageBackendError::BackendInternal {
            detail: format!(
                "{context}: another process opened the redb file with an incompatible lock protocol version"
            ),
        },
        // `redb::DatabaseError::UpgradeRequired(actual)` means the file uses an
        // *older* redb file format version (expected v3, not a newer Libra schema),
        // so keep it distinct from `SchemaTooNew` and give version-specific
        // manual redb format-upgrade instructions.
        DatabaseError::UpgradeRequired(actual) => {
            let detail = match actual {
                2 => format!(
                    "{context}: {role} redb file uses legacy redb format version 2 (expected format v3); upgrade it to v3 using `redb 2.6` `Database::upgrade()` before reopening"
                ),
                1 => format!(
                    "{context}: {role} redb file uses legacy redb format version 1 (expected format v3); export and rebuild it to format v2 with `redb 2.x`, then upgrade to v3 using `redb 2.6` `Database::upgrade()` before reopening"
                ),
                other => format!(
                    "{context}: {role} redb file uses unsupported legacy redb format version {other} (expected format v3); export and rebuild it into redb format v3 before reopening"
                ),
            };
            StorageBackendError::BackendInternal { detail }
        }
        DatabaseError::Storage(storage_err) => map_storage_error(storage_err, context),
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: unexpected redb database error"),
        },
    }
}

fn map_transaction_error(err: TransactionError, context: &'static str) -> StorageBackendError {
    match err {
        TransactionError::Storage(storage_err) => map_storage_error(storage_err, context),
        TransactionError::ReadTransactionStillInUse(_) => StorageBackendError::BackendInternal {
            detail: format!("{context}: read transaction still has live table handles"),
        },
        TransactionError::WouldBlock => StorageBackendError::BackendInternal {
            detail: format!("{context}: write transaction would block"),
        },
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: unexpected redb transaction error"),
        },
    }
}

fn map_commit_error(err: CommitError, context: &'static str) -> StorageBackendError {
    match err {
        CommitError::Storage(storage_err) => map_storage_error(storage_err, context),
        _ => StorageBackendError::BackendInternal {
            detail: format!("{context}: unexpected redb commit error"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf, process::Command, sync::Arc, time::Duration};

    use redb::{ReadableDatabase, ReadableTable, TableDefinition};
    use tempfile::tempdir;

    use super::*;

    const KV: TableDefinition<&str, &str> = TableDefinition::new("opener_test_kv");

    struct ChildReleaseGuard {
        child: Option<std::process::Child>,
        lock_go_path: PathBuf,
        release_path: PathBuf,
    }

    impl Drop for ChildReleaseGuard {
        fn drop(&mut self) {
            let _ = fs::write(&self.lock_go_path, b"lock");
            let _ = fs::write(&self.release_path, b"go");
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[derive(Default)]
    struct RecordingBuilder {
        mode: Option<ConcurrencyMode>,
        cache_size: Option<usize>,
    }

    impl RedbBuilderTarget for RecordingBuilder {
        fn apply_concurrency_mode(&mut self, mode: ConcurrencyMode) {
            self.mode = Some(mode);
        }

        fn apply_cache_size(&mut self, bytes: usize) {
            self.cache_size = Some(bytes);
        }
    }

    #[test]
    fn multiwriter_concurrent_open_and_bounded_write_timeout() {
        // Handle isolated child-process modes so AC5 & AC6 exercise genuine
        // multi-process OS file locking across distinct processes.
        if let Some(mode) = env::var_os("LIBRA_RB02_CHILD_MODE") {
            let db_path = PathBuf::from(
                env::var_os("LIBRA_RB02_DB_PATH").expect("child requires LIBRA_RB02_DB_PATH"),
            );
            match mode.to_str() {
                Some("writer_holder") => {
                    let opened_path =
                        PathBuf::from(env::var_os("LIBRA_RB02_OPENED_PATH").expect("opened path"));
                    let lock_go_path = PathBuf::from(
                        env::var_os("LIBRA_RB02_LOCK_GO_PATH").expect("lock_go path"),
                    );
                    let ready_path =
                        PathBuf::from(env::var_os("LIBRA_RB02_READY_PATH").expect("ready path"));
                    let release_path = PathBuf::from(
                        env::var_os("LIBRA_RB02_RELEASE_PATH").expect("release path"),
                    );
                    let db = open_redb_writable(
                        &db_path,
                        DatabaseRole::Repository,
                        RedbWriteProfile::StandardWrite,
                    )
                    .expect("holder child opens MultiWriter redb writable");
                    fs::write(&opened_path, b"opened").expect("signal parent that DB is open");

                    let open_wait = Instant::now();
                    while !lock_go_path.exists() && open_wait.elapsed() < Duration::from_secs(10) {
                        thread::sleep(Duration::from_millis(10));
                    }
                    assert!(
                        lock_go_path.exists(),
                        "timed out waiting for parent concurrent open signal"
                    );

                    let wtx = begin_write_bounded_blocking(
                        &db,
                        DatabaseRole::Repository,
                        Duration::from_secs(5),
                    )
                    .expect("holder child acquires write transaction");
                    {
                        let mut table = wtx.open_table(KV).expect("open kv table in holder");
                        table
                            .insert("holder", "committed")
                            .expect("insert holder value");
                    }
                    fs::write(&ready_path, b"ready")
                        .expect("signal parent that write lock is held");

                    let wait_start = Instant::now();
                    while !release_path.exists() && wait_start.elapsed() < Duration::from_secs(10) {
                        thread::sleep(Duration::from_millis(10));
                    }
                    assert!(
                        release_path.exists(),
                        "timed out waiting for parent release signal"
                    );
                    wtx.commit().expect("holder child commits transaction");
                    return;
                }
                Some("reader_probe") => {
                    let ro = open_redb_read_only(&db_path, DatabaseRole::Repository)
                        .expect("reader child opens MultiWriter ReadOnlyDatabase without DatabaseAlreadyOpen");
                    let rtx = ro.begin_read().expect("reader child begins read txn");
                    let table = rtx.open_table(KV).expect("reader child opens kv table");
                    let init_val = table
                        .get("init")
                        .expect("read init key")
                        .expect("init key present");
                    assert_eq!(init_val.value(), "seed");
                    return;
                }
                other => panic!("unexpected LIBRA_RB02_CHILD_MODE: {other:?}"),
            }
        }

        // AC1-AC4: Verify `configure_redb_builder` (called directly by `redb_builder_for`)
        // actively applies `ConcurrencyMode::MultiWriter` and exact 16/32/64 MiB cache caps.
        for (profile, expected_cache) in [
            (RedbOpenProfile::ReadOnly, 16 * 1024 * 1024),
            (RedbOpenProfile::StandardWrite, 32 * 1024 * 1024),
            (RedbOpenProfile::LongLived, 64 * 1024 * 1024),
        ] {
            let mut rec = RecordingBuilder::default();
            configure_redb_builder(&mut rec, profile);
            assert_eq!(
                rec.mode,
                Some(ConcurrencyMode::MultiWriter),
                "profile {profile:?} must configure MultiWriter on builder"
            );
            assert_eq!(
                rec.cache_size,
                Some(expected_cache),
                "profile {profile:?} must configure exact cache size on builder"
            );
        }

        // Verify legacy redb format (`UpgradeRequired`) is NOT misclassified as `SchemaTooNew`
        // and carries actionable, version-specific redb upgrade instructions.
        let upgrade_v2 = map_database_error(
            DatabaseError::UpgradeRequired(2),
            DatabaseRole::Capture,
            "open redb writable",
        );
        assert!(
            matches!(upgrade_v2, StorageBackendError::BackendInternal { .. }),
            "UpgradeRequired(2) must map to BackendInternal format-upgrade guidance, got {upgrade_v2:?}"
        );
        assert_eq!(upgrade_v2.stable_code_str(), "LBR-IO-001");
        let upgrade_v2_msg = upgrade_v2.to_string();
        assert!(
            upgrade_v2_msg.contains("legacy redb format version 2")
                && upgrade_v2_msg.contains("redb 2.6")
                && upgrade_v2_msg.contains("Database::upgrade()")
                && upgrade_v2_msg.contains("v3")
                && !upgrade_v2_msg.contains("libra-migrate")
                && !upgrade_v2_msg.contains("previous redb/Libra"),
            "UpgradeRequired(2) message must specify redb 2.6 Database::upgrade(): {upgrade_v2_msg}"
        );

        let upgrade_v1_msg = map_database_error(
            DatabaseError::UpgradeRequired(1),
            DatabaseRole::Repository,
            "open redb writable",
        )
        .to_string();
        assert!(
            upgrade_v1_msg.contains("legacy redb format version 1")
                && upgrade_v1_msg.contains("export and rebuild it to format v2 with `redb 2.x`")
                && upgrade_v1_msg.contains("upgrade to v3 using `redb 2.6` `Database::upgrade()`")
                && !upgrade_v1_msg.contains("libra-migrate")
                && !upgrade_v1_msg.contains("previous redb/Libra"),
            "UpgradeRequired(1) message must pin every step of the v1->v2->v3 upgrade path: {upgrade_v1_msg}"
        );

        let upgrade_other_msg = map_database_error(
            DatabaseError::UpgradeRequired(0),
            DatabaseRole::Repository,
            "open redb writable",
        )
        .to_string();
        assert!(
            upgrade_other_msg.contains("unsupported legacy redb format version 0")
                && upgrade_other_msg.contains("export and rebuild it into redb format v3")
                && !upgrade_other_msg.contains("libra-migrate")
                && !upgrade_other_msg.contains("previous redb/Libra"),
            "UpgradeRequired(0) message must pin the export-and-rebuild v3 recovery action: {upgrade_other_msg}"
        );

        let dir = tempdir().expect("create tempdir");
        let db_path = dir.path().join("git.gen-00000001.redb");
        let opened_path = dir.path().join("holder.opened");
        let lock_go_path = dir.path().join("holder.lock_go");
        let ready_path = dir.path().join("holder.ready");
        let release_path = dir.path().join("holder.release");

        // Seed initial database using StandardWrite (32 MiB, MultiWriter).
        {
            let seed_db = create_redb_writable(
                &db_path,
                DatabaseRole::Repository,
                RedbWriteProfile::StandardWrite,
            )
            .expect("create seed redb");
            let wtx = begin_write_bounded_blocking(
                &seed_db,
                DatabaseRole::Repository,
                Duration::from_secs(2),
            )
            .expect("begin seed write");
            {
                let mut table = wtx.open_table(KV).expect("create kv table");
                table.insert("init", "seed").expect("insert seed key");
            }
            wtx.commit().expect("commit seed write");

            // Pre-retry deadline check: if `started` already passed `timeout`
            // (e.g., delayed scheduling on a saturated blocking pool), must fail
            // immediately with `WriteLockBusy` without acquiring even an unlocked DB.
            let expired_start = Instant::now()
                .checked_sub(Duration::from_millis(50))
                .expect("compute expired instant");
            let expired_err = begin_write_bounded_since(
                &seed_db,
                DatabaseRole::Repository,
                expired_start,
                Duration::from_millis(10),
                None,
            )
            .err()
            .expect("already-expired deadline must fail without acquiring write lock");
            assert!(
                matches!(
                    expired_err,
                    StorageBackendError::WriteLockBusy {
                        role: DatabaseRole::Repository,
                        ..
                    }
                ),
                "expected WriteLockBusy for pre-expired deadline, got {expired_err:?}"
            );
            // Zero-timeout pre-expired check in `begin_write_bounded_since` must
            // never bypass the deadline, while synchronous `begin_write_bounded_blocking`
            // with `Duration::ZERO` performs a single immediate non-blocking try.
            let zero_since_err = begin_write_bounded_since(
                &seed_db,
                DatabaseRole::Repository,
                Instant::now(),
                Duration::ZERO,
                None,
            )
            .err()
            .expect("begin_write_bounded_since with Duration::ZERO must fail immediately");
            assert!(matches!(
                zero_since_err,
                StorageBackendError::WriteLockBusy {
                    role: DatabaseRole::Repository,
                    ..
                }
            ));
            let immediate_wtx =
                begin_write_bounded_blocking(&seed_db, DatabaseRole::Repository, Duration::ZERO)
                    .expect("synchronous zero-timeout probe succeeds immediately when unlocked");
            drop(immediate_wtx);
        }

        // Verify blocking-pool saturation respects both zero and non-zero deadlines
        // in `begin_write_bounded`.
        {
            let pool_db = Arc::new(
                open_redb_writable(
                    &db_path,
                    DatabaseRole::Repository,
                    RedbWriteProfile::StandardWrite,
                )
                .expect("open db for blocking-pool saturation test"),
            );
            let rt_sat = tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(1)
                .enable_all()
                .build()
                .expect("build 1-blocking-thread tokio runtime");
            rt_sat.block_on(async {
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let blocker = tokio::task::spawn_blocking(move || {
                    let _ = entered_tx.send(());
                    thread::sleep(Duration::from_millis(250));
                });
                entered_rx.await.expect("blocking thread entered");

                let zero_start = Instant::now();
                let zero_err = begin_write_bounded(
                    Arc::clone(&pool_db),
                    DatabaseRole::Repository,
                    Duration::ZERO,
                )
                .await
                .err()
                .expect("zero timeout under saturated blocking pool must fail immediately");
                let zero_elapsed = zero_start.elapsed();
                assert!(
                    matches!(
                        zero_err,
                        StorageBackendError::WriteLockBusy {
                            role: DatabaseRole::Repository,
                            waited_ms: 0,
                        }
                    ),
                    "expected immediate WriteLockBusy(waited_ms: 0) for zero timeout, got {zero_err:?}"
                );
                assert!(
                    zero_elapsed < Duration::from_millis(50),
                    "zero timeout must not wait on 250ms blocker, observed {zero_elapsed:?}"
                );

                let sat_start = Instant::now();
                let sat_err = begin_write_bounded(
                    Arc::clone(&pool_db),
                    DatabaseRole::Repository,
                    Duration::from_millis(40),
                )
                .await
                .err()
                .expect("saturated blocking pool must still honor caller timeout");
                let sat_elapsed = sat_start.elapsed();
                assert!(
                    matches!(
                        sat_err,
                        StorageBackendError::WriteLockBusy {
                            role: DatabaseRole::Repository,
                            ..
                        }
                    ),
                    "expected WriteLockBusy under blocking-pool saturation, got {sat_err:?}"
                );
                assert!(
                    sat_elapsed < Duration::from_millis(200),
                    "timeout must fire before 250ms blocker completes, observed {sat_elapsed:?}"
                );
                blocker.await.expect("wait for blocker task");
            });
        }

        // Spawn OS child process that opens the DB in MultiWriter mode first.
        let exe = env::current_exe().expect("locate current test executable");
        let holder = Command::new(&exe)
            .arg("--exact")
            .arg("storage::opener::tests::multiwriter_concurrent_open_and_bounded_write_timeout")
            .arg("--nocapture")
            .env("LIBRA_RB02_CHILD_MODE", "writer_holder")
            .env("LIBRA_RB02_DB_PATH", &db_path)
            .env("LIBRA_RB02_OPENED_PATH", &opened_path)
            .env("LIBRA_RB02_LOCK_GO_PATH", &lock_go_path)
            .env("LIBRA_RB02_READY_PATH", &ready_path)
            .env("LIBRA_RB02_RELEASE_PATH", &release_path)
            .spawn()
            .expect("spawn holder child process");
        let mut holder_guard = ChildReleaseGuard {
            child: Some(holder),
            lock_go_path: lock_go_path.clone(),
            release_path: release_path.clone(),
        };

        let wait_open = Instant::now();
        while !opened_path.exists() && wait_open.elapsed() < Duration::from_secs(10) {
            if let Some(child) = holder_guard.child.as_mut()
                && let Some(status) = child.try_wait().expect("poll holder child")
            {
                panic!("holder child exited prematurely with {status}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(opened_path.exists(), "holder child did not open DB in time");

        // AC5: Concurrent multi-process open of Database (LongLived 64 MiB) + ReadOnlyDatabase (16 MiB)
        // while holder child already holds an open writable MultiWriter Database handle.
        let parent_db = Arc::new(
            open_redb_writable(
                &db_path,
                DatabaseRole::Repository,
                RedbWriteProfile::LongLived,
            )
            .expect("parent opens MultiWriter Database concurrently with holder child"),
        );
        let parent_ro = open_redb_read_only(&db_path, DatabaseRole::Repository)
            .expect("parent opens ReadOnlyDatabase concurrently with holder child");
        {
            let rtx = parent_ro.begin_read().expect("parent read txn");
            let table = rtx.open_table(KV).expect("parent read table");
            assert_eq!(
                table
                    .get("init")
                    .expect("read init")
                    .expect("init present")
                    .value(),
                "seed"
            );
        }

        // Now let the holder child acquire the exclusive write transaction and hold it.
        fs::write(&lock_go_path, b"lock").expect("signal holder to acquire write lock");
        let wait_ready = Instant::now();
        while !ready_path.exists() && wait_ready.elapsed() < Duration::from_secs(10) {
            if let Some(child) = holder_guard.child.as_mut()
                && let Some(status) = child.try_wait().expect("poll holder child")
            {
                panic!("holder child exited prematurely with {status}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ready_path.exists(),
            "holder child did not acquire write lock in time"
        );

        // Second child process opens ReadOnlyDatabase even while holder holds active WriteTransaction.
        let reader_status = Command::new(&exe)
            .arg("--exact")
            .arg("storage::opener::tests::multiwriter_concurrent_open_and_bounded_write_timeout")
            .arg("--nocapture")
            .env("LIBRA_RB02_CHILD_MODE", "reader_probe")
            .env("LIBRA_RB02_DB_PATH", &db_path)
            .status()
            .expect("spawn reader_probe child process");
        assert!(
            reader_status.success(),
            "reader_probe child failed with {reader_status}"
        );

        // AC6: Async begin_write_bounded returns typed WriteLockBusy when another process
        // holds the write lock beyond the deadline (both Repository and Capture roles).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio test runtime");

        let busy_start = Instant::now();
        let repo_busy = rt
            .block_on(begin_write_bounded(
                Arc::clone(&parent_db),
                DatabaseRole::Repository,
                Duration::from_millis(80),
            ))
            .err()
            .expect("begin_write_bounded must time out while holder process holds write lock");
        let busy_elapsed = busy_start.elapsed();
        assert!(
            matches!(
                repo_busy,
                StorageBackendError::WriteLockBusy {
                    role: DatabaseRole::Repository,
                    ..
                }
            ),
            "expected WriteLockBusy(Repository), got {repo_busy:?}"
        );
        assert_eq!(repo_busy.stable_code_str(), "LBR-IO-001");
        assert!(
            busy_elapsed >= Duration::from_millis(60) && busy_elapsed < Duration::from_secs(3),
            "expected bounded wait near 80ms, observed {busy_elapsed:?}"
        );

        let cap_busy = begin_write_bounded_blocking(
            &parent_db,
            DatabaseRole::Capture,
            Duration::from_millis(20),
        )
        .err()
        .expect("capture bounded write must also time out under contention");
        assert_eq!(cap_busy.stable_code_str(), "LBR-AGENT-047");

        // Release holder process, let it commit, then verify parent acquires & commits via spawn_blocking.
        fs::write(&release_path, b"go").expect("signal holder to commit and exit");
        let holder_status = holder_guard
            .child
            .take()
            .expect("holder child present")
            .wait()
            .expect("wait for holder child");
        assert!(
            holder_status.success(),
            "holder child failed with {holder_status}"
        );

        rt.block_on(async {
            let wtx = begin_write_bounded(
                Arc::clone(&parent_db),
                DatabaseRole::Repository,
                Duration::from_secs(2),
            )
            .await
            .expect("parent acquires write lock once holder finishes");
            {
                let mut table = wtx.open_table(KV).expect("open kv table for parent write");
                table
                    .insert("parent", "after_holder")
                    .expect("insert parent key");
            }
            commit_write_bounded(wtx)
                .await
                .expect("commit parent write via spawn_blocking");
        });

        let final_ro = open_redb_read_only(&db_path, DatabaseRole::Repository)
            .expect("reopen read-only after both writers committed");
        let rtx = final_ro.begin_read().expect("begin final read");
        let table = rtx.open_table(KV).expect("open kv table");
        assert_eq!(table.get("holder").unwrap().unwrap().value(), "committed");
        assert_eq!(
            table.get("parent").unwrap().unwrap().value(),
            "after_holder"
        );
    }
}
