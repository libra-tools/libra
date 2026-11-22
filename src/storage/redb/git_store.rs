//! `redb 5.0` implementation of [`GitStore`] (`RB-04`).
//!
//! Stores core repository references (`reference`), reflog entries (`reflog` +
//! same-transaction secondary index `reflog_by_ref`), and atomic bridge
//! operation bindings (`agent_bridge_publication`) inside `git.redb` using the
//! versioned `[schema_ver: u16_le][postcard]` envelope (`crate::storage::codec`).
//!
//! Every read and write transaction validates `META_SCHEMA` (`Repository` role,
//! format version, and `u32::MAX` handle migration tombstone fence) as step 1
//! (`ADR-RB-03` #3). Read-only handles open via [`open_redb_read_only`] (`16 MiB`
//! cache, `MultiWriter`, zero write upgrades).

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use redb::{
    Database, ReadOnlyDatabase, ReadTransaction, ReadableTable, TableDefinition, TableHandle,
    WriteTransaction,
};
use serde::{Deserialize, Serialize};

use crate::{
    internal::db::schema::DatabaseRole,
    storage::{
        backend::StorageBackendKind,
        codec::{decode_versioned, encode_versioned},
        error::{StorageBackendError, StorageResult},
        git_store::{
            BridgePublicationPhase, BridgePublicationRecord, GitStore, REFLOG_HEAD_REF_NAME,
            RefCasOutcome, RefKey, RefRecord, RefTarget, ReflogEntry, ReflogWrite,
            is_valid_bridge_operation_id, validate_bridge_publication_binding, validate_ref_key,
            validate_ref_target, validate_reflog_writes_for_key, validate_worktree_filter,
        },
        manifest::fsync_dir,
        meta::{begin_checked_read, begin_checked_write_blocking, init_meta_schema},
        opener::{
            RedbRepairContext, begin_write_bounded_blocking, create_redb_writable,
            open_redb_read_only, open_redb_writable,
        },
        redb::{map_redb_commit_err, map_redb_storage_err, map_redb_table_err},
    },
};

/// Current postcard codec version for `reference` table values.
pub const GIT_REF_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `reflog` table values.
pub const GIT_REFLOG_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `reflog_by_ref` secondary index markers.
pub const GIT_REFLOG_INDEX_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `agent_bridge_publication` rows.
pub const GIT_BRIDGE_PUB_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for monotonic sequence counters.
pub const GIT_SEQ_CODEC_VERSION: u16 = 1;

/// Default bounded write-lock timeout for [`RedbGitStore`] (`30s`).
pub const DEFAULT_GIT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

const REFERENCE_TABLE_NAME: &str = "reference";
const REFLOG_TABLE_NAME: &str = "reflog";
const REFLOG_BY_REF_INDEX_NAME: &str = "reflog_by_ref";
const BRIDGE_PUBLICATION_TABLE_NAME: &str = "agent_bridge_publication";
const GIT_SEQ_TABLE_NAME: &str = "git_sequences";

const SEQ_KEY_REFLOG: &str = "reflog";

const REF_TAG_LOCAL_HEAD_MAIN: u8 = 0;
const REF_TAG_LOCAL_HEAD_LINKED: u8 = 1;
const REF_TAG_REMOTE_HEAD: u8 = 2;
const REF_TAG_LOCAL_BRANCH: u8 = 3;
const REF_TAG_REMOTE_BRANCH: u8 = 4;
const REF_TAG_TAG: u8 = 5;

/// Primary table for Git references (`(kind_tag, scope_or_remote, name) -> versioned RefTarget`).
pub const REFERENCE_TABLE: TableDefinition<'static, (u8, &str, &str), &[u8]> =
    TableDefinition::new(REFERENCE_TABLE_NAME);

/// Primary table for monotonic reflog entries (`id -> versioned ReflogStoredRow`).
pub const REFLOG_TABLE: TableDefinition<'static, u64, &[u8]> =
    TableDefinition::new(REFLOG_TABLE_NAME);

/// Same-transaction secondary index for reflog queries by `(ref_name, worktree_scope, id)`.
///
/// `worktree_scope` is `""` for main worktree / shared branch reflogs and the
/// non-empty `worktree_id` string for linked-worktree `HEAD` reflogs.
pub const REFLOG_BY_REF_INDEX: TableDefinition<'static, (&str, &str, u64), &[u8]> =
    TableDefinition::new(REFLOG_BY_REF_INDEX_NAME);

/// Primary table for atomic operation-to-commit bridge publication bindings
/// (`operation_id -> versioned BridgePublicationStoredRow`).
pub const BRIDGE_PUBLICATION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new(BRIDGE_PUBLICATION_TABLE_NAME);

/// Monotonic sequence counters for `git.redb` tables (`name -> versioned GitSequenceValue`).
pub const GIT_SEQ_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new(GIT_SEQ_TABLE_NAME);

static GIT_STORE_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RefTargetStored {
    target: RefTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReflogStoredRow {
    ref_name: String,
    old_oid: String,
    new_oid: String,
    timestamp: i64,
    committer_name: String,
    committer_email: String,
    action: String,
    message: String,
    worktree_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ReflogIndexMarker;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BridgePublicationStoredRow {
    commit_oid: String,
    phase: BridgePublicationPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct GitSequenceValue {
    last_id: u64,
}

fn encode_ref_key(key: &RefKey) -> (u8, &str, &str) {
    match key {
        RefKey::LocalHead { worktree_id: None } => (REF_TAG_LOCAL_HEAD_MAIN, "", ""),
        RefKey::LocalHead {
            worktree_id: Some(wt),
        } => (REF_TAG_LOCAL_HEAD_LINKED, wt.as_str(), ""),
        RefKey::RemoteHead { remote } => (REF_TAG_REMOTE_HEAD, remote.as_str(), ""),
        RefKey::LocalBranch { name } => (REF_TAG_LOCAL_BRANCH, "", name.as_str()),
        RefKey::RemoteBranch { remote, name } => {
            (REF_TAG_REMOTE_BRANCH, remote.as_str(), name.as_str())
        }
        RefKey::Tag { name } => (REF_TAG_TAG, "", name.as_str()),
    }
}

fn decode_ref_key(raw: (u8, &str, &str)) -> StorageResult<RefKey> {
    let (tag, p1, p2) = raw;
    match tag {
        REF_TAG_LOCAL_HEAD_MAIN => Ok(RefKey::LocalHead { worktree_id: None }),
        REF_TAG_LOCAL_HEAD_LINKED => {
            if p1.is_empty() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "corrupt redb linked LocalHead key with empty worktree_id".to_string(),
                });
            }
            Ok(RefKey::LocalHead {
                worktree_id: Some(p1.to_string()),
            })
        }
        REF_TAG_REMOTE_HEAD => {
            if p1.is_empty() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "corrupt redb RemoteHead key with empty remote".to_string(),
                });
            }
            Ok(RefKey::RemoteHead {
                remote: p1.to_string(),
            })
        }
        REF_TAG_LOCAL_BRANCH => {
            if p2.is_empty() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "corrupt redb LocalBranch key with empty name".to_string(),
                });
            }
            Ok(RefKey::LocalBranch {
                name: p2.to_string(),
            })
        }
        REF_TAG_REMOTE_BRANCH => {
            if p1.is_empty() || p2.is_empty() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "corrupt redb RemoteBranch key with empty remote or name".to_string(),
                });
            }
            Ok(RefKey::RemoteBranch {
                remote: p1.to_string(),
                name: p2.to_string(),
            })
        }
        REF_TAG_TAG => {
            if p2.is_empty() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "corrupt redb Tag key with empty name".to_string(),
                });
            }
            Ok(RefKey::Tag {
                name: p2.to_string(),
            })
        }
        _ => Err(StorageBackendError::BackendInternal {
            detail: format!("corrupt redb reference key tag {tag}"),
        }),
    }
}

fn decode_ref_target(bytes: &[u8]) -> StorageResult<RefTarget> {
    let row: RefTargetStored = decode_versioned(bytes, GIT_REF_CODEC_VERSION)?;
    Ok(row.target)
}

fn decode_reflog_row(id: u64, bytes: &[u8]) -> StorageResult<ReflogEntry> {
    let row: ReflogStoredRow = decode_versioned(bytes, GIT_REFLOG_CODEC_VERSION)?;
    let id_i64 = i64::try_from(id).map_err(|_| StorageBackendError::BackendInternal {
        detail: "reflog sequence id exceeded i64::MAX".to_string(),
    })?;
    Ok(ReflogEntry {
        id: id_i64,
        ref_name: row.ref_name,
        old_oid: row.old_oid,
        new_oid: row.new_oid,
        timestamp: row.timestamp,
        committer_name: row.committer_name,
        committer_email: row.committer_email,
        action: row.action,
        message: row.message,
        worktree_id: row.worktree_id,
    })
}

fn decode_bridge_pub_row(
    operation_id: String,
    bytes: &[u8],
) -> StorageResult<BridgePublicationRecord> {
    let row: BridgePublicationStoredRow = decode_versioned(bytes, GIT_BRIDGE_PUB_CODEC_VERSION)?;
    Ok(BridgePublicationRecord {
        operation_id,
        commit_oid: row.commit_oid,
        phase: row.phase,
    })
}

fn decode_seq_value(bytes: &[u8]) -> StorageResult<u64> {
    let seq: GitSequenceValue = decode_versioned(bytes, GIT_SEQ_CODEC_VERSION)?;
    Ok(seq.last_id)
}

fn initialize_core_git_tables(wtx: &WriteTransaction) -> StorageResult<()> {
    let _ = wtx
        .open_table(REFERENCE_TABLE)
        .map_err(|err| map_redb_table_err(err, "init reference table"))?;
    let _ = wtx
        .open_table(REFLOG_TABLE)
        .map_err(|err| map_redb_table_err(err, "init reflog table"))?;
    let _ = wtx
        .open_table(REFLOG_BY_REF_INDEX)
        .map_err(|err| map_redb_table_err(err, "init reflog_by_ref index"))?;
    let _ = wtx
        .open_table(BRIDGE_PUBLICATION_TABLE)
        .map_err(|err| map_redb_table_err(err, "init agent_bridge_publication table"))?;
    let mut seq = wtx
        .open_table(GIT_SEQ_TABLE)
        .map_err(|err| map_redb_table_err(err, "init git_sequences table"))?;
    let zero = encode_versioned(GIT_SEQ_CODEC_VERSION, &GitSequenceValue { last_id: 0 })?;
    seq.insert(SEQ_KEY_REFLOG, zero.as_slice())
        .map_err(|err| map_redb_storage_err(err, "seed reflog sequence"))?;
    Ok(())
}

fn required_core_table_names() -> [&'static str; 5] {
    [
        REFERENCE_TABLE_NAME,
        REFLOG_TABLE_NAME,
        REFLOG_BY_REF_INDEX_NAME,
        BRIDGE_PUBLICATION_TABLE_NAME,
        GIT_SEQ_TABLE_NAME,
    ]
}

fn verify_required_tables_read(rtx: &ReadTransaction) -> StorageResult<()> {
    let existing: Vec<String> = rtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list redb git store tables"))?
        .map(|handle| handle.name().to_string())
        .collect();
    for required in required_core_table_names() {
        if !existing.iter().any(|name| name == required) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("redb git store is missing required table `{required}`"),
            });
        }
    }
    Ok(())
}

fn verify_required_tables_write(wtx: &WriteTransaction) -> StorageResult<()> {
    let existing: Vec<String> = wtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list redb git store tables for write"))?
        .map(|handle| handle.name().to_string())
        .collect();
    for required in required_core_table_names() {
        if !existing.iter().any(|name| name == required) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("redb git store is missing required table `{required}`"),
            });
        }
    }
    Ok(())
}

fn next_reflog_id(wtx: &WriteTransaction) -> StorageResult<u64> {
    let mut seq_table = wtx
        .open_table(GIT_SEQ_TABLE)
        .map_err(|err| map_redb_table_err(err, "open git_sequences table"))?;
    let current = seq_table
        .get(SEQ_KEY_REFLOG)
        .map_err(|err| map_redb_storage_err(err, "read reflog sequence"))?
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "redb git store is missing reflog sequence counter".to_string(),
        })?;
    let prev = decode_seq_value(current.value())?;
    drop(current);
    let next = prev
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "reflog sequence counter overflow".to_string(),
        })?;
    let encoded = encode_versioned(GIT_SEQ_CODEC_VERSION, &GitSequenceValue { last_id: next })?;
    seq_table
        .insert(SEQ_KEY_REFLOG, encoded.as_slice())
        .map_err(|err| map_redb_storage_err(err, "advance reflog sequence"))?;
    Ok(next)
}

#[derive(Clone)]
enum RedbGitHandle {
    ReadWrite(Arc<Database>),
    ReadOnly(Arc<ReadOnlyDatabase>),
}

/// `redb 5.0` implementation of [`GitStore`] (`ADR-RB-03`, `ADR-RB-04`, `RB-04`).
#[derive(Clone)]
pub struct RedbGitStore {
    handle: RedbGitHandle,
    write_timeout: Duration,
}

impl std::fmt::Debug for RedbGitStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedbGitStore")
            .field("read_only", &self.is_read_only())
            .field("write_timeout", &self.write_timeout)
            .finish()
    }
}

struct TempPathGuard(Option<PathBuf>);

impl Drop for TempPathGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

impl RedbGitStore {
    /// Atomically create a brand-new `git.redb` store at `path` (`0600` on
    /// Unix, `MultiWriter`, `META_SCHEMA(Repository)`, core tables initialized).
    ///
    /// Fails closed with `Io { kind: AlreadyExists }` if `path` already exists.
    /// Stages into a unique sibling `.tmp` file, commits initial schema, then
    /// publishes via non-overwriting `hard_link` + `unlink(.tmp)` + `fsync_dir`
    /// so partially initialized database files are never exposed at `path`.
    pub fn create(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref();
        if fs::symlink_metadata(path).is_ok() {
            return Err(StorageBackendError::Io {
                context: "create redb git store",
                kind: io::ErrorKind::AlreadyExists,
                raw_os_error: None,
            });
        }
        let parent = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "redb git store path must end with a valid UTF-8 filename".to_string(),
            })?;

        let seq = GIT_STORE_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = parent.join(format!("{file_name}.{}.{seq}.tmp", std::process::id()));
        let mut tmp_guard = TempPathGuard(Some(tmp_path.clone()));

        {
            // Pre-create the temp staging file with strict `0600` (`O_CREAT | O_EXCL`)
            // before opening with `redb` so permissive umasks never expose a wider mode.
            let mut opts = fs::OpenOptions::new();
            opts.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                opts.mode(crate::storage::config_store::REPOSITORY_CONFIG_FILE_MODE)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
                let file = opts.open(&tmp_path).map_err(|err| {
                    StorageBackendError::io("stage redb git store temp file", err)
                })?;
                file.set_permissions(fs::Permissions::from_mode(
                    crate::storage::config_store::REPOSITORY_CONFIG_FILE_MODE,
                ))
                .map_err(|err| {
                    StorageBackendError::io("fchmod staged redb git store to 0600", err)
                })?;
                drop(file);
            }
            #[cfg(not(unix))]
            {
                let file = opts.open(&tmp_path).map_err(|err| {
                    StorageBackendError::io("stage redb git store temp file", err)
                })?;
                drop(file);
            }
            let tmp_db = create_redb_writable(
                &tmp_path,
                DatabaseRole::Repository,
                RedbRepairContext::WriteCommand,
            )?;
            let wtx = begin_write_bounded_blocking(
                &tmp_db,
                DatabaseRole::Repository,
                DEFAULT_GIT_WRITE_TIMEOUT,
            )?;
            init_meta_schema(&wtx, DatabaseRole::Repository)?;
            initialize_core_git_tables(&wtx)?;
            wtx.commit()
                .map_err(|err| map_redb_commit_err(err, "commit initial redb git schema"))?;
        }

        // Non-overwriting publication into `path`.
        fs::hard_link(&tmp_path, path)
            .map_err(|err| StorageBackendError::io("publish staged redb git store", err))?;
        fs::remove_file(&tmp_path)
            .map_err(|err| StorageBackendError::io("unlink staged redb git store alias", err))?;
        tmp_guard.0 = None;
        fsync_dir(parent)?;

        Self::open_read_write(path)
    }

    /// Open an existing `git.redb` store writable under [`RedbRepairContext::WriteCommand`].
    pub fn open_read_write(path: impl AsRef<Path>) -> StorageResult<Self> {
        Self::open_read_write_with_context(path, RedbRepairContext::WriteCommand)
    }

    /// Open an existing `git.redb` store writable under an explicit
    /// [`RedbRepairContext`] (`32 MiB` or `64 MiB` cache, `MultiWriter`).
    pub fn open_read_write_with_context(
        path: impl AsRef<Path>,
        context: RedbRepairContext,
    ) -> StorageResult<Self> {
        let db = open_redb_writable(path, DatabaseRole::Repository, context)?;
        let rtx = begin_checked_read(&db, DatabaseRole::Repository)?;
        verify_required_tables_read(&rtx)?;
        drop(rtx);
        Ok(Self {
            handle: RedbGitHandle::ReadWrite(Arc::new(db)),
            write_timeout: DEFAULT_GIT_WRITE_TIMEOUT,
        })
    }

    /// Open an existing `git.redb` store strictly read-only (`open_read_only`,
    /// `16 MiB` cache, `MultiWriter`, zero write upgrades).
    pub fn open_read_only(path: impl AsRef<Path>) -> StorageResult<Self> {
        let db = open_redb_read_only(path, DatabaseRole::Repository)?;
        let rtx = begin_checked_read(&db, DatabaseRole::Repository)?;
        verify_required_tables_read(&rtx)?;
        drop(rtx);
        Ok(Self {
            handle: RedbGitHandle::ReadOnly(Arc::new(db)),
            write_timeout: DEFAULT_GIT_WRITE_TIMEOUT,
        })
    }

    /// Wrap an already-opened writable `Arc<Database>` whose `META_SCHEMA` and
    /// core Git tables are initialized for [`DatabaseRole::Repository`].
    pub fn from_database(db: Arc<Database>) -> StorageResult<Self> {
        let rtx = begin_checked_read(db.as_ref(), DatabaseRole::Repository)?;
        verify_required_tables_read(&rtx)?;
        drop(rtx);
        Ok(Self {
            handle: RedbGitHandle::ReadWrite(db),
            write_timeout: DEFAULT_GIT_WRITE_TIMEOUT,
        })
    }

    /// Return a cloned handle using a custom bounded write-lock timeout.
    #[must_use]
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    /// Returns `true` if this handle was opened strictly read-only.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        matches!(self.handle, RedbGitHandle::ReadOnly(_))
    }

    fn writable_db(&self) -> StorageResult<Arc<Database>> {
        match &self.handle {
            RedbGitHandle::ReadWrite(db) => Ok(Arc::clone(db)),
            RedbGitHandle::ReadOnly(_) => Err(StorageBackendError::BackendInternal {
                detail: "cannot mutate a read-only git store".to_string(),
            }),
        }
    }

    async fn run_read<T, F>(&self, op: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&ReadTransaction) -> StorageResult<T> + Send + 'static,
    {
        let handle = self.handle.clone();
        tokio::task::spawn_blocking(move || {
            let rtx = match &handle {
                RedbGitHandle::ReadWrite(db) => {
                    begin_checked_read(db.as_ref(), DatabaseRole::Repository)?
                }
                RedbGitHandle::ReadOnly(db) => {
                    begin_checked_read(db.as_ref(), DatabaseRole::Repository)?
                }
            };
            verify_required_tables_read(&rtx)?;
            op(&rtx)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("redb git read task failed: {err}"),
        })?
    }

    async fn run_write_conditional<T, F>(&self, op: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&WriteTransaction) -> StorageResult<(bool, T)> + Send + 'static,
    {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum CancelState {
            Pending,
            Cancelled,
            Committing,
        }

        struct CancelOnDrop {
            state: Arc<Mutex<CancelState>>,
            armed: bool,
        }

        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                if self.armed
                    && let Ok(mut guard) = self.state.lock()
                    && *guard == CancelState::Pending
                {
                    *guard = CancelState::Cancelled;
                }
            }
        }

        let db = self.writable_db()?;
        let timeout = self.write_timeout;
        let state = Arc::new(Mutex::new(CancelState::Pending));
        let mut guard = CancelOnDrop {
            state: Arc::clone(&state),
            armed: true,
        };
        let worker_state = Arc::clone(&state);

        let result = tokio::task::spawn_blocking(move || {
            let is_cancelled = || -> bool {
                worker_state
                    .lock()
                    .map(|s| *s == CancelState::Cancelled)
                    .unwrap_or(true)
            };
            if is_cancelled() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "redb git write cancelled before starting".to_string(),
                });
            }
            let wtx = begin_checked_write_blocking(&db, DatabaseRole::Repository, timeout)?;
            if is_cancelled() {
                drop(wtx);
                return Err(StorageBackendError::BackendInternal {
                    detail: "redb git write cancelled after acquiring write lock".to_string(),
                });
            }
            verify_required_tables_write(&wtx)?;
            let (should_commit, value) = op(&wtx)?;
            if !should_commit {
                drop(wtx);
                return Ok(value);
            }
            {
                let mut st =
                    worker_state
                        .lock()
                        .map_err(|_| StorageBackendError::BackendInternal {
                            detail: "redb git write cancellation state poisoned".to_string(),
                        })?;
                if *st == CancelState::Cancelled {
                    drop(wtx);
                    return Err(StorageBackendError::BackendInternal {
                        detail: "redb git write cancelled before commit".to_string(),
                    });
                }
                *st = CancelState::Committing;
            }
            wtx.commit()
                .map_err(|err| map_redb_commit_err(err, "commit redb git write transaction"))?;
            Ok(value)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("redb git write task failed: {err}"),
        })?;

        guard.armed = false;
        result
    }

    async fn update_ref_cas_inner(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
        publication: Option<&BridgePublicationRecord>,
        abort_before_commit_for_test: bool,
    ) -> StorageResult<RefCasOutcome> {
        validate_ref_key(key)?;
        if let Some(exp) = expected {
            validate_ref_target(key, exp)?;
        }
        if let Some(new_t) = new_target {
            validate_ref_target(key, new_t)?;
        }
        validate_reflog_writes_for_key(key, reflogs)?;
        if let Some(pub_rec) = publication {
            validate_bridge_publication_binding(key, expected, new_target, pub_rec)?;
        }

        let key_owned = key.clone();
        let expected_owned = expected.cloned();
        let new_target_owned = new_target.cloned();
        let reflogs_owned = reflogs.to_vec();
        let publication_owned = publication.cloned();

        self.run_write_conditional(move |wtx| {
            let mut ref_table = wtx
                .open_table(REFERENCE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open reference table for write"))?;
            let encoded_key = encode_ref_key(&key_owned);
            let actual = match ref_table
                .get(encoded_key)
                .map_err(|err| map_redb_storage_err(err, "read reference for CAS"))?
            {
                Some(guard) => Some(decode_ref_target(guard.value())?),
                None => None,
            };

            if actual != expected_owned {
                return Ok((false, RefCasOutcome::CasConflict { actual }));
            }

            // Resolve symbolic `HEAD` when binding a bridge publication so switching
            // `Symbolic(branch -> X)` to `Direct(X)` cannot publish a record without
            // actually moving `HEAD` to a new commit OID.
            if let (Some(pub_rec), Some(RefTarget::Symbolic(sym_branch))) =
                (&publication_owned, &actual)
            {
                let branch_key = RefKey::LocalBranch {
                    name: sym_branch.clone(),
                };
                if let Some(branch_guard) = ref_table
                    .get(encode_ref_key(&branch_key))
                    .map_err(|err| {
                        map_redb_storage_err(
                            err,
                            "resolve symbolic HEAD target for bridge publication",
                        )
                    })?
                    && let RefTarget::Direct(resolved_oid) =
                        decode_ref_target(branch_guard.value())?
                    && resolved_oid == pub_rec.commit_oid
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "bridge publication requires HEAD to move to a new commit OID"
                            .to_string(),
                    });
                }
            }

            // When updating a `LocalBranch`, verify any accompanying `"HEAD"`
            // reflog entries belong to a worktree whose `LocalHead` points
            // symbolically to this branch, and when binding a bridge publication
            // verify the branch is checked out symbolically by a worktree HEAD.
            if let RefKey::LocalBranch { name: branch_name } = &key_owned {
                for entry in &reflogs_owned {
                    if entry.ref_name == REFLOG_HEAD_REF_NAME {
                        let head_key = RefKey::LocalHead {
                            worktree_id: entry.worktree_id.clone(),
                        };
                        let head_target = match ref_table
                            .get(encode_ref_key(&head_key))
                            .map_err(|err| {
                                map_redb_storage_err(
                                    err,
                                    "verify LocalHead for branch HEAD reflog",
                                )
                            })? {
                            Some(g) => Some(decode_ref_target(g.value())?),
                            None => None,
                        };
                        if head_target.as_ref() != Some(&RefTarget::Symbolic(branch_name.clone())) {
                            return Err(StorageBackendError::BackendInternal {
                                detail: "HEAD reflog worktree_id does not point symbolically to the LocalBranch being updated"
                                    .to_string(),
                            });
                        }
                    }
                }
                if publication_owned.is_some() {
                    let mut checked_out = false;
                    let range = ref_table
                        .range((REF_TAG_LOCAL_HEAD_MAIN, "", "")..(REF_TAG_REMOTE_HEAD, "", ""))
                        .map_err(|err| {
                            map_redb_storage_err(
                                err,
                                "scan local HEAD rows for bridge publication verification",
                            )
                        })?;
                    for entry in range {
                        let (_, v) = entry.map_err(|err| {
                            map_redb_storage_err(
                                err,
                                "read local HEAD row during bridge publication verification",
                            )
                        })?;
                        let target = decode_ref_target(v.value())?;
                        if matches!(target, RefTarget::Symbolic(ref b) if b == branch_name) {
                            checked_out = true;
                            break;
                        }
                    }
                    if !checked_out {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "bridge publication requires the updated LocalBranch to be checked out by HEAD"
                                .to_string(),
                        });
                    }
                }
            }

            // Enforce duplicate symbolic HEAD exclusivity across worktrees,
            // matching SQLite's `idx_name_kind`.
            if let (
                RefKey::LocalHead {
                    worktree_id: target_wt,
                },
                Some(RefTarget::Symbolic(branch_name)),
            ) = (&key_owned, &new_target_owned)
            {
                let mut conflict = false;
                let range = ref_table
                    .range((REF_TAG_LOCAL_HEAD_MAIN, "", "")..(REF_TAG_REMOTE_HEAD, "", ""))
                    .map_err(|err| {
                        map_redb_storage_err(err, "scan local HEAD rows for symbolic exclusivity")
                    })?;
                for entry in range {
                    let (k, v) = entry.map_err(|err| {
                        map_redb_storage_err(err, "read local HEAD row during exclusivity check")
                    })?;
                    let other_key = decode_ref_key(k.value())?;
                    if other_key.worktree_id() == target_wt.as_deref() {
                        continue;
                    }
                    let other_target = decode_ref_target(v.value())?;
                    if matches!(other_target, RefTarget::Symbolic(ref b) if b == branch_name) {
                        conflict = true;
                        break;
                    }
                }
                if conflict {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "branch is already checked out as symbolic HEAD in another worktree"
                                .to_string(),
                    });
                }
            }

            // Apply reference mutation.
            match &new_target_owned {
                Some(target) => {
                    let encoded_val = encode_versioned(
                        GIT_REF_CODEC_VERSION,
                        &RefTargetStored {
                            target: target.clone(),
                        },
                    )?;
                    ref_table
                        .insert(encoded_key, encoded_val.as_slice())
                        .map_err(|err| map_redb_storage_err(err, "write reference entry"))?;
                }
                None => {
                    let _ = ref_table
                        .remove(encoded_key)
                        .map_err(|err| map_redb_storage_err(err, "delete reference entry"))?;
                }
            }
            drop(ref_table);

            // Append reflog entries atomically inside the same WriteTransaction.
            if !reflogs_owned.is_empty() {
                let marker = encode_versioned(GIT_REFLOG_INDEX_CODEC_VERSION, &ReflogIndexMarker)?;
                let mut reflog_table = wtx
                    .open_table(REFLOG_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open reflog table for write"))?;
                let mut idx_table = wtx
                    .open_table(REFLOG_BY_REF_INDEX)
                    .map_err(|err| map_redb_table_err(err, "open reflog_by_ref index"))?;
                for entry in reflogs_owned {
                    let id = next_reflog_id(wtx)?;
                    let wt_scope = entry.worktree_id.clone().unwrap_or_default();
                    let ref_name_idx = entry.ref_name.clone();
                    let encoded = encode_versioned(
                        GIT_REFLOG_CODEC_VERSION,
                        &ReflogStoredRow {
                            ref_name: entry.ref_name,
                            old_oid: entry.old_oid,
                            new_oid: entry.new_oid,
                            timestamp: entry.timestamp,
                            committer_name: entry.committer_name,
                            committer_email: entry.committer_email,
                            action: entry.action,
                            message: entry.message,
                            worktree_id: entry.worktree_id,
                        },
                    )?;
                    reflog_table
                        .insert(id, encoded.as_slice())
                        .map_err(|err| map_redb_storage_err(err, "insert reflog entry"))?;
                    idx_table
                        .insert((ref_name_idx.as_str(), wt_scope.as_str(), id), marker.as_slice())
                        .map_err(|err| map_redb_storage_err(err, "insert reflog_by_ref index"))?;
                }
            }

            // Atomically bind `agent_bridge_publication` inside the same WriteTransaction.
            if let Some(pub_rec) = publication_owned {
                let mut pub_table = wtx.open_table(BRIDGE_PUBLICATION_TABLE).map_err(|err| {
                    map_redb_table_err(err, "open agent_bridge_publication table for write")
                })?;
                let existing = match pub_table
                    .get(pub_rec.operation_id.as_str())
                    .map_err(|err| map_redb_storage_err(err, "read agent_bridge_publication"))?
                {
                    Some(guard) => Some(decode_bridge_pub_row(
                        pub_rec.operation_id.clone(),
                        guard.value(),
                    )?),
                    None => None,
                };
                if let Some(prev) = existing
                    && prev.commit_oid != pub_rec.commit_oid
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "bridge operation is already bound to a different commit; refusing to publish"
                                .to_string(),
                    });
                }
                let encoded = encode_versioned(
                    GIT_BRIDGE_PUB_CODEC_VERSION,
                    &BridgePublicationStoredRow {
                        commit_oid: pub_rec.commit_oid,
                        phase: pub_rec.phase,
                    },
                )?;
                pub_table
                    .insert(pub_rec.operation_id.as_str(), encoded.as_slice())
                    .map_err(|err| {
                        map_redb_storage_err(err, "insert agent_bridge_publication entry")
                    })?;
            }

            if abort_before_commit_for_test {
                return Err(StorageBackendError::BackendInternal {
                    detail: "injected pre-commit abort for rollback verification".to_string(),
                });
            }

            Ok((
                true,
                RefCasOutcome::Applied {
                    previous: actual,
                    current: new_target_owned,
                },
            ))
        })
        .await
    }

    #[cfg(all(test, feature = "storage-sqlite"))]
    pub(crate) async fn update_ref_cas_with_bridge_publication_abort_before_commit_for_test(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
        publication: &BridgePublicationRecord,
    ) -> StorageResult<RefCasOutcome> {
        self.update_ref_cas_inner(key, expected, new_target, reflogs, Some(publication), true)
            .await
    }
}

#[async_trait]
impl GitStore for RedbGitStore {
    fn backend(&self) -> StorageBackendKind {
        StorageBackendKind::Redb
    }

    async fn get_ref(&self, key: &RefKey) -> StorageResult<Option<RefTarget>> {
        validate_ref_key(key)?;
        let key_owned = key.clone();
        self.run_read(move |rtx| {
            let table = rtx
                .open_table(REFERENCE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open reference table"))?;
            let Some(guard) = table
                .get(encode_ref_key(&key_owned))
                .map_err(|err| map_redb_storage_err(err, "get reference entry"))?
            else {
                return Ok(None);
            };
            decode_ref_target(guard.value()).map(Some)
        })
        .await
    }

    async fn list_refs(&self, worktree_id: Option<&str>) -> StorageResult<Vec<RefRecord>> {
        validate_worktree_filter(worktree_id)?;
        let wt_owned = worktree_id.map(str::to_string);
        self.run_read(move |rtx| {
            let table = rtx
                .open_table(REFERENCE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open reference table"))?;
            let mut records = Vec::new();
            let iter = table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iterate reference table"))?;
            for entry in iter {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read reference entry"))?;
                let key = decode_ref_key(k.value())?;
                if key.visible_to_worktree(wt_owned.as_deref()) {
                    let target = decode_ref_target(v.value())?;
                    records.push(RefRecord { key, target });
                }
            }
            records.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(records)
        })
        .await
    }

    async fn list_all_local_heads(&self) -> StorageResult<Vec<RefRecord>> {
        self.run_read(move |rtx| {
            let table = rtx
                .open_table(REFERENCE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open reference table"))?;
            let mut records = Vec::new();
            let range = table
                .range((REF_TAG_LOCAL_HEAD_MAIN, "", "")..(REF_TAG_REMOTE_HEAD, "", ""))
                .map_err(|err| map_redb_storage_err(err, "range scan local HEAD rows"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read local HEAD entry"))?;
                let key = decode_ref_key(k.value())?;
                let target = decode_ref_target(v.value())?;
                records.push(RefRecord { key, target });
            }
            records.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(records)
        })
        .await
    }

    async fn update_ref_cas(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
    ) -> StorageResult<RefCasOutcome> {
        self.update_ref_cas_inner(key, expected, new_target, reflogs, None, false)
            .await
    }

    async fn update_ref_cas_with_bridge_publication(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
        publication: &BridgePublicationRecord,
    ) -> StorageResult<RefCasOutcome> {
        self.update_ref_cas_inner(key, expected, new_target, reflogs, Some(publication), false)
            .await
    }

    async fn list_reflog(
        &self,
        ref_name: &str,
        worktree_id: Option<&str>,
        limit: Option<usize>,
    ) -> StorageResult<Vec<ReflogEntry>> {
        if ref_name.is_empty() || ref_name.contains('\0') {
            return Err(StorageBackendError::BackendInternal {
                detail: "reflog ref_name must be non-empty".to_string(),
            });
        }
        validate_worktree_filter(worktree_id)?;
        if ref_name != REFLOG_HEAD_REF_NAME && worktree_id.is_some() {
            return Err(StorageBackendError::BackendInternal {
                detail: "non-HEAD reflog queries must not specify a worktree_id".to_string(),
            });
        }
        let ref_name_owned = ref_name.to_string();
        let wt_scope = worktree_id.unwrap_or_default().to_string();

        self.run_read(move |rtx| {
            let idx_table = rtx
                .open_table(REFLOG_BY_REF_INDEX)
                .map_err(|err| map_redb_table_err(err, "open reflog_by_ref index"))?;
            let reflog_table = rtx
                .open_table(REFLOG_TABLE)
                .map_err(|err| map_redb_table_err(err, "open reflog table"))?;

            let mut out = Vec::new();
            let range = idx_table
                .range(
                    (ref_name_owned.as_str(), wt_scope.as_str(), 0u64)
                        ..=(ref_name_owned.as_str(), wt_scope.as_str(), u64::MAX),
                )
                .map_err(|err| map_redb_storage_err(err, "range scan reflog_by_ref index"))?;
            // Iterate newest-first (`id DESC`, matching SQLite `ORDER BY id DESC`).
            for entry in range.rev() {
                if let Some(max) = limit
                    && out.len() >= max
                {
                    break;
                }
                let (k, _) = entry
                    .map_err(|err| map_redb_storage_err(err, "read reflog_by_ref index entry"))?;
                let id = k.value().2;
                let row = reflog_table
                    .get(id)
                    .map_err(|err| map_redb_storage_err(err, "read reflog primary row"))?
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "reflog_by_ref index references missing reflog primary row"
                            .to_string(),
                    })?;
                out.push(decode_reflog_row(id, row.value())?);
            }
            Ok(out)
        })
        .await
    }

    async fn prepare_bridge_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<()> {
        if !is_valid_bridge_operation_id(operation_id) {
            return Err(StorageBackendError::BackendInternal {
                detail: "bridge publication operation_id is empty or contains invalid characters"
                    .to_string(),
            });
        }
        if commit_oid.is_empty() || commit_oid.contains('\0') {
            return Err(StorageBackendError::BackendInternal {
                detail: "bridge publication commit_oid must be non-empty".to_string(),
            });
        }
        let op_owned = operation_id.to_string();
        let oid_owned = commit_oid.to_string();

        self.run_write_conditional(move |wtx| {
            let mut pub_table = wtx.open_table(BRIDGE_PUBLICATION_TABLE).map_err(|err| {
                map_redb_table_err(err, "open agent_bridge_publication table for prepare")
            })?;
            let existing = match pub_table
                .get(op_owned.as_str())
                .map_err(|err| map_redb_storage_err(err, "read agent_bridge_publication"))?
            {
                Some(guard) => Some(decode_bridge_pub_row(op_owned.clone(), guard.value())?),
                None => None,
            };
            match existing {
                Some(prev) if prev.commit_oid == oid_owned => Ok((false, ())),
                Some(_) => Err(StorageBackendError::BackendInternal {
                    detail: "bridge operation is already bound to a different commit".to_string(),
                }),
                None => {
                    let encoded = encode_versioned(
                        GIT_BRIDGE_PUB_CODEC_VERSION,
                        &BridgePublicationStoredRow {
                            commit_oid: oid_owned,
                            phase: BridgePublicationPhase::Prepared,
                        },
                    )?;
                    pub_table
                        .insert(op_owned.as_str(), encoded.as_slice())
                        .map_err(|err| {
                            map_redb_storage_err(err, "insert prepared bridge publication")
                        })?;
                    Ok((true, ()))
                }
            }
        })
        .await
    }

    async fn get_bridge_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgePublicationRecord>> {
        if operation_id.is_empty() || operation_id.contains('\0') {
            return Ok(None);
        }
        let op_owned = operation_id.to_string();
        self.run_read(move |rtx| {
            let table = rtx
                .open_table(BRIDGE_PUBLICATION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open agent_bridge_publication table"))?;
            let Some(guard) = table
                .get(op_owned.as_str())
                .map_err(|err| map_redb_storage_err(err, "read agent_bridge_publication"))?
            else {
                return Ok(None);
            };
            decode_bridge_pub_row(op_owned, guard.value()).map(Some)
        })
        .await
    }

    async fn list_bridge_publications(&self) -> StorageResult<Vec<BridgePublicationRecord>> {
        self.run_read(move |rtx| {
            let table = rtx
                .open_table(BRIDGE_PUBLICATION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open agent_bridge_publication table"))?;
            let mut out = Vec::new();
            let iter = table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iterate agent_bridge_publication"))?;
            for entry in iter {
                let (k, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read bridge publication entry"))?;
                out.push(decode_bridge_pub_row(k.value().to_string(), v.value())?);
            }
            Ok(out)
        })
        .await
    }
}
