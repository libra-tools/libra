//! AI workflow history persistence backed by an orphan Git branch.
//!
//! Libra records every AI process artefact (Intent, Task, Run, Plan,
//! PatchSet, Evidence, ToolInvocation, Provenance, Decision, ContextFrame,
//! ...) on a parallel branch named [`AI_REF`] (`libra/intent`). The branch
//! is *orphan*: it shares no history with the user's code branches but
//! lives inside the same object database, which means:
//!
//! * The same `git gc` policy keeps both AI history and code history
//!   reachable.
//! * AI artefacts are content-addressed under standard Git rules and can be
//!   transferred via the same protocol as the rest of the repository.
//!
//! Each commit on this ref points to a tree that is partitioned by object
//! type (`intent/`, `task/`, `plan/`, ...), with one blob per object id
//! beneath the type subtree. The flow for `append` is:
//!
//! 1. Read the current head (with retry on a busy SQLite) — see
//!    [`HistoryManager::resolve_history_head`].
//! 2. Load that head's root tree, splice the new entry in beneath its type
//!    subtree, write a fresh root tree, and create a child commit — see
//!    [`HistoryManager::create_append_commit`].
//! 3. Compare-and-swap the ref forward, retrying on a stale head — see
//!    [`HistoryManager::update_ref_if_matches`].
//!
//! Concurrency is handled via two retry loops: a SQLite-busy retry that
//! covers transient lock contention, and a head-conflict retry that re-reads
//! the head and retries the splice when another process advanced the ref.
//! Both loops have bounded iteration counts so misuse cannot deadlock the
//! caller.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    str::FromStr,
    sync::{Arc, OnceLock, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
#[cfg(test)]
use git_internal::internal::object::types::ObjectType;
use git_internal::{
    hash::{ObjectHash, get_hash_kind},
    internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr,
    EntityTrait, QueryFilter, QueryResult, Set, SqlErr, Statement, TransactionTrait, Value,
    sea_query::Expr,
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::sleep,
};

#[cfg(test)]
use crate::utils::storage::tiered::verify_fetched_object;
use crate::{
    internal::{
        ai::observed_agents::RedactedBytes,
        model::reference::{self, ConfigKind},
    },
    utils::{
        object::{
            git_object_hash, read_git_object, read_git_object_bounded_validated, write_git_object,
            write_git_object_with_status,
        },
        storage::Storage,
    },
};

/// Default Git reference for the AI history orphan branch.
///
/// All AI process objects (Intent, Task, Run, Plan, PatchSet, Evidence,
/// ToolInvocation, Provenance, Decision) live on this single branch,
/// running in parallel with the normal code branch (`refs/heads/*`).
///
/// By keeping AI objects reachable from this ref, they are protected
/// from `git gc` — the branch acts as a GC root.
///
/// In the database, this is stored with kind='Branch' and name='libra/intent'.
pub const AI_REF: &str = "libra/intent";
/// Maximum attempts to retry a SQLite operation that returns a transient
/// "database is locked" error before propagating the failure.
const SQLITE_BUSY_MAX_RETRIES: usize = 15;
/// Base delay (ms) for the linear backoff applied between SQLite-busy retries.
/// The actual delay is `BASE * attempt`, so the worst-case wait is roughly
/// `BASE * SUM(1..=MAX_RETRIES)` which keeps total time bounded.
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 100;
/// Maximum attempts to re-read the history head and retry a splice when a
/// concurrent writer advances the ref between read and CAS. The bound is
/// generous because each retry is purely local (no network I/O).
const HISTORY_HEAD_CONFLICT_MAX_RETRIES: usize = 32;
const REJECTED_CLEANUP_MAX_VISITED_OBJECTS: usize = 250_000;
const REJECTED_CLEANUP_MAX_TRAVERSAL_DURATION: Duration = Duration::from_secs(30);
const OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET: Duration = Duration::from_millis(500);
const OBJECT_INDEX_CLEANUP_DRAIN_BUDGET: Duration = Duration::from_secs(5);
const REJECTED_CLEANUP_MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const REJECTED_CLEANUP_MAX_TOTAL_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const REJECTED_CLEANUP_MAX_INDEX_FILES: usize = 256;
const REJECTED_CLEANUP_INDEX_HELPER_FRAME_CAP: u64 = 64 * 1024 * 1024;
pub const REJECTED_CLEANUP_INDEX_HELPER_ARG: &str =
    "--libra-internal-rejected-cleanup-index-helper";
pub const CHECKPOINT_OBJECT_IO_HELPER_ARG: &str = "--libra-internal-checkpoint-object-io-helper";
pub const CHECKPOINT_OBJECT_IO_HELPER_INPUT_CAP: u64 = 32 * 1024 * 1024;
pub const CHECKPOINT_OBJECT_IO_HELPER_OUTPUT_CAP: u64 = 32 * 1024 * 1024;
const CHECKPOINT_OBJECT_READ_MAX_INFLATED_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
tokio::task_local! {
    static TEST_CHECKPOINT_SNAPSHOT_VERIFY_COUNT: std::cell::Cell<usize>;
}

#[cfg(test)]
pub(crate) async fn count_checkpoint_snapshot_verifications<F: std::future::Future>(
    future: F,
) -> (F::Output, usize) {
    TEST_CHECKPOINT_SNAPSHOT_VERIFY_COUNT
        .scope(std::cell::Cell::new(0), async move {
            let output = future.await;
            let count = TEST_CHECKPOINT_SNAPSHOT_VERIFY_COUNT.with(std::cell::Cell::get);
            (output, count)
        })
        .await
}

fn rejected_cleanup_traversal_duration() -> Duration {
    if cfg!(debug_assertions)
        && let Ok(value) = std::env::var("LIBRA_TEST_REJECTED_CLEANUP_DEADLINE_MS")
        && let Ok(milliseconds) = value.parse::<u64>()
        && milliseconds > 0
        && milliseconds <= REJECTED_CLEANUP_MAX_TRAVERSAL_DURATION.as_millis() as u64
    {
        return Duration::from_millis(milliseconds);
    }
    REJECTED_CLEANUP_MAX_TRAVERSAL_DURATION
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RejectedCleanupDbSnapshot {
    markers: Vec<TracesInflightMarker>,
    candidates: HashSet<String>,
    graph_roots: Vec<String>,
    active_operations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RejectedCleanupRootSnapshot {
    db: RejectedCleanupDbSnapshot,
    index_roots: HashSet<String>,
    index_fingerprints: Vec<(String, String)>,
    active_operations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RejectedCleanupIndexSnapshot {
    roots: HashSet<String>,
    fingerprints: Vec<(String, String)>,
    active_operations: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RejectedCleanupIndexHelperRequest {
    repo_path: PathBuf,
    hash_bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct RejectedCleanupIndexHelperResponse {
    snapshot: Option<RejectedCleanupIndexSnapshot>,
    error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CheckpointObjectIoHelperRequest {
    /// Standard-base64 encoding of the native path bytes (UTF-8 off Unix).
    repo_path_base64: String,
    operation: CheckpointObjectIoOperation,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CheckpointObjectIoOperation {
    Read {
        oid: String,
        expected_type: String,
    },
    Write {
        object_type: String,
        data_base64: String,
    },
    VerifySnapshot {
        head: String,
        cataloged_commits: Vec<String>,
        checkpoints: Vec<CheckpointDurabilityHelperSpec>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckpointDurabilityHelperSpec {
    checkpoint_id: String,
    traces_commit: String,
    tree_oid: String,
    metadata_blob_oid: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CheckpointObjectIoHelperResponse {
    Read {
        oid: String,
        object_type: String,
        data_base64: String,
    },
    Written {
        oid: String,
        was_created: bool,
    },
    Verified {
        oids: Vec<String>,
    },
    Error {
        message: String,
    },
}

struct CleanupHelperChild {
    child: Option<Child>,
    reaper: mpsc::Sender<Child>,
}

impl CleanupHelperChild {
    fn new(child: Child, reaper: mpsc::Sender<Child>) -> Self {
        Self {
            child: Some(child),
            reaper,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        // INVARIANT: the guard owns its child until Drop; no method removes it.
        self.child
            .as_mut()
            .expect("cleanup helper child remains owned by its reap guard")
    }
}

impl Drop for CleanupHelperChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                let _ = child.kill();
                // Waiting here would let a helper stuck in uninterruptible
                // filesystem I/O extend the foreground command past its
                // absolute deadline. The process-wide nonblocking reaper
                // owns the Child until try_wait observes and reaps its exit.
                if let Err(error) = self.reaper.send(child) {
                    let mut child = error.0;
                    let _ = child.try_wait();
                }
            }
        }
    }
}

static CLEANUP_HELPER_REAPER: OnceLock<Result<mpsc::Sender<Child>, String>> = OnceLock::new();

#[cfg(test)]
static CLEANUP_HELPER_REAPED_CHILDREN: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn cleanup_helper_reaper_sender() -> Result<mpsc::Sender<Child>> {
    let result = CLEANUP_HELPER_REAPER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Child>();
        std::thread::Builder::new()
            .name("libra-cleanup-helper-reaper".to_string())
            .spawn(move || {
                let mut children = Vec::<Child>::new();
                loop {
                    match receiver.recv_timeout(Duration::from_millis(25)) {
                        Ok(child) => children.push(child),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) if children.is_empty() => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {}
                    }
                    while let Ok(child) = receiver.try_recv() {
                        children.push(child);
                    }
                    let mut index = 0;
                    while index < children.len() {
                        match children[index].try_wait() {
                            Ok(Some(_)) => {
                                let mut reaped = children.swap_remove(index);
                                // `try_wait` above observed and reaped this
                                // process. Calling `wait` returns the cached
                                // status immediately and makes that lifecycle
                                // explicit to Clippy's zombie-process audit.
                                let _ = reaped.wait();
                                #[cfg(test)]
                                CLEANUP_HELPER_REAPED_CHILDREN
                                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }
                            Ok(None) | Err(_) => index += 1,
                        }
                    }
                }
            })
            .map(|_| sender)
            .map_err(|error| format!("start cleanup helper reaper thread: {error}"))
    });
    result
        .as_ref()
        .cloned()
        .map_err(|message| anyhow!(message.clone()))
}

/// Exact ownership identity for one traces writer marker. Checkpoint IDs are
/// intentionally stable across retries, so the random generation is what
/// prevents an expired writer from adopting a takeover writer's replacement
/// marker under the same metadata key.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TracesWriterFence {
    session_id: String,
    attempt_id: String,
    generation: String,
}

/// Outcome of a compare-and-swap reference update.
///
/// Used by [`HistoryManager::update_ref_if_matches`] to communicate whether
/// the ref moved successfully (`Updated`) or whether the expected head was
/// stale and the caller must restart the splice (`HeadChanged`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefUpdateOutcome {
    /// The ref was atomically advanced to the new commit.
    Updated,
    /// Another writer advanced the ref before our CAS — caller should
    /// re-read the head and rebuild the commit on top of it.
    HeadChanged,
}

/// Detect transient SQLite contention that should trigger a retry.
///
/// Functional scope:
/// - Inspects the error message for the well-known "database is locked" or
///   "database schema is locked" substrings emitted by SQLite under busy
///   contention.
///
/// Boundary conditions:
/// - This is intentionally a string match: the SeaORM error wraps the
///   underlying SQLite text, and there is no stable error-code variant for
///   busy/lock conditions in the wrapping layer.
fn is_sqlite_busy(err: &DbErr) -> bool {
    let message = err.to_string();
    message.contains("database is locked") || message.contains("database schema is locked")
}

fn anyhow_is_sqlite_busy(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<DbErr>())
        .any(is_sqlite_busy)
}

/// Detect unique-constraint violations on the `reference` table.
///
/// Functional scope:
/// - Used by the optimistic CAS path: when two writers race to insert the
///   same ref name, one will see a unique-constraint violation; we treat
///   that as a `HeadChanged` outcome rather than a hard error.
fn is_sqlite_unique_violation(err: &DbErr) -> bool {
    matches!(err.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
}

fn read_cleanup_regular_file(
    path: &Path,
    per_file_limit: u64,
    aggregate_remaining: u64,
    what: &str,
) -> Result<Option<Vec<u8>>> {
    read_cleanup_regular_file_inner(path, per_file_limit, aggregate_remaining, what, || {})
}

fn read_cleanup_regular_file_inner<F: FnOnce()>(
    path: &Path,
    per_file_limit: u64,
    aggregate_remaining: u64,
    what: &str,
    after_metadata: F,
) -> Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;

        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    };
    #[cfg(not(unix))]
    let opened = fs::File::open(path);
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("open {what} without following symlinks"));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect held {what} descriptor"))?;
    if !metadata.file_type().is_file() {
        bail!("{what} is not a regular file");
    }
    let limit = per_file_limit.min(aggregate_remaining);
    if metadata.len() > limit {
        bail!("{what} exceeds the {limit} byte cleanup read limit");
    }
    after_metadata();
    let mut bytes = Vec::new();
    (&file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read held {what} descriptor"))?;
    if bytes.len() as u64 > limit {
        bail!("{what} grew beyond the {limit} byte cleanup read limit");
    }
    Ok(Some(bytes))
}

fn parse_cleanup_index_roots(
    bytes: &[u8],
    hash_bytes: usize,
    what: &str,
) -> Result<HashSet<String>> {
    if !matches!(hash_bytes, 20 | 32) {
        bail!("{what} uses unsupported {hash_bytes}-byte object ids");
    }
    if bytes.len() < 12 + hash_bytes || &bytes[..4] != b"DIRC" {
        bail!("{what} has an invalid index header");
    }
    let read_u32 = |offset: usize| -> Result<u32> {
        let raw = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| anyhow!("{what} is truncated"))?;
        let raw: [u8; 4] = raw
            .try_into()
            .map_err(|_| anyhow!("{what} has an invalid integer field"))?;
        Ok(u32::from_be_bytes(raw))
    };
    if read_u32(4)? != 2 {
        bail!("{what} is not a supported version-2 index");
    }
    let entry_count = usize::try_from(read_u32(8)?)
        .with_context(|| format!("{what} entry count exceeds this platform"))?;
    let checksum_start = bytes.len() - hash_bytes;
    let expected_checksum = &bytes[checksum_start..];
    let checksum_matches = match hash_bytes {
        20 => {
            use sha1::Digest as _;
            sha1::Sha1::digest(&bytes[..checksum_start]).as_slice() == expected_checksum
        }
        32 => {
            use sha2::Digest as _;
            sha2::Sha256::digest(&bytes[..checksum_start]).as_slice() == expected_checksum
        }
        _ => false,
    };
    if !checksum_matches {
        bail!("{what} checksum does not match the held file bytes");
    }

    let minimum_entry_bytes = 40_usize
        .checked_add(hash_bytes)
        .and_then(|size| size.checked_add(3))
        .ok_or_else(|| anyhow!("{what} entry size overflow"))?;
    if entry_count > checksum_start.saturating_sub(12) / minimum_entry_bytes {
        bail!("{what} entry count exceeds its bounded file size");
    }
    let mut roots = HashSet::new();
    let mut cursor = 12_usize;
    for _ in 0..entry_count {
        let entry_start = cursor;
        let hash_start = cursor
            .checked_add(40)
            .ok_or_else(|| anyhow!("{what} entry offset overflow"))?;
        let hash_end = hash_start
            .checked_add(hash_bytes)
            .ok_or_else(|| anyhow!("{what} object id offset overflow"))?;
        let flags_end = hash_end
            .checked_add(2)
            .ok_or_else(|| anyhow!("{what} flags offset overflow"))?;
        if flags_end > checksum_start {
            bail!("{what} entry is truncated");
        }
        roots.insert(hex::encode(&bytes[hash_start..hash_end]));
        let flags = u16::from_be_bytes([bytes[hash_end], bytes[hash_end + 1]]);
        let declared_name_len = usize::from(flags & 0x0fff);
        let name_start = flags_end;
        let name_end = if declared_name_len == 0x0fff {
            bytes[name_start..checksum_start]
                .iter()
                .position(|byte| *byte == 0)
                .map(|offset| name_start + offset)
                .ok_or_else(|| anyhow!("{what} long path has no terminator"))?
        } else {
            let end = name_start
                .checked_add(declared_name_len)
                .ok_or_else(|| anyhow!("{what} path length overflow"))?;
            if end >= checksum_start || bytes[end] != 0 {
                bail!("{what} path is truncated or not NUL-terminated");
            }
            end
        };
        cursor = name_end + 1;
        while !(cursor - entry_start).is_multiple_of(8) {
            if cursor >= checksum_start || bytes[cursor] != 0 {
                bail!("{what} entry padding is invalid");
            }
            cursor += 1;
        }
    }

    while cursor < checksum_start {
        let header_end = cursor
            .checked_add(8)
            .ok_or_else(|| anyhow!("{what} extension offset overflow"))?;
        if header_end > checksum_start {
            bail!("{what} extension header is truncated");
        }
        if !bytes[cursor].is_ascii_uppercase() {
            bail!("{what} contains an unsupported required index extension");
        }
        let size = usize::try_from(read_u32(cursor + 4)?)
            .with_context(|| format!("{what} extension size exceeds this platform"))?;
        cursor = header_end
            .checked_add(size)
            .ok_or_else(|| anyhow!("{what} extension size overflow"))?;
        if cursor > checksum_start {
            bail!("{what} extension payload is truncated");
        }
    }
    Ok(roots)
}

fn collect_rejected_cleanup_index_snapshot(
    repo_path: &Path,
    hash_bytes: usize,
) -> Result<RejectedCleanupIndexSnapshot> {
    use sha2::{Digest, Sha256};

    let mut index_paths = vec![repo_path.join("index")];
    let registry_path = repo_path.join("worktrees.json");
    let mut fingerprints = Vec::new();
    let mut total_bytes = 0_u64;
    let registry_what = "worktree registry before rejected object cleanup";
    if let Some(bytes) = read_cleanup_regular_file(
        &registry_path,
        REJECTED_CLEANUP_MAX_INDEX_BYTES,
        REJECTED_CLEANUP_MAX_TOTAL_INDEX_BYTES,
        registry_what,
    )? {
        total_bytes = bytes.len() as u64;
        fingerprints.push((
            registry_path.to_string_lossy().into_owned(),
            hex::encode(Sha256::digest(&bytes)),
        ));
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .context("parse worktree registry before rejected object cleanup")?;
        let worktrees = document
            .get("worktrees")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow!("worktree registry has no worktrees array"))?;
        if worktrees.len() > REJECTED_CLEANUP_MAX_INDEX_FILES {
            bail!(
                "worktree registry exceeds the {} index-file cleanup limit",
                REJECTED_CLEANUP_MAX_INDEX_FILES
            );
        }
        for worktree in worktrees {
            let path = worktree
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("worktree registry entry has no path"))?;
            index_paths.push(Path::new(path).join(".libra/index"));
        }
    }
    index_paths.sort();
    index_paths.dedup();

    let mut index_roots = HashSet::new();
    for index_path in index_paths {
        let remaining = REJECTED_CLEANUP_MAX_TOTAL_INDEX_BYTES.saturating_sub(total_bytes);
        let what = format!("worktree index '{}'", index_path.display());
        let Some(bytes) = read_cleanup_regular_file(
            &index_path,
            REJECTED_CLEANUP_MAX_INDEX_BYTES,
            remaining,
            &what,
        )?
        else {
            continue;
        };
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| anyhow!("worktree index cleanup input size overflow"))?;
        fingerprints.push((
            index_path.to_string_lossy().into_owned(),
            hex::encode(Sha256::digest(&bytes)),
        ));
        index_roots.extend(parse_cleanup_index_roots(&bytes, hash_bytes, &what)?);
    }
    fingerprints.sort();

    let mut active_operations = Vec::new();
    for name in [
        "rebase-merge",
        "rebase-apply",
        "merge-state.json",
        "merge-autostash.json",
        "revert-state.json",
    ] {
        match fs::symlink_metadata(repo_path.join(name)) {
            Ok(_) => active_operations.push(name.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect repository operation state '{name}'"));
            }
        }
    }

    Ok(RejectedCleanupIndexSnapshot {
        roots: index_roots,
        fingerprints,
        active_operations,
    })
}

pub fn run_rejected_cleanup_index_helper(input: &[u8]) -> Result<Vec<u8>> {
    let request: RejectedCleanupIndexHelperRequest =
        serde_json::from_slice(input).context("decode rejected-cleanup index helper request")?;
    let response =
        match collect_rejected_cleanup_index_snapshot(&request.repo_path, request.hash_bytes) {
            Ok(snapshot) => RejectedCleanupIndexHelperResponse {
                snapshot: Some(snapshot),
                error: None,
            },
            Err(error) => RejectedCleanupIndexHelperResponse {
                snapshot: None,
                error: Some(format!("{error:#}")),
            },
        };
    serde_json::to_vec(&response).context("encode rejected-cleanup index helper response")
}

fn encode_checkpoint_object_path(path: &Path) -> Result<String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        Ok(STANDARD.encode(path.as_os_str().as_bytes()))
    }
    #[cfg(not(unix))]
    {
        let text = path
            .to_str()
            .ok_or_else(|| anyhow!("checkpoint object store path is not valid platform text"))?;
        Ok(STANDARD.encode(text.as_bytes()))
    }
}

fn decode_checkpoint_object_path(encoded: &str) -> Result<PathBuf> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let bytes = STANDARD
        .decode(encoded)
        .context("decode checkpoint object store path")?;
    #[cfg(unix)]
    {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        Ok(PathBuf::from(OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        let text =
            String::from_utf8(bytes).context("checkpoint object store path is not valid UTF-8")?;
        Ok(PathBuf::from(text))
    }
}

/// Execute one checkpoint object read/write in the private helper process.
/// Errors are encoded in the response so the parent receives an actionable
/// cause while still treating malformed private frames as a hard helper
/// failure.
pub fn run_checkpoint_object_io_helper(input: &[u8]) -> Result<Vec<u8>> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let request: CheckpointObjectIoHelperRequest =
        serde_json::from_slice(input).context("decode checkpoint object-I/O helper request")?;
    let response = match decode_checkpoint_object_path(&request.repo_path_base64) {
        Err(error) => CheckpointObjectIoHelperResponse::Error {
            message: format!("invalid checkpoint object store path: {error:#}"),
        },
        Ok(repo_path) => match request.operation {
            CheckpointObjectIoOperation::Read { oid, expected_type } => {
                if !matches!(expected_type.as_str(), "tree" | "commit") {
                    CheckpointObjectIoHelperResponse::Error {
                        message: format!("unsupported checkpoint read type '{expected_type}'"),
                    }
                } else {
                    match ObjectHash::from_str(&oid) {
                        Err(error) => CheckpointObjectIoHelperResponse::Error {
                            message: format!("invalid checkpoint object id '{oid}': {error}"),
                        },
                        Ok(parsed_oid) => match read_git_object_bounded_validated(
                            &repo_path,
                            &parsed_oid,
                            CHECKPOINT_OBJECT_READ_MAX_INFLATED_BYTES,
                        ) {
                            Ok((object_type, data)) if object_type == expected_type => {
                                CheckpointObjectIoHelperResponse::Read {
                                    oid,
                                    object_type,
                                    data_base64: STANDARD.encode(data),
                                }
                            }
                            Ok((object_type, _)) => CheckpointObjectIoHelperResponse::Error {
                                message: format!(
                                    "checkpoint object {parsed_oid} has type '{object_type}', expected '{expected_type}'"
                                ),
                            },
                            Err(error) => CheckpointObjectIoHelperResponse::Error {
                                message: format!(
                                    "failed to read checkpoint object {parsed_oid}: {error}"
                                ),
                            },
                        },
                    }
                }
            }
            CheckpointObjectIoOperation::Write {
                object_type,
                data_base64,
            } => {
                if !matches!(object_type.as_str(), "blob" | "tree" | "commit") {
                    CheckpointObjectIoHelperResponse::Error {
                        message: format!("unsupported checkpoint object type '{object_type}'"),
                    }
                } else {
                    match STANDARD.decode(data_base64) {
                        Err(error) => CheckpointObjectIoHelperResponse::Error {
                            message: format!("invalid checkpoint object payload: {error}"),
                        },
                        Ok(data) => {
                            if cfg!(debug_assertions)
                                && let Some(ready) = std::env::var_os(
                                    "LIBRA_TEST_CHECKPOINT_OBJECT_WRITE_READY_FILE",
                                )
                            {
                                let _ = std::fs::write(ready, b"ready");
                                loop {
                                    std::thread::park();
                                }
                            }
                            match write_git_object_with_status(&repo_path, &object_type, &data) {
                                Ok((oid, was_created)) => {
                                    CheckpointObjectIoHelperResponse::Written {
                                        oid: oid.to_string(),
                                        was_created,
                                    }
                                }
                                Err(error) => CheckpointObjectIoHelperResponse::Error {
                                    message: format!(
                                        "failed to write checkpoint {object_type} object: {error}"
                                    ),
                                },
                            }
                        }
                    }
                }
            }
            CheckpointObjectIoOperation::VerifySnapshot {
                head,
                cataloged_commits,
                checkpoints,
            } => {
                if cfg!(debug_assertions)
                    && let Some(ready) = std::env::var_os("LIBRA_TEST_CHECKPOINT_VERIFY_READY_FILE")
                {
                    let _ = std::fs::write(ready, b"ready");
                    loop {
                        std::thread::park();
                    }
                }
                let specs = checkpoints
                    .iter()
                    .map(|checkpoint| CheckpointDurabilitySpec {
                        checkpoint_id: &checkpoint.checkpoint_id,
                        traces_commit: &checkpoint.traces_commit,
                        tree_oid: &checkpoint.tree_oid,
                        metadata_blob_oid: &checkpoint.metadata_blob_oid,
                    })
                    .collect::<Vec<_>>();
                match parse_cataloged_traces_commits(&cataloged_commits).and_then(
                    |cataloged_commits| {
                        let head = ObjectHash::from_str(&head)
                            .map_err(|error| anyhow!("invalid traces snapshot head: {error}"))?;
                        checkpoint_snapshot_durable_oids_from_head(
                            &repo_path,
                            head,
                            &cataloged_commits,
                            &specs,
                            None,
                        )
                    },
                ) {
                    Ok(oids) => {
                        let mut oids = oids.into_iter().collect::<Vec<_>>();
                        oids.sort();
                        CheckpointObjectIoHelperResponse::Verified { oids }
                    }
                    Err(error) => CheckpointObjectIoHelperResponse::Error {
                        message: format!("checkpoint snapshot is not durable: {error:#}"),
                    },
                }
            }
        },
    };
    serde_json::to_vec(&response).context("encode checkpoint object-I/O helper response")
}

fn terminate_checkpoint_object_helper(
    mut child: tokio::process::Child,
    stdout_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) {
    stdout_task.abort();
    let _ = child.start_kill();
    // A helper blocked in kernel filesystem I/O may not become reapable
    // immediately after SIGKILL. Keep ownership in a detached task so the
    // foreground deadline never waits for that transition.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
}

async fn invoke_checkpoint_object_helper(
    repo_path: &Path,
    operation: CheckpointObjectIoOperation,
    deadline: Instant,
) -> Result<CheckpointObjectIoHelperResponse> {
    let request = CheckpointObjectIoHelperRequest {
        repo_path_base64: encode_checkpoint_object_path(repo_path)?,
        operation,
    };
    let frame = serde_json::to_vec(&request).context("encode checkpoint object-I/O request")?;
    if frame.len() as u64 > CHECKPOINT_OBJECT_IO_HELPER_INPUT_CAP {
        bail!(
            "checkpoint object-I/O request exceeds the {}-byte helper limit",
            CHECKPOINT_OBJECT_IO_HELPER_INPUT_CAP
        );
    }
    if Instant::now() >= deadline {
        bail!("checkpoint object I/O exceeded its command deadline");
    }

    let program = std::env::current_exe().context("resolve checkpoint object-I/O helper")?;
    let mut child = tokio::process::Command::new(program)
        .arg(CHECKPOINT_OBJECT_IO_HELPER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start checkpoint object-I/O helper")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("checkpoint object-I/O helper has no stdin pipe"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("checkpoint object-I/O helper has no stdout pipe"))?;
    let stdout_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout
            .take(CHECKPOINT_OBJECT_IO_HELPER_OUTPUT_CAP.saturating_add(1))
            .read_to_end(&mut bytes)
            .await?;
        Ok(bytes)
    });
    let send_result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        stdin.write_all(&frame).await?;
        stdin.shutdown().await
    })
    .await;
    match send_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            terminate_checkpoint_object_helper(child, stdout_task);
            return Err(error).context("send checkpoint object-I/O request");
        }
        Err(_) => {
            terminate_checkpoint_object_helper(child, stdout_task);
            bail!("checkpoint object I/O exceeded its command deadline");
        }
    }
    drop(stdin);

    let status =
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), child.wait()).await
        {
            Ok(result) => result.context("wait for checkpoint object-I/O helper")?,
            Err(_) => {
                terminate_checkpoint_object_helper(child, stdout_task);
                bail!("checkpoint object I/O exceeded its command deadline");
            }
        };
    let response_bytes =
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), stdout_task)
            .await
            .map_err(|_| anyhow!("checkpoint object I/O exceeded its command deadline"))?
            .context("join checkpoint object-I/O response reader")?
            .context("read checkpoint object-I/O response")?;
    if !status.success() || response_bytes.len() as u64 > CHECKPOINT_OBJECT_IO_HELPER_OUTPUT_CAP {
        bail!("checkpoint object-I/O helper returned an invalid response");
    }
    serde_json::from_slice(&response_bytes).context("decode checkpoint object-I/O response")
}

/// Manages object history using an orphan branch and Git Tree structure.
///
/// The default branch (`libra/intent`) stores **all** AI workflow objects,
/// running in parallel with the normal code history (`refs/heads/*`).
/// This is initialised during `libra init` so both branches exist from the start.
///
/// Structure (Commit -> Tree):
///   ├── intent/
///   │   └── <intent_id>
///   ├── task/
///   │   └── <task_id>
///   ├── run/
///   │   └── <run_id>
///   ├── plan/
///   │   └── <plan_id>
///   └── …
///
/// The manager is cheap to clone (all state lives behind `Arc` or owned
/// `String`/`PathBuf`) and is safe to share across async tasks. Concurrent
/// `append` calls on the same manager are serialised via the SQLite-side
/// CAS in [`Self::update_ref_if_matches`].
pub struct HistoryManager {
    #[allow(dead_code)]
    storage: Arc<dyn Storage + Send + Sync>,
    repo_path: PathBuf,
    db_conn: Arc<DatabaseConnection>,
    /// The reference name this manager writes to (e.g. "libra/intent").
    ref_name: String,
    /// Test-only injection point: runs right after the checkpoint CAS loop
    /// reads the head, BEFORE objects are spliced/committed against it —
    /// the deterministic window for `ref_cas_head_changed_rebuilds_commit_
    /// before_retry` to move the head under a competing writer.
    #[cfg(test)]
    pub(crate) test_after_head_read:
        Option<Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<()>> + Send + Sync>>,
}

impl HistoryManager {
    /// Build a manager bound to the canonical [`AI_REF`].
    ///
    /// Functional scope:
    /// - Convenience constructor that delegates to [`Self::new_with_ref`]
    ///   with the standard `libra/intent` branch.
    pub fn new(
        storage: Arc<dyn Storage + Send + Sync>,
        repo_path: PathBuf,
        db_conn: Arc<DatabaseConnection>,
    ) -> Self {
        Self::new_with_ref(storage, repo_path, db_conn, AI_REF)
    }

    /// Build a manager bound to an arbitrary ref name.
    ///
    /// Functional scope:
    /// - Used by tests and tooling that need to write a parallel AI history
    ///   under a custom ref (e.g. for staging, comparison, or namespace
    ///   isolation).
    ///
    /// Boundary conditions:
    /// - The ref name is not validated here; callers must ensure it is a
    ///   legal Git ref. The CAS path will fail loudly if the database
    ///   constraint rejects it.
    pub fn new_with_ref(
        storage: Arc<dyn Storage + Send + Sync>,
        repo_path: PathBuf,
        db_conn: Arc<DatabaseConnection>,
        ref_name: impl Into<String>,
    ) -> Self {
        Self {
            storage,
            repo_path,
            db_conn,
            ref_name: ref_name.into(),
            #[cfg(test)]
            test_after_head_read: None,
        }
    }

    /// Hand back a clone of the underlying SeaORM connection.
    ///
    /// Functional scope:
    /// - Convenience accessor for callers that need to issue auxiliary
    ///   queries against the same database (e.g. listing references for the
    ///   TUI) without having to thread a separate `Arc` around.
    pub fn database_connection(&self) -> DatabaseConnection {
        self.db_conn.as_ref().clone()
    }

    /// Initialise the AI orphan branch with an empty tree commit.
    ///
    /// This should be called once during `libra init` so that the AI ref
    /// exists from the start (parallel to `refs/heads/<branch>`).
    /// If the ref already exists this is a no-op.
    ///
    /// Functional scope:
    /// - Writes a single empty-tree commit and points the ref at it. The
    ///   commit has no parents (it is the root of the orphan branch) and
    ///   uses the canonical `Libra <ai@libra>` signatures so authorship is
    ///   traceable.
    ///
    /// Boundary conditions:
    /// - Returns early if the ref already exists; this makes the call
    ///   idempotent and safe to invoke from `libra init` regardless of
    ///   whether previous initialisations completed.
    /// - Surfaces errors from object serialisation, blob writing, or the
    ///   ref CAS so the caller can present an actionable message.
    pub async fn init_branch(&self) -> Result<()> {
        // Already initialised — nothing to do.
        if self.resolve_history_head().await?.is_some() {
            return Ok(());
        }

        // Write an empty tree.
        let empty_tree_hash = self.write_tree(&[])?;

        let author = Signature::new(
            SignatureType::Author,
            "Libra".to_string(),
            "ai@libra".to_string(),
        );
        let committer = Signature::new(
            SignatureType::Committer,
            "Libra".to_string(),
            "ai@libra".to_string(),
        );

        let commit = Commit::new(
            author,
            committer,
            empty_tree_hash,
            vec![],
            "Initialize AI history branch",
        );

        let commit_data = commit
            .to_data()
            .context("Failed to serialize AI history init commit")?;
        let commit_hash = write_git_object(&self.repo_path, "commit", &commit_data)?;
        self.update_ref(&self.ref_name, commit_hash).await?;

        Ok(())
    }

    /// Return the ref name this manager writes to.
    ///
    /// Functional scope:
    /// - Useful for diagnostics, log messages, and TUI labels that need to
    ///   present the active AI history branch to the user.
    pub fn ref_name(&self) -> &str {
        &self.ref_name
    }

    /// Append an object to the history log.
    /// This operation is synchronous (commits immediately) for the MVP.
    ///
    /// Functional scope:
    /// - Implements the read-merge-CAS loop:
    ///   1. Read the current head.
    ///   2. Write a new commit that adds `<object_type>/<object_id>`
    ///      (replacing any prior entry under that path).
    ///   3. CAS the ref forward.
    /// - Reuses [`Self::create_append_commit`] for splice logic and
    ///   [`Self::update_ref_if_matches`] for the optimistic ref update.
    ///
    /// Boundary conditions:
    /// - Retries up to [`HISTORY_HEAD_CONFLICT_MAX_RETRIES`] times when a
    ///   concurrent writer advances the ref between read and CAS. After the
    ///   bound is exhausted the call fails with a contextual error so the
    ///   caller can decide whether to back off and retry.
    /// - The intermediate commit objects from failed CAS attempts remain in
    ///   the object database as garbage; they are unreachable and will be
    ///   collected by the next `libra gc` cycle.
    ///
    /// See: `tests::test_history_append_simple` and
    /// `tests::test_update_ref_if_matches_rejects_stale_history_head`.
    pub async fn append(
        &self,
        object_type: &str,
        object_id: &str,
        blob_hash: ObjectHash,
    ) -> Result<()> {
        for attempt in 0..=HISTORY_HEAD_CONFLICT_MAX_RETRIES {
            // Phase 1: snapshot the head we are racing against.
            let parent_commit_id = self.resolve_history_head().await?;
            // Phase 2: build the new commit on top of the snapshot.
            let commit_hash =
                self.create_append_commit(parent_commit_id, object_type, object_id, blob_hash)?;

            // Phase 3: atomically advance the ref iff its current value still
            // equals the snapshot. On `HeadChanged`, restart from phase 1.
            match self
                .update_ref_if_matches(&self.ref_name, parent_commit_id, commit_hash)
                .await?
            {
                RefUpdateOutcome::Updated => return Ok(()),
                RefUpdateOutcome::HeadChanged if attempt < HISTORY_HEAD_CONFLICT_MAX_RETRIES => {
                    continue;
                }
                RefUpdateOutcome::HeadChanged => {
                    return Err(anyhow!(
                        "history head changed repeatedly while appending {}/{}",
                        object_type,
                        object_id
                    ));
                }
            }
        }

        unreachable!("head conflict retry loop must return on success or terminal error")
    }

    /// Retrieve the object hash for a given type and ID from the current history.
    ///
    /// Functional scope:
    /// - Resolves the head commit, walks `<root_tree>/<object_type>/<object_id>`,
    ///   and returns the leaf blob hash if it exists.
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when the ref is not initialised, when no
    ///   subtree exists for `object_type`, or when the `object_id` entry is
    ///   missing under that subtree.
    /// - Surfaces `Err` only for object-store / parse failures.
    pub async fn get_object_hash(
        &self,
        object_type: &str,
        object_id: &str,
    ) -> Result<Option<ObjectHash>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let root_items = self.load_commit_tree(&parent_id)?;
            if let Some(type_entry) = root_items.iter().find(|item| item.name == object_type) {
                let type_items = self.load_tree(&type_entry.id)?;
                if let Some(item) = type_items.iter().find(|item| item.name == object_id) {
                    return Ok(Some(item.id));
                }
            }
        }
        Ok(None)
    }

    /// Find an object by ID across all types in the history.
    /// Returns (hash, type).
    ///
    /// Functional scope:
    /// - Convenience wrapper around [`Self::find_object_hashes`] that
    ///   returns only the first match.
    ///
    /// Boundary conditions:
    /// - When the same object id exists under multiple type subtrees the
    ///   caller has no control over which is chosen; use
    ///   [`Self::find_object_hashes`] when a deterministic tie-break is
    ///   required.
    pub async fn find_object_hash(&self, object_id: &str) -> Result<Option<(ObjectHash, String)>> {
        Ok(self.find_object_hashes(object_id).await?.into_iter().next())
    }

    /// Find all objects that share the same object ID across history types.
    ///
    /// Functional scope:
    /// - Walks every type subtree under the head root tree and collects
    ///   `(blob_hash, type_name)` tuples for every subtree containing
    ///   `object_id`.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the ref is not initialised or the id
    ///   does not appear under any type.
    ///
    /// See: `tests::test_find_object_hashes_returns_all_matching_types`.
    pub async fn find_object_hashes(&self, object_id: &str) -> Result<Vec<(ObjectHash, String)>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let root_items = self.load_commit_tree(&parent_id)?;
            let mut matches = Vec::new();
            for type_entry in root_items {
                let type_items = self.load_tree(&type_entry.id)?;
                if let Some(item) = type_items.iter().find(|item| item.name == object_id) {
                    matches.push((item.id, type_entry.name.clone()));
                }
            }
            return Ok(matches);
        }
        Ok(Vec::new())
    }

    /// List all objects of a specific type from the current history.
    /// Returns a list of (object_id, object_hash).
    ///
    /// Functional scope:
    /// - Loads the head commit's `<object_type>` subtree and yields its
    ///   contents as `(name, blob_hash)` pairs in tree-order.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the ref is not initialised or no
    ///   subtree exists for `object_type`.
    pub async fn list_objects(&self, object_type: &str) -> Result<Vec<(String, ObjectHash)>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let root_items = self.load_commit_tree(&parent_id)?;
            if let Some(type_entry) = root_items.iter().find(|item| item.name == object_type) {
                let type_items = self.load_tree(&type_entry.id)?;
                return Ok(type_items
                    .into_iter()
                    .map(|item| (item.name, item.id))
                    .collect());
            }
        }
        Ok(Vec::new())
    }

    /// List all object types present at the current history head.
    ///
    /// Functional scope:
    /// - Returns the names of every top-level subtree under the head root,
    ///   sorted lexicographically for stable output.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the ref is not initialised. The empty
    ///   tree case (initialised ref with no objects) likewise yields an
    ///   empty vector.
    ///
    /// See: `tests::test_list_object_types_returns_sorted_types`.
    pub async fn list_object_types(&self) -> Result<Vec<String>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let mut root_items = self.load_commit_tree(&parent_id)?;
            root_items.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(root_items.into_iter().map(|item| item.name).collect());
        }
        Ok(Vec::new())
    }

    /// Resolve the current head commit of the AI history ref.
    ///
    /// Functional scope:
    /// - Queries the `reference` table for the row that matches
    ///   `(name=ref_name, kind=Branch)` and parses its `commit` column into
    ///   an [`ObjectHash`].
    /// - Tolerates transient SQLite-busy errors with a bounded linear
    ///   backoff governed by [`SQLITE_BUSY_MAX_RETRIES`] /
    ///   [`SQLITE_BUSY_RETRY_BASE_MS`].
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when the ref row is missing or its `commit`
    ///   column is `NULL` (the ref exists but points nowhere yet).
    /// - Returns `Err` if the stored commit string is not a valid object
    ///   hash — this indicates database corruption and the caller should
    ///   surface it rather than silently treating it as missing.
    pub async fn resolve_history_head(&self) -> Result<Option<ObjectHash>> {
        let mut attempt = 0;
        let ref_model = loop {
            match reference::Entity::find()
                .filter(reference::Column::Name.eq(&self.ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&*self.db_conn)
                .await
            {
                Ok(found) => break found,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    attempt += 1;
                    // Linear backoff (BASE * attempt) — see SQLITE_BUSY_* constants.
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * attempt as u64,
                    ))
                    .await;
                }
                Err(err) => return Err(err).context("Failed to query history head"),
            }
        };

        match ref_model {
            Some(model) => match model.commit {
                Some(commit_hash) => ObjectHash::from_str(&commit_hash)
                    .map(Some)
                    .map_err(|e| anyhow!("Invalid commit hash in DB: {}", e)),
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    /// Load the root tree of a commit by parsing its `tree <hash>` header
    /// line.
    ///
    /// Functional scope:
    /// - Reads the commit blob, scans its text lines for the leading
    ///   `tree ` header, parses the referenced tree, and returns its items.
    ///
    /// Boundary conditions:
    /// - Returns an error when the commit blob is missing the `tree`
    ///   header. That should never happen for objects we wrote ourselves
    ///   but we guard against repository corruption.
    fn load_commit_tree(&self, commit_id: &ObjectHash) -> Result<Vec<TreeItem>> {
        let data = read_git_object(&self.repo_path, commit_id)?;
        // Commit format: tree <hash>\nparent...
        let content = String::from_utf8_lossy(&data);
        for line in content.lines() {
            if let Some(hash_str) = line.strip_prefix("tree ") {
                let tree_hash = ObjectHash::from_str(hash_str)
                    .map_err(|e| anyhow!("Invalid tree hash in commit: {}", e))?;
                return self.load_tree(&tree_hash);
            }
        }
        Err(anyhow!("Commit has no tree"))
    }

    /// Load and parse a tree object's items.
    ///
    /// Functional scope:
    /// - Thin wrapper around `Tree::from_bytes` for the AI-history call
    ///   sites; centralised so all tree reads go through the same error
    ///   path.
    fn load_tree(&self, tree_id: &ObjectHash) -> Result<Vec<TreeItem>> {
        let data = read_git_object(&self.repo_path, tree_id)?;

        let tree = Tree::from_bytes(&data, *tree_id)?;
        Ok(tree.tree_items)
    }

    /// Serialise tree items into Git's binary tree format and persist as
    /// an object.
    ///
    /// Functional scope:
    /// - Encodes each item as `<mode> <name>\0<binary_hash>` per the Git
    ///   tree spec, concatenates them in caller-provided order, and writes
    ///   the bytes to the object database under type `tree`.
    ///
    /// Boundary conditions:
    /// - Items must already be sorted by the caller (`append`/the splice
    ///   helpers do this). Unsorted items would still parse but would
    ///   produce a different tree hash than canonical Git.
    /// - Rejects hashes whose binary length is not 20 (SHA-1) or 32
    ///   (SHA-256) — protection against malformed inputs that would
    ///   otherwise corrupt the object store.
    fn write_tree(&self, tree_items: &[TreeItem]) -> Result<ObjectHash> {
        Ok(self.write_tree_with_size(tree_items)?.0)
    }

    /// Encode `tree_items` as a Git tree, write the object, and return
    /// `(hash, encoded_size)`. The size is the *content* length (no Git
    /// header) — same convention as `object_index.o_size`.
    ///
    /// Used by the agent capture path (Phase 3.5c) which needs the byte
    /// count to pair with [`crate::utils::client_storage::enqueue_agent_blob_object_index_update`].
    /// All other callers go through [`Self::write_tree`] and discard the
    /// size.
    fn write_tree_with_size(&self, tree_items: &[TreeItem]) -> Result<(ObjectHash, usize)> {
        let mut ignored = HashSet::new();
        self.write_tree_with_size_tracked(tree_items, &mut ignored)
    }

    fn write_tree_with_size_tracked(
        &self,
        tree_items: &[TreeItem],
        newly_written: &mut HashSet<String>,
    ) -> Result<(ObjectHash, usize)> {
        let data = Self::encode_tree_data(tree_items)?;
        let size = data.len();
        let (hash, was_created) = write_git_object_with_status(&self.repo_path, "tree", &data)?;
        if was_created {
            newly_written.insert(hash.to_string());
        }
        Ok((hash, size))
    }

    fn encode_tree_data(tree_items: &[TreeItem]) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        for item in tree_items {
            let mode_str = match item.mode {
                TreeItemMode::Tree => "40000",
                TreeItemMode::Blob => "100644",
                TreeItemMode::BlobExecutable => "100755",
                TreeItemMode::Link => "120000",
                TreeItemMode::Commit => "160000",
            };
            data.extend_from_slice(mode_str.as_bytes());
            data.push(b' ');
            data.extend_from_slice(item.name.as_bytes());
            data.push(0);
            let hash_hex = item.id.to_string();
            let hash_bytes =
                hex::decode(&hash_hex).map_err(|e| anyhow!("Invalid hash hex: {}", e))?;
            // 20 bytes for SHA-1, 32 for SHA-256. Anything else is a
            // signal that we are about to corrupt the object database.
            if hash_bytes.len() != 20 && hash_bytes.len() != 32 {
                return Err(anyhow!("Invalid object hash length: {}", hash_bytes.len()));
            }
            data.extend_from_slice(&hash_bytes);
        }
        Ok(data)
    }

    /// Write a tree object and stamp it into `object_index` with the
    /// given `o_type`. Used by the agent capture path so cloud sync
    /// uploads the trees that compose `refs/libra/traces`.
    fn write_tree_indexed_tracked(
        &self,
        tree_items: &[TreeItem],
        o_type: &str,
        newly_written: &mut HashSet<String>,
    ) -> Result<ObjectHash> {
        let (hash, size) = self.write_tree_with_size_tracked(tree_items, newly_written)?;
        crate::utils::client_storage::enqueue_agent_blob_object_index_update(
            &self.repo_path,
            &hash.to_string(),
            o_type,
            size as i64,
        )
        .with_context(|| format!("register durable object-index repair for tree {hash}"))?;
        Ok(hash)
    }

    async fn load_traces_writer_fence(
        &self,
        session_id: &str,
        attempt_id: &str,
    ) -> Result<TracesWriterFence> {
        let entry = crate::internal::metadata::MetadataKv::get_with_conn(
            self.db_conn.as_ref(),
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            session_id,
            attempt_id,
        )
        .await
        .context("load checkpoint writer marker generation")?
        .ok_or_else(|| anyhow!("checkpoint writer marker is missing before append"))?;
        let marker =
            decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key)?;
        if marker.cleanup_pending {
            bail!("checkpoint writer marker entered cleanup before append; retry the operation");
        }
        let generation = marker.generation.ok_or_else(|| {
            anyhow!(
                "checkpoint writer marker predates generation fencing; wait for it to expire and retry"
            )
        })?;
        Ok(TracesWriterFence {
            session_id: session_id.to_string(),
            attempt_id: attempt_id.to_string(),
            generation,
        })
    }

    fn ensure_marker_matches_fence(
        marker: &TracesInflightMarker,
        fence: &TracesWriterFence,
    ) -> Result<()> {
        if marker.session_id != fence.session_id
            || marker.attempt_id != fence.attempt_id
            || marker.generation.as_deref() != Some(fence.generation.as_str())
            || marker.cleanup_pending
        {
            bail!(
                "checkpoint writer marker generation was fenced or replaced; retry the operation"
            );
        }
        Ok(())
    }

    async fn persist_attempt_oid_before_write(
        &self,
        fence: &TracesWriterFence,
        oid: &ObjectHash,
        deadline: Option<Instant>,
    ) -> Result<()> {
        // Object-index updates from the preceding object use a background
        // SQLite writer. Optimistically take the marker transaction; if the
        // queue won the lock race, drain it before the bounded retry. Waiting
        // unconditionally here would serialize every object-index update and
        // make multi-turn historical imports miss their total deadline.
        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                bail!("checkpoint append exceeded the historical import execution deadline");
            }
            let result: Result<()> = async {
                let txn = self
                    .db_conn
                    .begin()
                    .await
                    .context("begin checkpoint object ownership update")?;
                let entry = crate::internal::metadata::MetadataKv::get_with_conn(
                    &txn,
                    crate::internal::metadata::MetadataScope::AgentTracesInflight,
                    &fence.session_id,
                    &fence.attempt_id,
                )
                .await
                .context("load checkpoint writer marker before object write")?
                .ok_or_else(|| {
                    anyhow!(
                        "checkpoint writer marker disappeared before object write; refusing to create loose objects"
                    )
                })?;
                let mut marker = decode_and_validate_traces_inflight_marker(
                    &entry.value,
                    &entry.target,
                    &entry.key,
                )?;
                Self::ensure_marker_matches_fence(&marker, fence)?;
                marker.schema_version = marker.schema_version.max(3);
                let oid = oid.to_string();
                if !marker.oids.contains(&oid) {
                    marker.oids.push(oid);
                    marker.oids.sort();
                    if !update_traces_inflight_marker_if_generation(
                        &txn,
                        &marker,
                        &fence.generation,
                    )
                    .await
                    .context("persist checkpoint object ownership before write")?
                    {
                        txn.rollback().await.ok();
                        bail!("checkpoint writer marker generation changed before object write");
                    }
                }
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    txn.rollback().await.ok();
                    bail!(
                        "checkpoint append exceeded the historical import execution deadline"
                    );
                }
                txn.commit()
                    .await
                    .context("commit checkpoint object ownership before write")?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(error)
                    if anyhow_is_sqlite_busy(&error) && attempt < SQLITE_BUSY_MAX_RETRIES =>
                {
                    let now = Instant::now();
                    let drain_deadline = deadline
                        .unwrap_or(now + OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET)
                        .min(now + OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET);
                    let _ = crate::utils::client_storage::ClientStorage::wait_for_background_tasks_until(
                        drain_deadline,
                    )
                    .await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(anyhow!(
            "checkpoint object ownership update exhausted its bounded SQLite retry loop"
        ))
    }

    async fn finalize_attempt_oid_after_write(
        &self,
        fence: &TracesWriterFence,
        oid: &ObjectHash,
        was_created: bool,
        deadline: Option<Instant>,
    ) -> Result<()> {
        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                bail!("checkpoint append exceeded the historical import execution deadline");
            }
            let result: Result<()> = async {
                let txn = self
                    .db_conn
                    .begin()
                    .await
                    .context("begin checkpoint object ownership finalization")?;
                let entry = crate::internal::metadata::MetadataKv::get_with_conn(
                    &txn,
                    crate::internal::metadata::MetadataScope::AgentTracesInflight,
                    &fence.session_id,
                    &fence.attempt_id,
                )
                .await
                .context("load checkpoint writer marker after object write")?
                .ok_or_else(|| {
                    anyhow!(
                        "checkpoint writer marker disappeared after object write; refusing to continue"
                    )
                })?;
                let mut marker = decode_and_validate_traces_inflight_marker(
                    &entry.value,
                    &entry.target,
                    &entry.key,
                )?;
                Self::ensure_marker_matches_fence(&marker, fence)?;
                marker.schema_version = marker.schema_version.max(3);
                let oid = oid.to_string();
                marker.oids.retain(|candidate| candidate != &oid);
                if was_created && !marker.created_oids.contains(&oid) {
                    marker.created_oids.push(oid);
                    marker.created_oids.sort();
                }
                if !update_traces_inflight_marker_if_generation(
                    &txn,
                    &marker,
                    &fence.generation,
                )
                .await
                .context("finalize checkpoint object ownership after write")?
                {
                    txn.rollback().await.ok();
                    bail!("checkpoint writer marker generation changed after object write");
                }
                txn.commit()
                    .await
                    .context("commit checkpoint object ownership finalization")?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(error)
                    if anyhow_is_sqlite_busy(&error) && attempt < SQLITE_BUSY_MAX_RETRIES =>
                {
                    let now = Instant::now();
                    let drain_deadline = deadline
                        .unwrap_or(now + OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET)
                        .min(now + OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET);
                    let _ = crate::utils::client_storage::ClientStorage::wait_for_background_tasks_until(
                        drain_deadline,
                    )
                    .await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(anyhow!(
            "checkpoint object ownership finalization exhausted its bounded SQLite retry loop"
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_indexed_object_for_attempt(
        &self,
        object_type: &str,
        data: &[u8],
        index_type: &str,
        what: &str,
        fence: &TracesWriterFence,
        deadline: Option<Instant>,
        newly_written: &mut HashSet<String>,
    ) -> Result<ObjectHash> {
        let expected_oid = git_object_hash(object_type, data);
        let oid_string = expected_oid.to_string();
        let needs_preclaim = if deadline.is_some() {
            // A foreground existence probe can itself block on FUSE/NFS.
            // Preclaiming an already-existing object is harmless: successful
            // helper completion removes that transient ownership row.
            true
        } else {
            !self
                .repo_path
                .join("objects")
                .join(&oid_string[..2])
                .join(&oid_string[2..])
                .exists()
        };
        if needs_preclaim {
            self.persist_attempt_oid_before_write(fence, &expected_oid, deadline)
                .await?;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("checkpoint append exceeded the historical import execution deadline");
        }
        let (oid, was_created) = if let Some(deadline) = deadline {
            use base64::{Engine as _, engine::general_purpose::STANDARD};

            let response = invoke_checkpoint_object_helper(
                &self.repo_path,
                CheckpointObjectIoOperation::Write {
                    object_type: object_type.to_string(),
                    data_base64: STANDARD.encode(data),
                },
                deadline,
            )
            .await
            .with_context(|| format!("failed to write checkpoint {what} {object_type}"))?;
            match response {
                CheckpointObjectIoHelperResponse::Written { oid, was_created } => (
                    ObjectHash::from_str(&oid).map_err(|error| {
                        anyhow!("helper returned invalid checkpoint oid '{oid}': {error}")
                    })?,
                    was_created,
                ),
                CheckpointObjectIoHelperResponse::Error { message } => {
                    bail!("failed to write checkpoint {what} {object_type}: {message}")
                }
                CheckpointObjectIoHelperResponse::Read { .. } => {
                    bail!("checkpoint object-I/O helper returned a read response for a write")
                }
                CheckpointObjectIoHelperResponse::Verified { .. } => {
                    bail!("checkpoint object-I/O helper returned a verify response for a write")
                }
            }
        } else {
            write_git_object_with_status(&self.repo_path, object_type, data)
                .with_context(|| format!("failed to write checkpoint {what} {object_type}"))?
        };
        if oid != expected_oid {
            bail!("checkpoint object hash changed between ownership registration and write");
        }
        if was_created {
            newly_written.insert(oid.to_string());
        }
        self.finalize_attempt_oid_after_write(fence, &oid, was_created, deadline)
            .await?;
        if was_created
            && cfg!(debug_assertions)
            && std::env::var_os("LIBRA_TEST_CHECKPOINT_CRASH_AFTER_FIRST_OBJECT").is_some()
        {
            std::process::exit(86);
        }
        crate::utils::client_storage::enqueue_agent_blob_object_index_update(
            &self.repo_path,
            &oid.to_string(),
            index_type,
            data.len() as i64,
        )
        .with_context(|| format!("register durable object-index repair for {what} {oid}"))?;
        Ok(oid)
    }

    async fn read_checkpoint_object_for_attempt(
        &self,
        oid: &ObjectHash,
        expected_type: &str,
        deadline: Option<Instant>,
    ) -> Result<Vec<u8>> {
        let Some(deadline) = deadline else {
            return read_git_object(&self.repo_path, oid).map_err(Into::into);
        };
        let response = invoke_checkpoint_object_helper(
            &self.repo_path,
            CheckpointObjectIoOperation::Read {
                oid: oid.to_string(),
                expected_type: expected_type.to_string(),
            },
            deadline,
        )
        .await?;
        match response {
            CheckpointObjectIoHelperResponse::Read {
                oid: returned_oid,
                object_type,
                data_base64,
            } => {
                use base64::{Engine as _, engine::general_purpose::STANDARD};

                if returned_oid != oid.to_string() {
                    bail!("checkpoint object-I/O helper returned the wrong object id");
                }
                if object_type != expected_type {
                    bail!(
                        "checkpoint object-I/O helper returned type '{object_type}', expected '{expected_type}'"
                    );
                }
                STANDARD
                    .decode(data_base64)
                    .context("decode checkpoint object-I/O read payload")
            }
            CheckpointObjectIoHelperResponse::Error { message } => {
                bail!("failed to read checkpoint object {oid}: {message}")
            }
            CheckpointObjectIoHelperResponse::Written { .. } => {
                bail!("checkpoint object-I/O helper returned a write response for a read")
            }
            CheckpointObjectIoHelperResponse::Verified { .. } => {
                bail!("checkpoint object-I/O helper returned a verify response for a read")
            }
        }
    }

    async fn load_commit_tree_for_attempt(
        &self,
        commit_id: &ObjectHash,
        deadline: Option<Instant>,
    ) -> Result<Vec<TreeItem>> {
        let data = self
            .read_checkpoint_object_for_attempt(commit_id, "commit", deadline)
            .await?;
        let content = String::from_utf8_lossy(&data);
        for line in content.lines() {
            if let Some(hash_str) = line.strip_prefix("tree ") {
                let tree_hash = ObjectHash::from_str(hash_str)
                    .map_err(|error| anyhow!("Invalid tree hash in commit: {error}"))?;
                return self.load_tree_for_attempt(&tree_hash, deadline).await;
            }
        }
        bail!("Commit has no tree")
    }

    async fn load_tree_for_attempt(
        &self,
        tree_id: &ObjectHash,
        deadline: Option<Instant>,
    ) -> Result<Vec<TreeItem>> {
        let data = self
            .read_checkpoint_object_for_attempt(tree_id, "tree", deadline)
            .await?;
        Ok(Tree::from_bytes(&data, *tree_id)?.tree_items)
    }

    async fn write_tree_indexed_for_attempt(
        &self,
        tree_items: &[TreeItem],
        fence: &TracesWriterFence,
        deadline: Option<Instant>,
        newly_written: &mut HashSet<String>,
    ) -> Result<ObjectHash> {
        let data = Self::encode_tree_data(tree_items)?;
        self.write_indexed_object_for_attempt(
            "tree",
            &data,
            "tree",
            "tree",
            fence,
            deadline,
            newly_written,
        )
        .await
    }

    fn create_append_commit(
        &self,
        parent_commit_id: Option<ObjectHash>,
        object_type: &str,
        object_id: &str,
        blob_hash: ObjectHash,
    ) -> Result<ObjectHash> {
        let mut root_items = if let Some(parent_id) = parent_commit_id {
            self.load_commit_tree(&parent_id)?
        } else {
            Vec::new()
        };

        let type_tree_entry = root_items
            .iter()
            .find(|item| item.name == object_type)
            .cloned();

        let mut type_items = if let Some(entry) = type_tree_entry {
            self.load_tree(&entry.id)?
        } else {
            Vec::new()
        };

        let new_item = TreeItem::new(TreeItemMode::Blob, blob_hash, object_id.to_string());
        type_items.retain(|item| item.name != object_id);
        type_items.push(new_item);
        type_items.sort_by(|a, b| a.name.cmp(&b.name));

        let type_tree_hash = self.write_tree(&type_items)?;

        let new_root_item =
            TreeItem::new(TreeItemMode::Tree, type_tree_hash, object_type.to_string());
        root_items.retain(|item| item.name != object_type);
        root_items.push(new_root_item);
        root_items.sort_by(|a, b| a.name.cmp(&b.name));

        let root_tree_hash = self.write_tree(&root_items)?;

        let author = Signature::new(
            SignatureType::Author,
            "Libra".to_string(),
            "history@libra".to_string(),
        );

        let signature = Signature::new(
            SignatureType::Committer,
            "Libra".to_string(),
            "history@libra".to_string(),
        );

        let message = format!("Update {}/{}", object_type, object_id);
        let parents = parent_commit_id.into_iter().collect::<Vec<_>>();
        let commit = Commit::new(author, signature, root_tree_hash, parents, &message);
        let commit_data = commit
            .to_data()
            .context("Failed to serialize AI history commit")?;
        write_git_object(&self.repo_path, "commit", &commit_data)
            .context("Failed to write AI history commit")
    }

    async fn update_ref(&self, ref_name: &str, hash: ObjectHash) -> Result<()> {
        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            let txn: DatabaseTransaction = match self.db_conn.begin().await {
                Ok(txn) => txn,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to begin transaction"),
            };

            let existing = match reference::Entity::find()
                .filter(reference::Column::Name.eq(ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&txn)
                .await
            {
                Ok(existing) => existing,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to query reference"),
            };

            let had_existing = existing.is_some();
            let write_result = if let Some(model) = existing {
                let mut active: reference::ActiveModel = model.into();
                active.commit = Set(Some(hash.to_string()));
                active.update(&txn).await.map(|_| ())
            } else {
                let new_ref = reference::ActiveModel {
                    name: Set(Some(ref_name.to_string())),
                    kind: Set(ConfigKind::Branch),
                    commit: Set(Some(hash.to_string())),
                    remote: Set(None),
                    ..Default::default()
                };
                new_ref.insert(&txn).await.map(|_| ())
            };

            match write_result {
                Ok(()) => {}
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => {
                    let context = if had_existing {
                        "Failed to update reference"
                    } else {
                        "Failed to insert reference"
                    };
                    return Err(err).context(context);
                }
            }

            match txn.commit().await {
                Ok(()) => return Ok(()),
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => return Err(err).context("Failed to commit transaction"),
            }
        }

        unreachable!("sqlite busy retry loop must return on success or terminal error")
    }

    async fn update_ref_if_matches(
        &self,
        ref_name: &str,
        expected_head: Option<ObjectHash>,
        new_hash: ObjectHash,
    ) -> Result<RefUpdateOutcome> {
        self.update_ref_if_matches_with_extra(ref_name, expected_head, new_hash, None, None, None)
            .await
    }

    /// Conditional ref update with optional transactional companion writes
    /// (plan-20260713 ADR-DR-10). When `extra` is provided, its SQL runs in
    /// the SAME transaction as the ref write — after the CAS row update
    /// succeeds, before COMMIT — so catalog/claim/revision state can never
    /// diverge from the ref. An `extra` error rolls the whole transaction
    /// back (the ref does not move) and propagates as a hard error, not a
    /// `HeadChanged` retry.
    async fn update_ref_if_matches_with_extra(
        &self,
        ref_name: &str,
        expected_head: Option<ObjectHash>,
        new_hash: ObjectHash,
        extra: Option<(&dyn TracesTxnExtra, &TracesCommitCtx)>,
        deadline: Option<Instant>,
        marker_fence: Option<&TracesWriterFence>,
    ) -> Result<RefUpdateOutcome> {
        let expected_commit = expected_head.map(|hash| hash.to_string());
        let new_commit = new_hash.to_string();

        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                bail!("checkpoint append exceeded the historical import execution deadline");
            }
            let txn: DatabaseTransaction = match self.db_conn.begin().await {
                Ok(txn) => txn,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to begin transaction"),
            };

            // An expired ordinary marker may have been fenced and retired by
            // crash recovery while this writer was stalled. The marker check
            // rides the same SQLite writer transaction as the ref/catalog CAS:
            // cleanup wins first => this writer cannot publish; this writer
            // wins first => cleanup observes the committed root/catalog.
            if let Some(marker_fence) = marker_fence {
                let entry = crate::internal::metadata::MetadataKv::get_with_conn(
                    &txn,
                    crate::internal::metadata::MetadataScope::AgentTracesInflight,
                    &marker_fence.session_id,
                    &marker_fence.attempt_id,
                )
                .await
                .context("revalidate checkpoint writer marker before ref update")?;
                let Some(entry) = entry else {
                    txn.rollback().await.ok();
                    bail!(
                        "checkpoint writer marker was fenced before ref update; retry the operation"
                    );
                };
                let marker = decode_and_validate_traces_inflight_marker(
                    &entry.value,
                    &entry.target,
                    &entry.key,
                )?;
                if let Err(error) = Self::ensure_marker_matches_fence(&marker, marker_fence) {
                    txn.rollback().await.ok();
                    return Err(error.context("revalidate marker generation before ref update"));
                }
            }

            let existing = match reference::Entity::find()
                .filter(reference::Column::Name.eq(ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&txn)
                .await
            {
                Ok(existing) => existing,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to query reference"),
            };

            let write_result = match existing {
                Some(model) if model.commit != expected_commit => {
                    let _ = txn.rollback().await;
                    return Ok(RefUpdateOutcome::HeadChanged);
                }
                Some(model) => {
                    let mut update = reference::Entity::update_many()
                        .filter(reference::Column::Id.eq(model.id))
                        .filter(reference::Column::Name.eq(ref_name))
                        .filter(reference::Column::Kind.eq(ConfigKind::Branch));
                    update = match expected_commit.as_ref() {
                        Some(commit) => update.filter(reference::Column::Commit.eq(commit.clone())),
                        None => update.filter(reference::Column::Commit.is_null()),
                    };

                    update
                        .col_expr(
                            reference::Column::Commit,
                            Expr::value(Some(new_commit.clone())),
                        )
                        .exec(&txn)
                        .await
                        .map(Some)
                }
                None if expected_commit.is_some() => {
                    let _ = txn.rollback().await;
                    return Ok(RefUpdateOutcome::HeadChanged);
                }
                None => {
                    let new_ref = reference::ActiveModel {
                        name: Set(Some(ref_name.to_string())),
                        kind: Set(ConfigKind::Branch),
                        commit: Set(Some(new_commit.clone())),
                        remote: Set(None),
                        ..Default::default()
                    };
                    match new_ref.insert(&txn).await {
                        Ok(_) => Ok(None),
                        Err(err) if is_sqlite_unique_violation(&err) => {
                            let _ = txn.rollback().await;
                            return Ok(RefUpdateOutcome::HeadChanged);
                        }
                        Err(err) => Err(err),
                    }
                }
            };

            let rows_affected = match write_result {
                Ok(rows_affected) => rows_affected,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to compare-and-swap history head"),
            };

            if rows_affected.is_some_and(|result| result.rows_affected != 1) {
                let _ = txn.rollback().await;
                return Ok(RefUpdateOutcome::HeadChanged);
            }

            // ADR-DR-10: companion writes ride the ref transaction. A
            // failure here must NOT move the ref — roll back and fail
            // closed (no HeadChanged retry: the failure is a gate/fence
            // violation or DB fault, not a CAS race).
            if let Some((extra, ctx)) = extra
                && let Err(err) = extra.apply(&txn, ctx).await
            {
                let _ = txn.rollback().await;
                return Err(
                    err.context("transactional companion writes failed; ref update rolled back")
                );
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let _ = txn.rollback().await;
                bail!("checkpoint append exceeded the historical import execution deadline");
            }

            match txn.commit().await {
                Ok(()) => return Ok(RefUpdateOutcome::Updated),
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => return Err(err).context("Failed to commit transaction"),
            }
        }

        unreachable!("sqlite busy retry loop must return on success or terminal error")
    }

    /// Append a checkpoint commit to this manager's ref.
    ///
    /// AG-20 (E4-libra layout). Builds the layered tree
    ///
    /// ```text
    /// checkpoint/<id[:2]>/<id[2:]>/
    ///   metadata.json
    ///   manifest.json
    ///   events/lifecycle.jsonl
    ///   transcript/<agent_kind>.jsonl        (or `.jsonl.001…` chunks, E5)
    ///   redaction_report.json
    ///   content_hash.txt
    /// ```
    ///
    /// and merges it into the parent commit's tree so successive checkpoints
    /// accumulate (rather than overwrite). The resulting commit message
    /// carries `Libra-*` trailers per the design spec (see
    /// `docs/development/commands/_general.md` §3.3). Pre-AG-20 checkpoints
    /// (metadata.json + `transcript/<provider>` only) remain readable as
    /// legacy-v1; this writer never emits that layout again.
    ///
    /// Returns the freshly-written commit hash plus the OIDs callers need to
    /// stamp onto `agent_checkpoint` (root tree OID and metadata blob OID),
    /// along with span bookkeeping (`cas_retries`, `object_count`).
    pub async fn append_checkpoint_commit(
        &self,
        params: CheckpointCommitParams<'_>,
    ) -> Result<CheckpointCommit> {
        // Durable rejected-object markers are diagnostic/GC work. Appends do
        // only the O(1) exact writer-fence check below; a repository-wide
        // reachability scan here can otherwise impose a permanent 30s+ stall
        // on every checkpoint while making no foreground deletion decision.
        let writer_fence = self
            .load_traces_writer_fence(params.session_id, params.checkpoint_id)
            .await?;
        if writer_fence.generation != params.marker_generation {
            bail!(
                "checkpoint writer marker generation was fenced or replaced before append; retry the operation"
            );
        }
        let mut newly_written = HashSet::new();
        let result = self
            .append_checkpoint_commit_inner(params, &writer_fence, &mut newly_written)
            .await;
        match result {
            Ok(commit) => Ok(commit),
            Err(error) => {
                if let Err(cleanup_error) = self
                    .cleanup_rejected_checkpoint_objects(&writer_fence, &newly_written)
                    .await
                {
                    return Err(anyhow!(
                        "{error:#}; failed to clean rejected checkpoint objects: {cleanup_error:#}"
                    ));
                }
                Err(error)
            }
        }
    }

    async fn append_checkpoint_commit_inner(
        &self,
        params: CheckpointCommitParams<'_>,
        writer_fence: &TracesWriterFence,
        newly_written: &mut HashSet<String>,
    ) -> Result<CheckpointCommit> {
        if cfg!(debug_assertions)
            && let Ok(value) = std::env::var("LIBRA_TEST_CHECKPOINT_APPEND_DELAY_MS")
            && let Ok(delay_ms) = value.parse::<u64>()
        {
            let delay = sleep(Duration::from_millis(delay_ms));
            if let Some(deadline) = params.deadline {
                tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), delay)
                    .await
                    .map_err(|_| {
                        anyhow!(
                            "checkpoint append exceeded the historical import execution deadline"
                        )
                    })?;
            } else {
                delay.await;
            }
        }
        // Phase 1: write content blobs once. They are content-addressed, so
        // re-running a CAS retry loop never duplicates them.
        //
        // CEX-EntireIO §14.3 phase-3 item 3: every agent blob is tagged in
        // `object_index` so `libra cloud sync` uploads it to R2. Only the
        // transcript blob(s) carry the distinguished o_type
        // ("agent_transcript"); the JSON sidecars use the standard "blob"
        // tag because cloud sync doesn't filter by o_type — the custom tag
        // exists for downstream tooling that enumerates captured
        // transcripts.
        let deadline = params.deadline;
        let ensure_deadline = || -> Result<()> {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                bail!("checkpoint append exceeded the historical import execution deadline");
            }
            Ok(())
        };
        ensure_deadline()?;
        let mut object_count: u64 = 0;
        let metadata_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                params.metadata_json.bytes(),
                "blob",
                "metadata.json",
                writer_fence,
                params.deadline,
                newly_written,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;
        let events_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                params.lifecycle_events_jsonl.bytes(),
                "blob",
                "events/lifecycle.jsonl",
                writer_fence,
                params.deadline,
                newly_written,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;
        let report_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                params.redaction_report_json.bytes(),
                "blob",
                "redaction_report.json",
                writer_fence,
                params.deadline,
                newly_written,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;

        // Transcript: E5 line-boundary-safe chunking above the threshold.
        // Small transcripts stay a single `transcript/<agent_kind>.jsonl`
        // file; larger ones split into `.jsonl.001`, `.jsonl.002`, … parts
        // declared (in order) by the manifest's `transcript` role.
        let transcript_bytes = params.transcript_redacted.bytes();
        let threshold = transcript_chunk_threshold();
        let transcript_file_name = format!("{}.jsonl", params.agent_kind);
        let chunks: Vec<&[u8]> = if transcript_bytes.len() > threshold {
            chunk_transcript_line_safe(transcript_bytes, threshold)?
        } else {
            vec![transcript_bytes]
        };
        let chunked = chunks.len() > 1;
        let mut transcript_parts: Vec<TranscriptPartRef> = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.iter().enumerate() {
            let name = if chunked {
                format!("{}.{:03}", transcript_file_name, index + 1)
            } else {
                transcript_file_name.clone()
            };
            let oid = self
                .write_indexed_object_for_attempt(
                    "blob",
                    chunk,
                    "agent_transcript",
                    "transcript",
                    writer_fence,
                    params.deadline,
                    newly_written,
                )
                .await?;
            object_count += 1;
            ensure_deadline()?;
            transcript_parts.push(TranscriptPartRef {
                name,
                oid,
                byte_len: chunk.len(),
            });
        }

        // content_hash.txt: `sha256:<64-lowercase-hex>` (no trailing
        // newline) over the concatenated bytes of the coverage roles in
        // [`CHECKPOINT_CONTENT_HASH_COVERAGE`] order. The transcript
        // contributes its logical (pre-chunking) byte stream, so the hash
        // is invariant under re-chunking. See the E4-libra section of
        // `docs/development/tracing/agent.md`.
        let content_hash = checkpoint_content_hash(&[
            params.metadata_json.bytes(),
            params.lifecycle_events_jsonl.bytes(),
            transcript_bytes,
            params.redaction_report_json.bytes(),
        ]);
        let content_hash_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                content_hash.as_bytes(),
                "blob",
                "content_hash.txt",
                writer_fence,
                params.deadline,
                newly_written,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;

        // manifest.json is written LAST among the blobs: it declares every
        // other entry's OID/length (including content_hash.txt), so nothing
        // can hash or list the manifest itself without circularity.
        let manifest_bytes = build_checkpoint_manifest_json(
            params.checkpoint_id,
            &transcript_file_name,
            ManifestBlobRef::new(metadata_blob_oid, params.metadata_json.len()),
            ManifestBlobRef::new(events_blob_oid, params.lifecycle_events_jsonl.len()),
            &transcript_parts,
            transcript_bytes.len(),
            ManifestBlobRef::new(report_blob_oid, params.redaction_report_json.len()),
            ManifestBlobRef::new(content_hash_blob_oid, content_hash.len()),
        )?;
        let manifest_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                &manifest_bytes,
                "blob",
                "manifest.json",
                writer_fence,
                params.deadline,
                newly_written,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;

        // Phase 2: build the leaf trees (transcript/, events/).
        // All trees written under the agent capture path go through
        // `write_tree_indexed` so they reach `object_index` and the
        // standard cloud sync path; otherwise the orphan ref's commits
        // would dereference to missing trees on a fresh `cloud restore`.
        let mut transcript_items: Vec<TreeItem> = transcript_parts
            .iter()
            .map(|part| TreeItem::new(TreeItemMode::Blob, part.oid, part.name.clone()))
            .collect();
        transcript_items.sort_by(|a, b| a.name.cmp(&b.name));
        let transcript_subtree = self
            .write_tree_indexed_for_attempt(
                &transcript_items,
                writer_fence,
                params.deadline,
                newly_written,
            )
            .await?;
        let events_subtree = self
            .write_tree_indexed_for_attempt(
                &[TreeItem::new(
                    TreeItemMode::Blob,
                    events_blob_oid,
                    CHECKPOINT_LIFECYCLE_EVENTS_FILE.to_string(),
                )],
                writer_fence,
                params.deadline,
                newly_written,
            )
            .await?;
        object_count += 2;

        let mut inner_items = vec![
            TreeItem::new(
                TreeItemMode::Blob,
                metadata_blob_oid,
                "metadata.json".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                manifest_blob_oid,
                "manifest.json".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                report_blob_oid,
                "redaction_report.json".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                content_hash_blob_oid,
                "content_hash.txt".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Tree,
                transcript_subtree,
                "transcript".to_string(),
            ),
            TreeItem::new(TreeItemMode::Tree, events_subtree, "events".to_string()),
        ];
        inner_items.sort_by(|a, b| a.name.cmp(&b.name));
        let inner_tree = self
            .write_tree_indexed_for_attempt(
                &inner_items,
                writer_fence,
                params.deadline,
                newly_written,
            )
            .await?;
        object_count += 1;

        // Phase 3: CAS loop. Read parent, splice
        // `checkpoint/<prefix>/<rest>` into its tree, write the new commit,
        // and update the ref atomically. Retries on head conflict, mirroring
        // the existing `append` flow.
        let prefix = params
            .checkpoint_id
            .get(..2)
            .ok_or_else(|| anyhow!("checkpoint_id must be at least 2 characters"))?
            .to_string();
        let rest = params.checkpoint_id[2..].to_string();
        for attempt in 0..=HISTORY_HEAD_CONFLICT_MAX_RETRIES {
            ensure_deadline()?;
            let parent = self.resolve_history_head().await?;
            ensure_deadline()?;
            // Test-only: deterministic head-moved-between-read-and-CAS
            // injection (see the struct field's doc).
            #[cfg(test)]
            if let Some(hook) = &self.test_after_head_read {
                hook().await?;
            }
            let new_root = self
                .splice_checkpoint_tree_for_attempt(
                    parent,
                    &prefix,
                    &rest,
                    inner_tree,
                    writer_fence,
                    params.deadline,
                    newly_written,
                )
                .await?;
            // splice_checkpoint_tree writes exactly three trees
            // (rest→prefix→checkpoint→root splice) per attempt; +1 commit.
            object_count += 4;

            let trailer = format_libra_trailers(&params);
            let message = format!(
                "traces: {} checkpoint {}\n\n{trailer}",
                params.scope.as_str(),
                params.checkpoint_id,
            );
            let author = Signature::new(
                SignatureType::Author,
                "Libra".to_string(),
                "traces@libra".to_string(),
            );
            let committer = Signature::new(
                SignatureType::Committer,
                "Libra".to_string(),
                "traces@libra".to_string(),
            );
            let parents = parent.into_iter().collect::<Vec<_>>();
            let commit = Commit::new(author, committer, new_root, parents, &message);
            let commit_data = commit
                .to_data()
                .context("failed to serialize checkpoint commit")?;
            let commit_hash = self
                .write_indexed_object_for_attempt(
                    "commit",
                    &commit_data,
                    "commit",
                    "commit",
                    writer_fence,
                    params.deadline,
                    newly_written,
                )
                .await?;
            ensure_deadline()?;

            // Per-attempt ctx: commit hash and root tree change on every CAS
            // rebuild, so the companion writes get the values of THIS attempt.
            let commit_ctx = TracesCommitCtx {
                commit_hash: commit_hash.to_string(),
                tree_oid: new_root.to_string(),
                metadata_blob_oid: metadata_blob_oid.to_string(),
            };
            match self
                .update_ref_if_matches_with_extra(
                    &self.ref_name,
                    parent,
                    commit_hash,
                    params.txn_extra.map(|extra| (extra, &commit_ctx)),
                    params.deadline,
                    Some(writer_fence),
                )
                .await?
            {
                RefUpdateOutcome::Updated => {
                    if cfg!(debug_assertions)
                        && let Ok(value) =
                            std::env::var("LIBRA_TEST_CHECKPOINT_POST_COMMIT_DELAY_MS")
                        && let Ok(delay_ms) = value.parse::<u64>()
                    {
                        sleep(Duration::from_millis(delay_ms)).await;
                    }
                    return Ok(CheckpointCommit {
                        commit_hash,
                        tree_oid: new_root,
                        metadata_blob_oid,
                        marker_generation: writer_fence.generation.clone(),
                        cas_retries: attempt as u64,
                        object_count,
                    });
                }
                RefUpdateOutcome::HeadChanged if attempt < HISTORY_HEAD_CONFLICT_MAX_RETRIES => {
                    continue;
                }
                RefUpdateOutcome::HeadChanged => {
                    return Err(anyhow!(
                        "history head changed repeatedly while appending checkpoint {}",
                        params.checkpoint_id
                    ));
                }
            }
        }
        Err(anyhow!(
            "checkpoint CAS retry loop exhausted without a terminal outcome"
        ))
    }

    async fn rejected_cleanup_db_snapshot<C: ConnectionTrait>(
        &self,
        conn: &C,
    ) -> Result<RejectedCleanupDbSnapshot> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut markers = list_all_traces_inflight_markers(conn).await?;
        markers.sort_by(|left, right| {
            (&left.session_id, &left.attempt_id).cmp(&(&right.session_id, &right.attempt_id))
        });

        let mut candidates = HashSet::new();
        for marker in markers.iter().filter(|marker| {
            marker.cleanup_pending
                || !marker.is_live(now_ms)
                || !marker.time_fields_trustworthy(now_ms)
        }) {
            let cataloged = conn
                .query_one(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT 1 FROM agent_checkpoint WHERE checkpoint_id = ?",
                    [marker.attempt_id.clone().into()],
                ))
                .await
                .context("verify rejected checkpoint cleanup candidate")?;
            if cataloged.is_none() {
                candidates.extend(marker.created_oids.iter().cloned());
            }
        }

        let mut root_rows = conn
            .query_all(Statement::from_string(
                conn.get_database_backend(),
                "SELECT `commit` AS oid FROM reference WHERE `commit` IS NOT NULL LIMIT 250001"
                    .to_string(),
            ))
            .await
            .context("list reference roots for rejected object cleanup")?;
        let reflog_exists = conn
            .query_one(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
                ["reflog".into()],
            ))
            .await
            .context("check reflog table before rejected object cleanup")?
            .is_some();
        if reflog_exists && root_rows.len() <= REJECTED_CLEANUP_MAX_VISITED_OBJECTS {
            root_rows.extend(
                conn.query_all(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT old_oid AS oid FROM reflog
                     UNION ALL SELECT new_oid AS oid FROM reflog
                     LIMIT 250001"
                        .to_string(),
                ))
                .await
                .context("list reflog roots for rejected object cleanup")?,
            );
        }
        if root_rows.len() > REJECTED_CLEANUP_MAX_VISITED_OBJECTS {
            bail!(
                "repository has more than {} reference/reflog cleanup roots",
                REJECTED_CLEANUP_MAX_VISITED_OBJECTS
            );
        }
        let mut graph_roots = Vec::with_capacity(root_rows.len());
        for row in root_rows {
            let value: String = row.try_get_by("oid")?;
            if !value.is_empty() && !value.bytes().all(|byte| byte == b'0') {
                ObjectHash::from_str(&value).map_err(|error| {
                    anyhow!("repository cleanup root {value} is invalid: {error}")
                })?;
                graph_roots.push(value);
            }
        }
        graph_roots.sort();
        graph_roots.dedup();

        let mut active_operations = Vec::new();
        for table in ["rebase_state", "sequence_state"] {
            let table_exists = conn
                .query_one(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
                    [table.into()],
                ))
                .await
                .with_context(|| format!("check {table} table before object cleanup"))?
                .is_some();
            if table_exists
                && conn
                    .query_one(Statement::from_string(
                        conn.get_database_backend(),
                        format!("SELECT 1 FROM {table} LIMIT 1"),
                    ))
                    .await
                    .with_context(|| format!("check active {table} before object cleanup"))?
                    .is_some()
            {
                active_operations.push(table.to_string());
            }
        }

        Ok(RejectedCleanupDbSnapshot {
            markers,
            candidates,
            graph_roots,
            active_operations,
        })
    }

    fn rejected_cleanup_index_snapshot(
        &self,
        deadline: Instant,
    ) -> Result<RejectedCleanupIndexSnapshot> {
        if Instant::now() >= deadline {
            bail!("repository cleanup deadline expired before index snapshot");
        }
        let request = serde_json::to_vec(&RejectedCleanupIndexHelperRequest {
            repo_path: self.repo_path.clone(),
            hash_bytes: get_hash_kind().size(),
        })
        .context("encode rejected-cleanup index helper request")?;
        let current_exe = std::env::current_exe()
            .context("resolve Libra executable for rejected-cleanup index helper")?;
        let program = if cfg!(debug_assertions)
            && !current_exe
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem == "libra")
        {
            current_exe
                .parent()
                .and_then(Path::parent)
                .map(|parent| parent.join("libra"))
                .filter(|candidate| candidate.is_file())
                .unwrap_or(current_exe)
        } else {
            current_exe
        };
        let mut output =
            tempfile::tempfile().context("create rejected-cleanup index helper output file")?;
        // Initialize the nonblocking reap service before creating a child, so
        // thread-allocation failure cannot strand an already-started helper.
        let reaper = cleanup_helper_reaper_sender()?;
        let child_output = output
            .try_clone()
            .context("clone rejected-cleanup index helper output file")?;
        let child = Command::new(&program)
            .arg(REJECTED_CLEANUP_INDEX_HELPER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(child_output))
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| {
                format!(
                    "start rejected-cleanup index helper '{}'",
                    program.display()
                )
            })?;
        let mut child = CleanupHelperChild::new(child, reaper);
        let mut stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or_else(|| anyhow!("rejected-cleanup index helper has no stdin pipe"))?;
        stdin
            .write_all(&request)
            .context("write rejected-cleanup index helper request")?;
        drop(stdin);

        let status = loop {
            if let Some(status) = child
                .child_mut()
                .try_wait()
                .context("poll rejected-cleanup index helper")?
            {
                break status;
            }
            if Instant::now() >= deadline {
                bail!("repository cleanup index snapshot exceeded its traversal deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !status.success() {
            bail!("rejected-cleanup index helper exited unsuccessfully");
        }
        output
            .seek(SeekFrom::Start(0))
            .context("rewind rejected-cleanup index helper output")?;
        let mut response = Vec::new();
        (&mut output)
            .take(REJECTED_CLEANUP_INDEX_HELPER_FRAME_CAP.saturating_add(1))
            .read_to_end(&mut response)
            .context("read rejected-cleanup index helper output")?;
        if response.len() as u64 > REJECTED_CLEANUP_INDEX_HELPER_FRAME_CAP {
            bail!("rejected-cleanup index helper response exceeds its frame limit");
        }
        let response: RejectedCleanupIndexHelperResponse = serde_json::from_slice(&response)
            .context("decode rejected-cleanup index helper response")?;
        match (response.snapshot, response.error) {
            (Some(snapshot), None) => Ok(snapshot),
            (None, Some(error)) => bail!("rejected-cleanup index snapshot failed: {error}"),
            _ => bail!("rejected-cleanup index helper returned an invalid response"),
        }
    }

    async fn rejected_cleanup_root_snapshot<C: ConnectionTrait>(
        &self,
        conn: &C,
        deadline: Instant,
    ) -> Result<RejectedCleanupRootSnapshot> {
        let db = self.rejected_cleanup_db_snapshot(conn).await?;
        let RejectedCleanupIndexSnapshot {
            roots,
            fingerprints,
            mut active_operations,
        } = self.rejected_cleanup_index_snapshot(deadline)?;
        active_operations.extend(db.active_operations.iter().cloned());
        active_operations.sort();
        active_operations.dedup();
        Ok(RejectedCleanupRootSnapshot {
            db,
            index_roots: roots,
            index_fingerprints: fingerprints,
            active_operations,
        })
    }

    #[cfg(test)]
    async fn reachable_rejected_objects_with_limit(
        &self,
        ref_heads: Vec<ObjectHash>,
        candidates: &HashSet<String>,
        max_inflated_object_bytes: u64,
    ) -> Result<HashSet<String>> {
        self.reachable_rejected_objects_with_limits(
            ref_heads,
            candidates,
            max_inflated_object_bytes,
            REJECTED_CLEANUP_MAX_VISITED_OBJECTS,
            Instant::now() + REJECTED_CLEANUP_MAX_TRAVERSAL_DURATION,
        )
        .await
    }

    #[cfg(test)]
    async fn reachable_rejected_objects_with_limits(
        &self,
        ref_heads: Vec<ObjectHash>,
        candidates: &HashSet<String>,
        max_inflated_object_bytes: u64,
        max_visited_objects: usize,
        deadline: Instant,
    ) -> Result<HashSet<String>> {
        let mut reachable = HashSet::new();
        let mut seen = HashSet::new();
        let mut stack = ref_heads;
        while let Some(oid) = stack.pop() {
            if Instant::now() >= deadline {
                bail!(
                    "ref-reachability cleanup exceeded its {} second traversal deadline after visiting {} objects",
                    REJECTED_CLEANUP_MAX_TRAVERSAL_DURATION.as_secs(),
                    seen.len()
                );
            }
            if !seen.contains(&oid) && seen.len() >= max_visited_objects {
                bail!(
                    "ref-reachability cleanup exceeded its {max_visited_objects} object traversal limit"
                );
            }
            if !seen.insert(oid) {
                continue;
            }
            let oid_string = oid.to_string();
            if candidates.contains(&oid_string) {
                reachable.insert(oid_string);
            }
            // Diagnostic reachability must understand every ref root, including
            // objects held in local packs or alternates. The storage-level
            // bounded read enforces a conservative load-cost cap before
            // materializing the payload; the explicit OID verification keeps
            // a corrupt loose/packed object from making deletion unsafe.
            let (data, object_type) = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.storage.get_with_limit(&oid, max_inflated_object_bytes),
            )
            .await
            .map_err(|_| {
                anyhow!(
                    "ref-reachability cleanup exceeded its traversal deadline while reading {oid}"
                )
            })?
            .with_context(|| format!("read ref-reachable {oid} during cleanup"))?;
            if Instant::now() >= deadline {
                bail!(
                    "ref-reachability cleanup exceeded its traversal deadline after reading {oid}"
                );
            }
            verify_fetched_object(&oid, object_type, &data)
                .with_context(|| format!("verify ref-reachable {oid} during cleanup"))?;
            match object_type {
                ObjectType::Commit => {
                    let commit = Commit::from_bytes(&data, oid)
                        .map_err(|error| anyhow!("parse ref-reachable commit {oid}: {error}"))?;
                    stack.push(commit.tree_id);
                    stack.extend(commit.parent_commit_ids);
                }
                ObjectType::Tree => {
                    let tree = Tree::from_bytes(&data, oid)
                        .map_err(|error| anyhow!("parse ref-reachable tree {oid}: {error}"))?;
                    for item in tree.tree_items {
                        let item_oid = item.id.to_string();
                        if candidates.contains(&item_oid) {
                            reachable.insert(item_oid);
                        }
                        if item.mode == TreeItemMode::Tree {
                            stack.push(item.id);
                        }
                    }
                }
                ObjectType::Tag => {
                    let body = std::str::from_utf8(&data).with_context(|| {
                        format!("parse ref-reachable annotated tag {oid} as UTF-8")
                    })?;
                    let target = body
                        .lines()
                        .next()
                        .and_then(|line| line.strip_prefix("object "))
                        .ok_or_else(|| {
                            anyhow!("ref-reachable annotated tag {oid} has no object target")
                        })?;
                    stack.push(ObjectHash::from_str(target).map_err(|error| {
                        anyhow!("parse annotated tag {oid} target {target}: {error}")
                    })?);
                }
                ObjectType::Blob => {}
                other => {
                    bail!(
                        "ref-reachable object {oid} has unsupported type '{other}' during rejected checkpoint cleanup"
                    )
                }
            }
        }
        Ok(reachable)
    }

    /// Convert objects created by a rejected append into a durable ownership
    /// record. The foreground failure path only registers the exact writer's
    /// cleanup job; doctor/GC owns object-index draining and repository-wide
    /// reachability so an append deadline cannot be extended by maintenance.
    async fn cleanup_rejected_checkpoint_objects(
        &self,
        writer_fence: &TracesWriterFence,
        newly_written: &HashSet<String>,
    ) -> Result<()> {
        // Persist ownership of every candidate before returning the rejected
        // append to its caller. This happens before any optional background
        // queue wait, so a timeout can never erase the only durable record of
        // newly-created object ownership.
        let txn = self
            .db_conn
            .begin()
            .await
            .context("begin rejected checkpoint cleanup registration")?;
        let existing = crate::internal::metadata::MetadataKv::get_with_conn(
            &txn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &writer_fence.session_id,
            &writer_fence.attempt_id,
        )
        .await
        .context("load rejected checkpoint writer marker")?;
        let mut marker = match existing {
            Some(entry) => {
                let marker = decode_and_validate_traces_inflight_marker(
                    &entry.value,
                    &entry.target,
                    &entry.key,
                )?;
                if Self::ensure_marker_matches_fence(&marker, writer_fence).is_err() {
                    txn.rollback().await.ok();
                    tracing::debug!(
                        session_id = %writer_fence.session_id,
                        checkpoint_id = %writer_fence.attempt_id,
                        "leaving rejected objects to repository GC after marker generation changed"
                    );
                    return Ok(());
                }
                marker
            }
            None => {
                txn.rollback().await.ok();
                return Ok(());
            }
        };
        marker.schema_version = marker.schema_version.max(3);
        marker.created_oids.extend(newly_written.iter().cloned());
        marker.created_oids.sort();
        marker.created_oids.dedup();
        if marker.created_oids.is_empty() {
            clear_traces_inflight_marker_if_generation(
                &txn,
                &writer_fence.session_id,
                &writer_fence.attempt_id,
                &writer_fence.generation,
            )
            .await?;
            txn.commit()
                .await
                .context("commit empty rejected checkpoint cleanup")?;
            return Ok(());
        }
        marker.cleanup_pending = true;
        if !update_traces_inflight_marker_if_generation(&txn, &marker, &writer_fence.generation)
            .await
            .context("persist rejected checkpoint cleanup job")?
        {
            txn.rollback().await.ok();
            return Ok(());
        }
        txn.commit()
            .await
            .context("commit rejected checkpoint cleanup registration")?;
        Ok(())
    }

    /// Drain all durable rejected-append cleanup jobs in one serialized
    /// root-fenced ownership retirement. A non-cleanup live writer defers retirement; pending
    /// jobs themselves may be expired and are still never forgotten.
    async fn drain_rejected_checkpoint_cleanup_jobs(&self) -> Result<()> {
        self.drain_rejected_checkpoint_cleanup_jobs_ignoring(None)
            .await
    }

    /// Doctor repair entry point for one valid expired marker. The shared
    /// serialized drain revalidates repository roots and writer state before
    /// retiring ownership. Physical payload reachability and reclamation remain
    /// the repository GC's responsibility. Returns whether the named marker
    /// was fully retired.
    pub async fn repair_expired_traces_inflight_marker(
        &self,
        session_id: &str,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        let entry = crate::internal::metadata::MetadataKv::get_with_conn(
            self.db_conn.as_ref(),
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            session_id,
            attempt_id,
        )
        .await
        .context("load expired traces marker for doctor repair")?;
        let Some(entry) = entry else {
            return Ok(true);
        };
        let marker =
            decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key)?;
        // A LIVE marker refuses retirement — but only when its time fields
        // are trustworthy: a future-dated row would otherwise read as "live"
        // forever and be unrepairable (W2 §C.4.3).
        if !marker.cleanup_pending
            && marker.time_fields_trustworthy(now_ms)
            && marker.is_live(now_ms)
        {
            return Ok(false);
        }
        self.drain_rejected_checkpoint_cleanup_jobs().await?;
        Ok(crate::internal::metadata::MetadataKv::get_with_conn(
            self.db_conn.as_ref(),
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            session_id,
            attempt_id,
        )
        .await
        .context("verify expired traces marker doctor repair")?
        .is_none())
    }

    async fn drain_rejected_checkpoint_cleanup_jobs_ignoring(
        &self,
        ignored_attempt: Option<(&str, &str)>,
    ) -> Result<()> {
        if !crate::utils::client_storage::ClientStorage::wait_for_background_tasks_until(
            Instant::now() + OBJECT_INDEX_CLEANUP_DRAIN_BUDGET,
        )
        .await
        {
            return Err(RejectedCheckpointCleanupDeferred {
                reason: "object-index queue did not drain within 5 seconds".to_string(),
            }
            .into());
        }
        let cleanup_deadline = Instant::now() + rejected_cleanup_traversal_duration();

        // Snapshot all DB and filesystem roots without a SQLite writer
        // transaction. Index helpers remain bounded and therefore do not hold
        // the repository writer lock while inspecting filesystem state.
        let initial = self
            .rejected_cleanup_root_snapshot(self.db_conn.as_ref(), cleanup_deadline)
            .await
            .map_err(|error| RejectedCheckpointCleanupDeferred {
                reason: format!("repository cleanup roots could not be snapshotted: {error:#}"),
            })?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let pending = initial
            .db
            .markers
            .iter()
            .filter(|marker| {
                marker.cleanup_pending
                    || !marker.is_live(now_ms)
                    // W2 §C.4.3: a future-dated (untrustworthy) row reads as
                    // "live" under the absolute deadline forever — it must be
                    // RETIRABLE here, or doctor can never unblock the
                    // fail-closed listing.
                    || !marker.time_fields_trustworthy(now_ms)
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }
        if let Some(other) = initial.db.markers.iter().find(|marker| {
            !marker.cleanup_pending
                && marker.time_fields_trustworthy(now_ms)
                && marker.is_live(now_ms)
                && ignored_attempt.is_none_or(|(session_id, attempt_id)| {
                    marker.session_id != session_id || marker.attempt_id != attempt_id
                })
        }) {
            return Err(CheckpointPruneGuardError::LiveWriterMarker {
                session_id: other.session_id.clone(),
                attempt_id: other.attempt_id.clone(),
                ttl_ms: other.ttl_ms,
            }
            .into());
        }
        if !initial.active_operations.is_empty() {
            return Err(RejectedCheckpointCleanupDeferred {
                reason: format!(
                    "repository operation state is active ({})",
                    initial.active_operations.join(", ")
                ),
            }
            .into());
        }

        // Rejected-checkpoint cleanup is deliberately non-destructive: it
        // retires durable writer ownership but leaves both payloads and
        // object-index rows for repository GC. Walking every ref graph here
        // therefore cannot make a deletion safer, while a large unrelated
        // history can make the marker impossible to retire. The stable root
        // snapshots below still fence concurrent writers and repository
        // operations; GC performs the eventual reachability proof when it
        // actually reclaims objects.

        // Re-read every root before retirement. Any concurrent ref, reflog,
        // worktree-index, operation-state, marker, or catalog change makes
        // the fence stale and leaves the durable cleanup job for a retry.
        let revalidated = self
            .rejected_cleanup_root_snapshot(self.db_conn.as_ref(), cleanup_deadline)
            .await
            .map_err(|error| RejectedCheckpointCleanupDeferred {
                reason: format!("repository cleanup roots could not be revalidated: {error:#}"),
            })?;
        if revalidated != initial {
            return Err(RejectedCheckpointCleanupDeferred {
                reason: "repository cleanup roots changed before ownership retirement".to_string(),
            }
            .into());
        }

        // Take the writer lock only for the final compare/retire phase.
        // Ref/reflog/marker/catalog writers cannot move after the comparison
        // until this transaction commits.
        let txn = self
            .db_conn
            .begin()
            .await
            .context("begin rejected checkpoint object cleanup")?;
        txn.execute(Statement::from_string(
            txn.get_database_backend(),
            "UPDATE metadata_kv SET updated_at = updated_at
             WHERE scope = 'agent_traces_inflight'"
                .to_string(),
        ))
        .await
        .context("lock traces marker registry for rejected object cleanup")?;
        let locked_db = self.rejected_cleanup_db_snapshot(&txn).await?;
        if locked_db != revalidated.db {
            txn.rollback().await.ok();
            return Err(RejectedCheckpointCleanupDeferred {
                reason: "database cleanup roots changed before the retirement lock was acquired"
                    .to_string(),
            }
            .into());
        }
        let RejectedCleanupIndexSnapshot {
            roots: locked_index_roots,
            fingerprints: locked_index_fingerprints,
            active_operations: mut locked_operations,
        } = match self.rejected_cleanup_index_snapshot(cleanup_deadline) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                txn.rollback().await.ok();
                return Err(RejectedCheckpointCleanupDeferred {
                    reason: format!(
                        "filesystem cleanup roots could not be locked before the deadline: {error:#}"
                    ),
                }
                .into());
            }
        };
        locked_operations.extend(locked_db.active_operations.iter().cloned());
        locked_operations.sort();
        locked_operations.dedup();
        if locked_index_roots != revalidated.index_roots
            || locked_index_fingerprints != revalidated.index_fingerprints
            || locked_operations != revalidated.active_operations
        {
            txn.rollback().await.ok();
            return Err(RejectedCheckpointCleanupDeferred {
                reason: "filesystem cleanup roots changed before ownership retirement".to_string(),
            }
            .into());
        }
        let locked_now_ms = chrono::Utc::now().timestamp_millis();
        let pending = locked_db
            .markers
            .iter()
            .filter(|marker| {
                marker.cleanup_pending
                    || !marker.is_live(locked_now_ms)
                    || !marker.time_fields_trustworthy(locked_now_ms)
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            txn.commit()
                .await
                .context("commit empty rejected checkpoint cleanup")?;
            return Ok(());
        }
        if let Some(other) = locked_db.markers.iter().find(|marker| {
            !marker.cleanup_pending
                && marker.time_fields_trustworthy(locked_now_ms)
                && marker.is_live(locked_now_ms)
                && ignored_attempt.is_none_or(|(session_id, attempt_id)| {
                    marker.session_id != session_id || marker.attempt_id != attempt_id
                })
        }) {
            txn.rollback().await.ok();
            return Err(CheckpointPruneGuardError::LiveWriterMarker {
                session_id: other.session_id.clone(),
                attempt_id: other.attempt_id.clone(),
                ttl_ms: other.ttl_ms,
            }
            .into());
        }
        // Do not unlink shared content-addressed payloads or object-index rows
        // here. Repository GC owns physical reclamation and its reachability
        // proof; this transaction only retires exact marker generations.
        for marker in pending {
            clear_traces_inflight_marker(&txn, &marker.session_id, &marker.attempt_id).await?;
        }
        txn.commit()
            .await
            .context("commit rejected checkpoint object cleanup")?;
        Ok(())
    }

    /// Remove checkpoint commits from this manager's ref and delete their
    /// `agent_checkpoint` rows.
    ///
    /// This is the `libra agent clean` counterpart to
    /// [`Self::append_checkpoint_commit`]. It rewrites the orphan
    /// `refs/libra/traces` chain from the checkpoint catalog, omitting
    /// the supplied checkpoint IDs. Rewriting is necessary because later
    /// committed checkpoints may descend from temporary checkpoints; simply
    /// moving the ref to an ancestor would either keep those temporary commits
    /// reachable or discard later retained checkpoints.
    ///
    /// Repositories that only have catalog rows and an empty traces ref
    /// (older fixtures, partial migrations, or pre-Phase-2 data) still get the
    /// catalog deletion without a ref rewrite.
    pub async fn prune_checkpoint_commits(
        &self,
        checkpoint_ids_to_remove: &[String],
    ) -> Result<CheckpointPruneOutcome> {
        self.prune_checkpoint_commits_inner(checkpoint_ids_to_remove, true)
            .await
    }

    async fn prune_checkpoint_commits_inner(
        &self,
        checkpoint_ids_to_remove: &[String],
        record_cloud_tombstones: bool,
    ) -> Result<CheckpointPruneOutcome> {
        // AG-20 observability (`agent.md` §6): one `agent.clean.prune` span
        // per prune. Required fields: deleted_objects, deleted_sessions,
        // window_guard, duration_ms. No raw filesystem path is ever
        // recorded (forbidden: raw path outside repo).
        let prune_span = tracing::info_span!(
            "agent.clean.prune",
            deleted_objects = tracing::field::Empty,
            deleted_sessions = tracing::field::Empty,
            window_guard = tracing::field::Empty,
            duration_ms = tracing::field::Empty,
        );
        let started = std::time::Instant::now();
        let finish_span = |guard: &'static str, deleted_objects: u64| {
            prune_span.record("deleted_objects", deleted_objects);
            // The prune never deletes `agent_session` rows (sessions are
            // retained for history; only checkpoint rows are dropped).
            prune_span.record("deleted_sessions", 0_u64);
            prune_span.record("window_guard", guard);
            prune_span.record("duration_ms", started.elapsed().as_millis() as u64);
        };

        let remove_set: HashSet<&str> = checkpoint_ids_to_remove
            .iter()
            .map(String::as_str)
            .collect();
        if remove_set.is_empty() {
            finish_span("noop", 0);
            return Ok(CheckpointPruneOutcome {
                removed_checkpoints: 0,
                rewritten_checkpoints: 0,
                ref_rewritten: false,
                window_guard: "noop",
                deleted_object_index_rows: 0,
                deleted_import_identities: 0,
            });
        }

        for attempt in 0..=HISTORY_HEAD_CONFLICT_MAX_RETRIES {
            let expected_head = self.resolve_history_head().await?;
            let rows = self.load_checkpoint_history_rows().await?;
            let existing_remove_ids = rows
                .iter()
                .filter(|row| remove_set.contains(row.checkpoint_id.as_str()))
                .map(|row| row.checkpoint_id.clone())
                .collect::<HashSet<_>>();

            if existing_remove_ids.is_empty() {
                finish_span("noop", 0);
                return Ok(CheckpointPruneOutcome {
                    removed_checkpoints: 0,
                    rewritten_checkpoints: 0,
                    ref_rewritten: false,
                    window_guard: "noop",
                    deleted_object_index_rows: 0,
                    deleted_import_identities: 0,
                });
            }

            // AG-20 window A/B guards — both must pass before any rewrite.
            if let Err(guard_err) = self.enforce_prune_window_guards(expected_head, &rows).await {
                let guard_label = if guard_err
                    .downcast_ref::<SubagentContentReservationPruneGuard>()
                    .is_some()
                {
                    "subagent_reservation_blocked"
                } else {
                    match guard_err.downcast_ref::<CheckpointPruneGuardError>() {
                        Some(CheckpointPruneGuardError::LiveWriterMarker { .. }) => {
                            "live_marker_blocked"
                        }
                        Some(CheckpointPruneGuardError::RefCatalogOrphans { .. }) => {
                            "catalog_orphans_blocked"
                        }
                        // A guard that cannot complete (unreadable chain,
                        // marker-listing failure) still fails the prune closed.
                        None => "guard_check_failed",
                    }
                };
                finish_span(guard_label, 0);
                return Err(guard_err);
            }

            let (retained_rows, removed_rows): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|row| !existing_remove_ids.contains(&row.checkpoint_id));

            let (new_head, rewritten) = match expected_head {
                Some(head) => self.rebuild_checkpoint_history(head, &retained_rows)?,
                None => (None, Vec::new()),
            };

            let unreachable_oids =
                collect_exclusive_unreachable_oids(&removed_rows, &retained_rows, &rewritten);

            match self
                .commit_checkpoint_prune(
                    expected_head,
                    new_head,
                    &rewritten,
                    &existing_remove_ids,
                    &unreachable_oids,
                    record_cloud_tombstones,
                )
                .await?
            {
                (
                    RefUpdateOutcome::Updated,
                    removed_checkpoints,
                    deleted_object_index_rows,
                    deleted_import_identities,
                ) => {
                    finish_span("markers_and_catalog_verified", deleted_object_index_rows);
                    return Ok(CheckpointPruneOutcome {
                        removed_checkpoints,
                        rewritten_checkpoints: rewritten.len(),
                        ref_rewritten: expected_head != new_head,
                        window_guard: "markers_and_catalog_verified",
                        deleted_object_index_rows,
                        deleted_import_identities,
                    });
                }
                (RefUpdateOutcome::HeadChanged, _, _, _)
                    if attempt < HISTORY_HEAD_CONFLICT_MAX_RETRIES =>
                {
                    continue;
                }
                (RefUpdateOutcome::HeadChanged, _, _, _) => {
                    return Err(anyhow!(
                        "traces head changed repeatedly while pruning checkpoints"
                    ));
                }
            }
        }

        unreachable!("checkpoint prune retry loop must return on success or terminal error")
    }

    /// AG-24a local erasure for one session (plan.md Task A8.5): make the
    /// three local faces consistent — rewrite `refs/libra/traces` to drop
    /// the session's checkpoints, delete its `agent_checkpoint` and
    /// `agent_session` rows, and clean the now-unreachable `object_index`
    /// rows. The append-only `agent_audit_log` is a separate table and is
    /// never touched.
    ///
    /// Order matters: checkpoints are pruned FIRST (while the catalog rows
    /// still exist, so the ref rewrite can enumerate what to keep), then
    /// the `agent_session` row is deleted. Deleting the session first would
    /// cascade its checkpoint rows away (FK `ON DELETE CASCADE`) and leave
    /// `refs/libra/traces` pointing at orphan commits.
    ///
    /// D1/R2 cloud-mirror deletion propagation is explicitly out of scope
    /// (documented deferral): this covers local consistency only.
    pub async fn erase_session_local(&self, session_id: &str) -> Result<SessionEraseOutcome> {
        use sea_orm::{Statement, Value};
        let backend = self.db_conn.get_database_backend();

        // ADR-DR-19 (M4): establish the anti-resurrection barrier BEFORE
        // pruning/deleting anything. The same transaction fences every
        // import/export/coverage holder for this provider identity. If the
        // later ref prune is interrupted, the session remains tombstoned and
        // a retry can safely finish deletion; no in-flight writer can revive
        // it in the meantime.
        let identity = self
            .db_conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT agent_kind, provider_session_id, metadata_json
                 FROM agent_session WHERE session_id = ?",
                [Value::from(session_id.to_string())],
            ))
            .await
            .context("read provider identity for session erasure")?;
        let provider_identity = if let Some(row) = identity {
            let agent_kind: String = row.try_get_by("agent_kind")?;
            let provider_session_id: String = row.try_get_by("provider_session_id")?;
            let metadata_json: String = row.try_get_by("metadata_json")?;
            let source_fingerprint = serde_json::from_str::<serde_json::Value>(&metadata_json)
                .ok()
                .and_then(|metadata| {
                    metadata
                        .get("source_fingerprint")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| {
                            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                        })
                        .map(str::to_owned)
                });
            let txn = self
                .db_conn
                .begin()
                .await
                .context("begin agent erasure tombstone transaction")?;
            let incarnation_namespace = uuid::Uuid::new_v4().simple().to_string();
            let incarnation = txn
                .execute(Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO agent_capture_incarnation (
                        agent_kind, provider_session_id, next_session_sync_revision,
                        source_namespace, updated_at
                     )
                     SELECT agent_kind, provider_session_id,
                            MAX(sync_revision + 1, 2), ?, ?
                     FROM agent_session WHERE session_id = ?
                     ON CONFLICT(agent_kind, provider_session_id) DO UPDATE SET
                        next_session_sync_revision = MAX(
                            agent_capture_incarnation.next_session_sync_revision,
                            excluded.next_session_sync_revision
                        ),
                        source_namespace = excluded.source_namespace,
                        updated_at = excluded.updated_at",
                    [
                        incarnation_namespace.into(),
                        chrono::Utc::now().timestamp_millis().into(),
                        session_id.into(),
                    ],
                ))
                .await
                .context("preserve agent capture replication incarnation before erasure")?;
            if incarnation.rows_affected() != 1 {
                if let Err(rollback) = txn.rollback().await {
                    bail!(
                        "agent session disappeared while preserving its cloud replication \
                         incarnation, and rolling back that failed transaction also failed: \
                         {rollback}; retry the erase after checking the local database"
                    );
                }
                bail!(
                    "agent session disappeared while preserving its cloud replication incarnation; retry the erase"
                );
            }
            txn.execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO agent_import_tombstone (
                    tombstone_id, agent_kind, provider_session_id,
                    erased_session_id, source_fingerprint, erased_at
                 ) VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(agent_kind, provider_session_id) DO UPDATE SET
                    erased_session_id = excluded.erased_session_id,
                    source_fingerprint = COALESCE(
                        excluded.source_fingerprint,
                        agent_import_tombstone.source_fingerprint
                    ),
                    erased_at = excluded.erased_at",
                [
                    uuid::Uuid::new_v4().to_string().into(),
                    agent_kind.clone().into(),
                    provider_session_id.clone().into(),
                    session_id.into(),
                    Value::from(source_fingerprint),
                    chrono::Utc::now().timestamp_millis().into(),
                ],
            ))
            .await
            .context("write agent import anti-resurrection tombstone")?;
            txn.execute(Statement::from_sql_and_values(
                backend,
                "UPDATE agent_import_identity
                 SET state = 'failed', owner = NULL, lease_expires_at = NULL,
                     fence_token = COALESCE(fence_token, 0) + 1,
                     last_error_code = 'LBR-AGENT-019', updated_at = ?
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [
                    chrono::Utc::now().timestamp_millis().into(),
                    agent_kind.clone().into(),
                    provider_session_id.clone().into(),
                ],
            ))
            .await
            .context("fence import identity holders during erasure")?;
            txn.execute(Statement::from_sql_and_values(
                backend,
                "UPDATE agent_coverage_claim
                 SET state = 'abandoned', owner = NULL, lease_expires_at = NULL,
                     fence_token = COALESCE(fence_token, 0) + 1, updated_at = ?
                 WHERE session_id = ? AND state IN ('reserved_live','reserved_import')",
                [
                    chrono::Utc::now().timestamp_millis().into(),
                    session_id.into(),
                ],
            ))
            .await
            .context("fence coverage claim holders during erasure")?;
            txn.execute(Statement::from_sql_and_values(
                backend,
                "UPDATE agent_export_job
                 SET state = 'failed', owner = NULL, lease_expires_at = NULL,
                     fence_token = fence_token + 1,
                     last_error_code = 'LBR-AGENT-019', updated_at = ?
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [
                    chrono::Utc::now().timestamp_millis().into(),
                    agent_kind.clone().into(),
                    provider_session_id.clone().into(),
                ],
            ))
            .await
            .context("fence export job holders during erasure")?;
            txn.commit()
                .await
                .context("commit agent erasure tombstone transaction")?;
            Some((agent_kind, provider_session_id))
        } else {
            None
        };

        // A session can be between reservation and its first catalog row, so
        // an empty checkpoint list does not prove that erasure has no writer
        // to race. Marker creation is serialized with the tombstone
        // transaction above; once the tombstone wins, no new marker for this
        // provider identity can be created. Refuse this attempt until the
        // already-marked writer finishes or its rejected objects are cleaned.
        let live_markers = list_live_traces_inflight_markers(
            self.db_conn.as_ref(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .context("verify in-flight writers before agent session erasure")?;
        if let Some(marker) = live_markers
            .into_iter()
            .find(|marker| marker.session_id == session_id)
        {
            return Err(CheckpointPruneGuardError::LiveWriterMarker {
                session_id: marker.session_id,
                attempt_id: marker.attempt_id,
                ttl_ms: marker.ttl_ms,
            }
            .into());
        }

        // Enumerate the session's checkpoints from the catalog.
        let rows = self
            .db_conn
            .query_all(Statement::from_sql_and_values(
                backend,
                "SELECT checkpoint_id FROM agent_checkpoint WHERE session_id = ?",
                [Value::from(session_id.to_string())],
            ))
            .await
            .context("list checkpoints for session erasure")?;
        let checkpoint_ids: Vec<String> = rows
            .into_iter()
            .map(|row| row.try_get_by::<String, _>("checkpoint_id"))
            .collect::<std::result::Result<_, _>>()
            .context("decode checkpoint_id for session erasure")?;

        // Prune the checkpoints (ref rewrite + row + object_index) BEFORE
        // deleting the session row.
        // Session erasure deliberately remains local-only (ADR-DR-15). Do not
        // create ordinary retention tombstones here: a later cloud restore is
        // documented to be able to resurrect the remote session snapshot.
        let prune = self
            .prune_checkpoint_commits_inner(&checkpoint_ids, false)
            .await?;

        // Delete the session row (cascades claims/revisions/checkpoints) and
        // application-owned import/export job rows together. The tombstone is
        // deliberately retained outside ordinary retention.
        let txn = self
            .db_conn
            .begin()
            .await
            .context("begin agent session catalog erasure")?;
        let deleted = txn
            .execute(Statement::from_sql_and_values(
                backend,
                "DELETE FROM agent_session WHERE session_id = ?",
                [Value::from(session_id.to_string())],
            ))
            .await
            .context("delete agent_session row for erasure")?;
        txn.execute(Statement::from_sql_and_values(
            backend,
            "DELETE FROM metadata_kv WHERE scope = ? AND target = ?",
            [
                crate::internal::metadata::MetadataScope::AgentImportIndexRepair
                    .as_str()
                    .into(),
                session_id.into(),
            ],
        ))
        .await
        .context("delete import object-index repair marker for erased session")?;
        if let Some((agent_kind, provider_session_id)) = provider_identity {
            txn.execute(Statement::from_sql_and_values(
                backend,
                "DELETE FROM agent_import_identity
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [
                    agent_kind.clone().into(),
                    provider_session_id.clone().into(),
                ],
            ))
            .await
            .context("delete import identity rows for erased session")?;
            txn.execute(Statement::from_sql_and_values(
                backend,
                "DELETE FROM agent_export_job
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [agent_kind.into(), provider_session_id.into()],
            ))
            .await
            .context("delete export job rows for erased session")?;
        }
        txn.commit()
            .await
            .context("commit agent session catalog erasure")?;

        Ok(SessionEraseOutcome {
            session_deleted: deleted.rows_affected() > 0,
            removed_checkpoints: prune.removed_checkpoints,
            ref_rewritten: prune.ref_rewritten,
            deleted_object_index_rows: prune.deleted_object_index_rows,
        })
    }

    /// AG-20 window A/B prune guards (`agent.md` write-sequence matrix,
    /// rows 727-732). Both refusals are deterministic and fail-closed:
    ///
    /// - **Window A/B (live writer)**: any live in-flight marker — for ANY
    ///   session, not just the ones being pruned — blocks the prune. The
    ///   prune is a whole-chain rewrite of the shared `refs/libra/traces`
    ///   ref plus the catalog, so a concurrent writer between stages
    ///   (a)–(d) could otherwise lose loose objects (window A) or a
    ///   ref-reachable-but-uncataloged commit (window B). Markers carry a
    ///   TTL ([`AGENT_TRACES_INFLIGHT_TTL_MS`]), so a crashed writer only
    ///   defers pruning temporarily.
    /// - **Window B residue (ref-vs-catalog)**: walks the first-parent
    ///   chain of the current traces head and refuses when any reachable
    ///   commit has no `agent_checkpoint.traces_commit` row. The rebuild is
    ///   catalog-driven and would silently drop such commits; backfilling
    ///   the catalog is `libra agent doctor --repair`'s job.
    async fn enforce_prune_window_guards(
        &self,
        expected_head: Option<ObjectHash>,
        rows: &[CheckpointHistoryRow],
    ) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        // A marker-listing failure cannot prove the absence of a live
        // writer — propagate it, which aborts (fails closed) the prune.
        let live_markers = list_live_traces_inflight_markers(self.db_conn.as_ref(), now_ms)
            .await
            .context("failed to verify traces in-flight markers (prune fails closed)")?;
        if let Some(marker) = live_markers.first() {
            return Err(CheckpointPruneGuardError::LiveWriterMarker {
                session_id: marker.session_id.clone(),
                attempt_id: marker.attempt_id.clone(),
                ttl_ms: marker.ttl_ms,
            }
            .into());
        }

        // DR-06 closes the short reservation→marker window: a subagent
        // source claim is durable before its marker can be registered. A
        // whole-chain prune in that interval could delete the claim's current
        // leaf and invalidate the writer's source revision base, so fail
        // closed on every unexpired reservation just as we do for markers.
        let reserved = self
            .db_conn
            .query_one(Statement::from_sql_and_values(
                self.db_conn.get_database_backend(),
                "SELECT parent_session_id, attempt_checkpoint_id, lease_expires_at
                 FROM agent_subagent_content_claim
                 WHERE state = 'reserved' AND lease_expires_at > ?
                 ORDER BY lease_expires_at LIMIT 1",
                [now_ms.into()],
            ))
            .await
            .context("failed to verify subagent content reservations (prune fails closed)")?;
        if let Some(row) = reserved {
            return Err(SubagentContentReservationPruneGuard {
                session_id: row.try_get_by("parent_session_id")?,
                attempt_id: row
                    .try_get_by::<Option<String>, _>("attempt_checkpoint_id")?
                    .unwrap_or_else(|| "reservation-before-checkpoint-bind".to_string()),
                lease_expires_at: row.try_get_by("lease_expires_at")?,
            }
            .into());
        }

        let Some(head) = expected_head else {
            return Ok(());
        };
        let cataloged: HashSet<&str> = rows
            .iter()
            .filter_map(|row| row.traces_commit.as_deref())
            .collect();
        // An unreadable chain means the catalog cannot be verified —
        // propagate the walk error (fail closed) rather than pruning blind.
        let mut orphans: Vec<String> = Vec::new();
        for commit_hash in self.first_parent_commit_hashes(head)? {
            if !cataloged.contains(commit_hash.as_str()) {
                orphans.push(commit_hash);
            }
        }
        if let Some(first_commit) = orphans.first().cloned() {
            return Err(CheckpointPruneGuardError::RefCatalogOrphans {
                orphan_count: orphans.len(),
                first_commit,
            }
            .into());
        }
        Ok(())
    }

    /// First-parent commit hashes reachable from `head` (head first),
    /// with a visited-set cycle guard.
    fn first_parent_commit_hashes(&self, head: ObjectHash) -> Result<Vec<String>> {
        let mut hashes = Vec::new();
        let mut visited: HashSet<ObjectHash> = HashSet::new();
        let mut next = Some(head);
        while let Some(oid) = next {
            if !visited.insert(oid) {
                break;
            }
            let data = read_git_object(&self.repo_path, &oid).with_context(|| {
                format!("failed to read traces commit {oid} while walking refs/libra/traces")
            })?;
            let commit = Commit::from_bytes(&data, oid)
                .map_err(|err| anyhow!("failed to parse traces commit {oid}: {err}"))?;
            hashes.push(oid.to_string());
            next = commit.parent_commit_ids.first().copied();
        }
        Ok(hashes)
    }

    async fn load_checkpoint_history_rows(&self) -> Result<Vec<CheckpointHistoryRow>> {
        let backend = self.db_conn.get_database_backend();
        let rows = self
            .db_conn
            .query_all(Statement::from_string(
                backend,
                "SELECT cp.checkpoint_id, cp.session_id, cp.scope, cp.parent_commit, \
                        cp.traces_commit, cp.tree_oid, cp.metadata_blob_oid, cp.created_at, \
                        COALESCE(s.agent_kind, 'unknown') AS agent_kind \
                 FROM agent_checkpoint cp \
                 LEFT JOIN agent_session s ON s.session_id = cp.session_id \
                 ORDER BY cp.created_at ASC, cp.checkpoint_id ASC"
                    .to_string(),
            ))
            .await
            .context("failed to load agent_checkpoint rows for traces rewrite")?;

        rows.into_iter()
            .map(CheckpointHistoryRow::from_query_result)
            .collect()
    }

    fn rebuild_checkpoint_history(
        &self,
        current_head: ObjectHash,
        retained_rows: &[CheckpointHistoryRow],
    ) -> Result<(Option<ObjectHash>, Vec<RewrittenCheckpoint>)> {
        if retained_rows.is_empty() {
            return Ok((None, Vec::new()));
        }

        let current_root = self.load_commit_tree(&current_head)?;
        let mut parent = None;
        let mut rewritten = Vec::with_capacity(retained_rows.len());

        for row in retained_rows {
            let inner_tree = self
                .checkpoint_inner_tree_from_root(&current_root, &row.checkpoint_id)?
                .ok_or_else(|| {
                    anyhow!(
                        "traces tree is missing retained checkpoint {}",
                        row.checkpoint_id
                    )
                })?;
            let (prefix, rest) = checkpoint_tree_path(&row.checkpoint_id)?;
            let root_tree = self.splice_checkpoint_tree(parent, &prefix, &rest, inner_tree)?;
            let commit_hash = self.write_rewritten_checkpoint_commit(parent, root_tree, row)?;
            rewritten.push(RewrittenCheckpoint {
                checkpoint_id: row.checkpoint_id.clone(),
                traces_commit: commit_hash,
                tree_oid: root_tree,
            });
            parent = Some(commit_hash);
        }

        Ok((parent, rewritten))
    }

    fn checkpoint_inner_tree_from_root(
        &self,
        root_items: &[TreeItem],
        checkpoint_id: &str,
    ) -> Result<Option<ObjectHash>> {
        let (prefix, rest) = checkpoint_tree_path(checkpoint_id)?;
        let Some(checkpoint_entry) = root_items.iter().find(|item| item.name == "checkpoint")
        else {
            return Ok(None);
        };
        if checkpoint_entry.mode != TreeItemMode::Tree {
            return Err(anyhow!(
                "traces tree corruption: 'checkpoint' entry expected to be a tree, got mode {:?}",
                checkpoint_entry.mode
            ));
        }

        let checkpoint_items = self.load_tree(&checkpoint_entry.id)?;
        let Some(prefix_entry) = checkpoint_items.iter().find(|item| item.name == prefix) else {
            return Ok(None);
        };
        if prefix_entry.mode != TreeItemMode::Tree {
            return Err(anyhow!(
                "traces tree corruption: 'checkpoint/{prefix}' entry expected to be a tree, got mode {:?}",
                prefix_entry.mode
            ));
        }

        let prefix_items = self.load_tree(&prefix_entry.id)?;
        let Some(rest_entry) = prefix_items.iter().find(|item| item.name == rest) else {
            return Ok(None);
        };
        if rest_entry.mode != TreeItemMode::Tree {
            return Err(anyhow!(
                "traces tree corruption: 'checkpoint/{prefix}/{rest}' entry expected to be a tree, got mode {:?}",
                rest_entry.mode
            ));
        }
        Ok(Some(rest_entry.id))
    }

    fn write_rewritten_checkpoint_commit(
        &self,
        parent: Option<ObjectHash>,
        root_tree: ObjectHash,
        row: &CheckpointHistoryRow,
    ) -> Result<ObjectHash> {
        let message = format!(
            "traces: {} checkpoint {}\n\n{}",
            row.scope,
            row.checkpoint_id,
            format_rewritten_checkpoint_trailers(row)
        );
        let author = Signature::new(
            SignatureType::Author,
            "Libra".to_string(),
            "traces@libra".to_string(),
        );
        let committer = Signature::new(
            SignatureType::Committer,
            "Libra".to_string(),
            "traces@libra".to_string(),
        );
        let parents = parent.into_iter().collect::<Vec<_>>();
        let commit = Commit::new(author, committer, root_tree, parents, &message);
        let commit_data = commit
            .to_data()
            .context("failed to serialize rewritten checkpoint commit")?;
        let commit_hash = write_git_object(&self.repo_path, "commit", &commit_data)?;
        crate::utils::client_storage::enqueue_agent_blob_object_index_update(
            &self.repo_path,
            &commit_hash.to_string(),
            "commit",
            commit_data.len() as i64,
        )
        .with_context(|| {
            format!("register durable object-index repair for rewritten commit {commit_hash}")
        })?;
        Ok(commit_hash)
    }

    /// Transactionally CAS the traces ref, update rewritten rows, delete
    /// pruned rows, and drop `object_index` rows for
    /// `unreachable_oids` (the conservative exclusively-removed set from
    /// [`collect_exclusive_unreachable_oids`]). The `object_index` deletion
    /// is idempotent — re-running deletes nothing — and rides in the same
    /// transaction so a crash cannot leave the catalog and the index
    /// disagreeing about the pruned checkpoints.
    ///
    /// Returns `(outcome, removed_rows, deleted_object_index_rows,
    /// deleted_import_identities)`.
    async fn commit_checkpoint_prune(
        &self,
        expected_head: Option<ObjectHash>,
        new_head: Option<ObjectHash>,
        rewritten: &[RewrittenCheckpoint],
        remove_ids: &HashSet<String>,
        unreachable_oids: &[String],
        record_cloud_tombstones: bool,
    ) -> Result<(RefUpdateOutcome, u64, u64, u64)> {
        let expected_commit = expected_head.map(|hash| hash.to_string());
        let new_commit = new_head.map(|hash| hash.to_string());
        let _object_index_deletion_fence =
            crate::utils::client_storage::acquire_object_index_deletion_fence(
                &self.repo_path.join(crate::utils::util::DATABASE),
                unreachable_oids,
            )
            .await
            .with_context(|| {
                "refusing checkpoint prune because concurrent object-index repair work could recreate a deleted catalog row; retry after the repair marker drains"
            })?;

        'retry_sqlite: for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            let txn: DatabaseTransaction = match self.db_conn.begin().await {
                Ok(txn) => txn,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => {
                    return Err(err).context("Failed to begin checkpoint prune transaction");
                }
            };

            let existing = match reference::Entity::find()
                .filter(reference::Column::Name.eq(&self.ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&txn)
                .await
            {
                Ok(existing) => existing,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to query checkpoint prune ref"),
            };

            let write_ref = match existing {
                Some(model) if model.commit != expected_commit => {
                    let _ = txn.rollback().await;
                    return Ok((RefUpdateOutcome::HeadChanged, 0, 0, 0));
                }
                Some(model) => {
                    let mut active: reference::ActiveModel = model.into();
                    active.commit = Set(new_commit.clone());
                    active.update(&txn).await.map(|_| ())
                }
                None if expected_commit.is_some() => {
                    let _ = txn.rollback().await;
                    return Ok((RefUpdateOutcome::HeadChanged, 0, 0, 0));
                }
                None => {
                    let new_ref = reference::ActiveModel {
                        name: Set(Some(self.ref_name.clone())),
                        kind: Set(ConfigKind::Branch),
                        commit: Set(new_commit.clone()),
                        remote: Set(None),
                        ..Default::default()
                    };
                    new_ref.insert(&txn).await.map(|_| ())
                }
            };

            if let Err(err) = write_ref {
                if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                return Err(err).context("Failed to update checkpoint prune ref");
            }

            let backend = txn.get_database_backend();
            for item in rewritten {
                if let Err(err) = txn
                    .execute(Statement::from_sql_and_values(
                        backend,
                        "UPDATE agent_checkpoint SET traces_commit = ?, tree_oid = ?, \
                            sync_revision = sync_revision + 1 \
                         WHERE checkpoint_id = ?",
                        vec![
                            Value::from(item.traces_commit.to_string()),
                            Value::from(item.tree_oid.to_string()),
                            Value::from(item.checkpoint_id.clone()),
                        ],
                    ))
                    .await
                {
                    if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES {
                        let _ = txn.rollback().await;
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue 'retry_sqlite;
                    }
                    return Err(err).context("Failed to update rewritten checkpoint row");
                }
            }

            let mut removed = 0;
            for id in remove_ids {
                if record_cloud_tombstones {
                    txn.execute(Statement::from_sql_and_values(
                        backend,
                        "INSERT INTO agent_checkpoint_prune_tombstone (
                            checkpoint_id, session_id, pruned_at
                         )
                         SELECT checkpoint_id, session_id, ?
                         FROM agent_checkpoint WHERE checkpoint_id = ?
                         ON CONFLICT(checkpoint_id) DO UPDATE SET
                            session_id = excluded.session_id,
                            pruned_at = MAX(
                                agent_checkpoint_prune_tombstone.pruned_at,
                                excluded.pruned_at
                            )",
                        [
                            Value::from(chrono::Utc::now().timestamp_millis()),
                            Value::from(id.clone()),
                        ],
                    ))
                    .await
                    .context("record cloud fence for pruned checkpoint")?;
                }
                // DR-06: subagent content revisions are source-scoped, not
                // checkpoint-parent scoped. Repoint the current source leaf
                // to its newest surviving revision before the checkpoint FK
                // cascades its revision/link.  Keep an empty claim as the
                // durable revision high-water mark so a later capture cannot
                // reuse an audited revision number for different content.
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_subagent_content_revision
                     WHERE checkpoint_id = ?",
                    [Value::from(id.clone())],
                ))
                .await
                .context("delete subagent content revision for pruned checkpoint")?;
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    "UPDATE agent_subagent_content_claim
                     SET sync_revision = sync_revision + 1,
                         current_revision = COALESCE((
                           SELECT r.revision FROM agent_subagent_content_revision r
                           WHERE r.parent_session_id = agent_subagent_content_claim.parent_session_id
                             AND r.provider_kind = agent_subagent_content_claim.provider_kind
                             AND r.source_key = agent_subagent_content_claim.source_key
                             AND r.content_schema_version = agent_subagent_content_claim.content_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ), 0),
                         current_checkpoint_id = (
                           SELECT r.checkpoint_id FROM agent_subagent_content_revision r
                           WHERE r.parent_session_id = agent_subagent_content_claim.parent_session_id
                             AND r.provider_kind = agent_subagent_content_claim.provider_kind
                             AND r.source_key = agent_subagent_content_claim.source_key
                             AND r.content_schema_version = agent_subagent_content_claim.content_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         current_digest = (
                           SELECT r.content_digest FROM agent_subagent_content_revision r
                           WHERE r.parent_session_id = agent_subagent_content_claim.parent_session_id
                             AND r.provider_kind = agent_subagent_content_claim.provider_kind
                             AND r.source_key = agent_subagent_content_claim.source_key
                             AND r.content_schema_version = agent_subagent_content_claim.content_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         state = 'idle', attempt_digest = NULL,
                         attempt_checkpoint_id = NULL, owner = NULL,
                         lease_expires_at = NULL, updated_at = ?
                     WHERE current_checkpoint_id = ?",
                    [
                        Value::from(chrono::Utc::now().timestamp_millis()),
                        Value::from(id.clone()),
                    ],
                ))
                .await
                .context("repoint subagent content claim after checkpoint prune")?;
                // GC-DR-11: coverage revisions/current pointers and import
                // attempt cursors are part of the same catalog fact as the
                // checkpoint. Reconcile them before deleting the row so no
                // committed claim can point at a pruned checkpoint.
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_coverage_conflict
                     WHERE incumbent_checkpoint_id = ?
                        OR EXISTS (
                          SELECT 1 FROM agent_coverage_claim c
                          WHERE c.session_id = agent_coverage_conflict.session_id
                            AND c.logical_turn_key = agent_coverage_conflict.logical_turn_key
                            AND c.coverage_schema_version = agent_coverage_conflict.coverage_schema_version
                            AND c.checkpoint_id = ?
                        )",
                    [Value::from(id.clone()), Value::from(id.clone())],
                ))
                .await
                .context("delete conflict evidence whose incumbent checkpoint is pruned")?;
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_coverage_revision WHERE checkpoint_id = ?",
                    [Value::from(id.clone())],
                ))
                .await
                .context("delete coverage revisions for pruned checkpoint")?;
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_coverage_claim
                     WHERE checkpoint_id = ?
                       AND NOT EXISTS (
                         SELECT 1 FROM agent_coverage_revision r
                         WHERE r.session_id = agent_coverage_claim.session_id
                           AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                           AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                       )",
                    [Value::from(id.clone())],
                ))
                .await
                .context("delete coverage claims emptied by checkpoint prune")?;
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    "UPDATE agent_coverage_claim
                     SET revision = (
                           SELECT r.revision FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         coverage_digest = (
                           SELECT r.coverage_digest FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         completeness = (
                           SELECT r.completeness FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         source_channel = (
                           SELECT r.source_channel FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         checkpoint_id = (
                           SELECT r.checkpoint_id FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         traces_commit = (
                           SELECT c.traces_commit
                           FROM agent_coverage_revision r
                           JOIN agent_checkpoint c ON c.checkpoint_id = r.checkpoint_id
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         state = 'catalog_committed', owner = NULL,
                         lease_expires_at = NULL, updated_at = ?
                     WHERE checkpoint_id = ?",
                    [
                        Value::from(chrono::Utc::now().timestamp_millis()),
                        Value::from(id.clone()),
                    ],
                ))
                .await
                .context("repoint coverage claim after checkpoint prune")?;
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    "UPDATE agent_import_identity SET attempt_checkpoint_id = NULL
                     WHERE attempt_checkpoint_id = ?",
                    [Value::from(id.clone())],
                ))
                .await
                .context("clear pruned import attempt checkpoint pointer")?;
                match txn
                    .execute(Statement::from_sql_and_values(
                        backend,
                        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
                        [Value::from(id.clone())],
                    ))
                    .await
                {
                    Ok(result) => removed += result.rows_affected(),
                    Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                        let _ = txn.rollback().await;
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue 'retry_sqlite;
                    }
                    Err(err) => return Err(err).context("Failed to delete pruned checkpoint row"),
                }
            }

            let deleted_import_identities = txn
                .execute(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_import_identity
                 WHERE state IN ('discovered','partial','committed','failed')
                   AND owner IS NULL
                   AND EXISTS (
                     SELECT 1 FROM agent_session s
                     WHERE s.agent_kind = agent_import_identity.agent_kind
                       AND s.provider_session_id = agent_import_identity.provider_session_id
                       AND NOT EXISTS (
                         SELECT 1 FROM agent_coverage_claim c
                         WHERE c.session_id = s.session_id
                       )
                 )",
                    Vec::<Value>::new(),
                ))
                .await
                .context("delete import identity after pruning its final coverage claim")?
                .rows_affected();

            // AG-20: drop `object_index` rows for OIDs this prune made
            // unreachable so cloud sync stops advertising them. Rides in
            // the same transaction; idempotent (missing rows delete 0).
            let deleted_object_index_rows =
                match crate::utils::client_storage::remove_object_index_rows_with_conn(
                    &txn,
                    unreachable_oids,
                )
                .await
                {
                    Ok(count) => count,
                    Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                        let _ = txn.rollback().await;
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue 'retry_sqlite;
                    }
                    Err(err) => {
                        return Err(err).context("Failed to delete pruned object_index rows");
                    }
                };

            match txn.commit().await {
                Ok(()) => {
                    return Ok((
                        RefUpdateOutcome::Updated,
                        removed,
                        deleted_object_index_rows,
                        deleted_import_identities,
                    ));
                }
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => {
                    return Err(err).context("Failed to commit checkpoint prune transaction");
                }
            }
        }

        unreachable!("sqlite busy retry loop must return on success or terminal error")
    }

    /// Splice `inner_tree` into `parent`'s tree at the path
    /// `checkpoint/<prefix>/<rest>`, preserving any existing entries in the
    /// surrounding subtrees. Phase 2.1 helper for [`append_checkpoint_commit`].
    fn splice_checkpoint_tree(
        &self,
        parent: Option<ObjectHash>,
        prefix: &str,
        rest: &str,
        inner_tree: ObjectHash,
    ) -> Result<ObjectHash> {
        let mut ignored = HashSet::new();
        self.splice_checkpoint_tree_tracked(parent, prefix, rest, inner_tree, &mut ignored)
    }

    fn splice_checkpoint_tree_tracked(
        &self,
        parent: Option<ObjectHash>,
        prefix: &str,
        rest: &str,
        inner_tree: ObjectHash,
        newly_written: &mut HashSet<String>,
    ) -> Result<ObjectHash> {
        let mut root_items = match parent {
            Some(parent_id) => self.load_commit_tree(&parent_id)?,
            None => Vec::new(),
        };
        let checkpoint_entry = root_items
            .iter()
            .find(|item| item.name == "checkpoint")
            .cloned();
        let mut checkpoint_items = match checkpoint_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => self.load_tree(&entry.id)?,
            Some(entry) => {
                return Err(anyhow!(
                    "traces tree corruption: 'checkpoint' entry expected to be a tree, \
                     got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                ));
            }
            None => Vec::new(),
        };

        let prefix_entry = checkpoint_items
            .iter()
            .find(|item| item.name == prefix)
            .cloned();
        let mut prefix_items = match prefix_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => self.load_tree(&entry.id)?,
            Some(entry) => {
                return Err(anyhow!(
                    "traces tree corruption: 'checkpoint/{prefix}' entry expected to be a \
                     tree, got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                ));
            }
            None => Vec::new(),
        };

        prefix_items.retain(|item| item.name != rest);
        prefix_items.push(TreeItem::new(
            TreeItemMode::Tree,
            inner_tree,
            rest.to_string(),
        ));
        prefix_items.sort_by(|a, b| a.name.cmp(&b.name));
        // Phase 3.5c: tag every tree spliced into the agent capture
        // history so cloud sync uploads the full reachability set.
        let prefix_tree = self.write_tree_indexed_tracked(&prefix_items, "tree", newly_written)?;

        checkpoint_items.retain(|item| item.name != prefix);
        checkpoint_items.push(TreeItem::new(
            TreeItemMode::Tree,
            prefix_tree,
            prefix.to_string(),
        ));
        checkpoint_items.sort_by(|a, b| a.name.cmp(&b.name));
        let checkpoint_tree =
            self.write_tree_indexed_tracked(&checkpoint_items, "tree", newly_written)?;

        root_items.retain(|item| item.name != "checkpoint");
        root_items.push(TreeItem::new(
            TreeItemMode::Tree,
            checkpoint_tree,
            "checkpoint".to_string(),
        ));
        root_items.sort_by(|a, b| a.name.cmp(&b.name));
        self.write_tree_indexed_tracked(&root_items, "tree", newly_written)
    }

    #[allow(clippy::too_many_arguments)]
    async fn splice_checkpoint_tree_for_attempt(
        &self,
        parent: Option<ObjectHash>,
        prefix: &str,
        rest: &str,
        inner_tree: ObjectHash,
        writer_fence: &TracesWriterFence,
        deadline: Option<Instant>,
        newly_written: &mut HashSet<String>,
    ) -> Result<ObjectHash> {
        let mut root_items = match parent {
            Some(parent_id) => {
                self.load_commit_tree_for_attempt(&parent_id, deadline)
                    .await?
            }
            None => Vec::new(),
        };
        let checkpoint_entry = root_items
            .iter()
            .find(|item| item.name == "checkpoint")
            .cloned();
        let mut checkpoint_items = match checkpoint_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => {
                self.load_tree_for_attempt(&entry.id, deadline).await?
            }
            Some(entry) => {
                bail!(
                    "traces tree corruption: 'checkpoint' entry expected to be a tree, got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                )
            }
            None => Vec::new(),
        };
        let prefix_entry = checkpoint_items
            .iter()
            .find(|item| item.name == prefix)
            .cloned();
        let mut prefix_items = match prefix_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => {
                self.load_tree_for_attempt(&entry.id, deadline).await?
            }
            Some(entry) => {
                bail!(
                    "traces tree corruption: 'checkpoint/{prefix}' entry expected to be a tree, got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                )
            }
            None => Vec::new(),
        };

        prefix_items.retain(|item| item.name != rest);
        prefix_items.push(TreeItem::new(
            TreeItemMode::Tree,
            inner_tree,
            rest.to_string(),
        ));
        prefix_items.sort_by(|a, b| a.name.cmp(&b.name));
        let prefix_tree = self
            .write_tree_indexed_for_attempt(&prefix_items, writer_fence, deadline, newly_written)
            .await?;

        checkpoint_items.retain(|item| item.name != prefix);
        checkpoint_items.push(TreeItem::new(
            TreeItemMode::Tree,
            prefix_tree,
            prefix.to_string(),
        ));
        checkpoint_items.sort_by(|a, b| a.name.cmp(&b.name));
        let checkpoint_tree = self
            .write_tree_indexed_for_attempt(
                &checkpoint_items,
                writer_fence,
                deadline,
                newly_written,
            )
            .await?;

        root_items.retain(|item| item.name != "checkpoint");
        root_items.push(TreeItem::new(
            TreeItemMode::Tree,
            checkpoint_tree,
            "checkpoint".to_string(),
        ));
        root_items.sort_by(|a, b| a.name.cmp(&b.name));
        self.write_tree_indexed_for_attempt(&root_items, writer_fence, deadline, newly_written)
            .await
    }

    #[cfg(test)]
    pub fn get_storage(&self) -> Arc<dyn Storage + Send + Sync> {
        self.storage.clone()
    }
}

/// Inputs to [`HistoryManager::append_checkpoint_commit`].
///
/// All byte slices live for the duration of the call; the function does not
/// retain references after returning.
#[derive(Debug)]
pub struct CheckpointCommitParams<'a> {
    /// UUIDv4 of the checkpoint, used both as the row primary key and as
    /// the leaf path under `checkpoint/<id[:2]>/<id[2:]>/...`.
    pub checkpoint_id: &'a str,
    /// `agent_session.session_id` this checkpoint belongs to.
    pub session_id: &'a str,
    /// Random generation returned by the marker registration that authorized
    /// this specific writer. The append entry point compares it before any
    /// object is created, so a stalled writer cannot adopt a replacement
    /// marker registered under the same session/checkpoint key.
    pub marker_generation: &'a str,
    /// `agent_session.agent_kind` (snake_case form, e.g. `claude_code`).
    /// Also the file-name stem of `transcript/<agent_kind>.jsonl` — E4-libra
    /// pins the snake_case db tag here, never the CLI slug (`claude-code`).
    pub agent_kind: &'a str,
    /// User-branch HEAD oid at the moment the checkpoint was taken.
    pub parent_commit: Option<&'a str>,
    /// Scope category: temporary, committed, or subagent.
    pub scope: CheckpointScope,
    /// Optional tool-use id when the checkpoint was triggered by a tool call.
    pub tool_use_id: Option<&'a str>,
    /// Pre-serialised metadata JSON to land at `metadata.json`. Typed as
    /// [`RedactedBytes`] (AG-19 / G4) so the traces write path can only ever
    /// receive bytes that passed through the redaction type.
    pub metadata_json: &'a RedactedBytes,
    /// Already-redacted transcript bytes. Typed as [`RedactedBytes`]
    /// (not `&[u8]`) so the traces write path can only ever receive
    /// bytes that passed through the redaction type — entire.md §8.1 /
    /// §13 P0: every transcript blob written to `traces` must go
    /// through `RedactedBytes`.
    pub transcript_redacted: &'a RedactedBytes,
    /// E3-canonical lifecycle JSONL bytes to land at
    /// `events/lifecycle.jsonl` — one already-redacted canonical event per
    /// line (see `hooks::lifecycle::lifecycle_events_to_canonical_jsonl`).
    /// Today the runtime passes the single triggering event; multi-event
    /// batches are just additional lines. Typed as [`RedactedBytes`]
    /// (AG-19 / G4) so no `&[u8]` can reach the checkpoint sink.
    pub lifecycle_events_jsonl: &'a RedactedBytes,
    /// The aggregated redaction-report JSON (same document that lands in
    /// `agent_session.redaction_report` / metadata.json) to land at
    /// `redaction_report.json`. Rule-hit statistics only — never raw text.
    /// Typed as [`RedactedBytes`] (AG-19 / G4) to keep the whole checkpoint
    /// tree behind the redaction type.
    pub redaction_report_json: &'a RedactedBytes,
    /// plan-20260713 DR-05c-0 (ADR-DR-10): extra SQL applied INSIDE the
    /// winning ref-CAS transaction — catalog row, coverage revision inserts
    /// and claim advances commit atomically with the ref update, or the
    /// whole transaction (ref included) rolls back. `None` keeps the legacy
    /// behavior (catalog inserted separately after the CAS).
    pub txn_extra: Option<&'a dyn TracesTxnExtra>,
    /// Absolute command deadline for historical imports. Live/export writers
    /// pass `None`; import object construction and CAS are cancelled when the
    /// deadline is reached.
    pub deadline: Option<Instant>,
}

/// Per-attempt commit identifiers handed to [`TracesTxnExtra::apply`] — the
/// commit hash and root tree change on every CAS rebuild, so the extra must
/// receive them at apply time rather than capture them up front.
#[derive(Debug, Clone)]
pub struct TracesCommitCtx {
    pub commit_hash: String,
    pub tree_oid: String,
    pub metadata_blob_oid: String,
}

/// A catalog row reconstructed from a `refs/libra/traces` checkpoint commit
/// (plan-20260713 DR-05c-0): the SHARED classification boundary used by
/// doctor's class-2 repair and by claim recovery, so both apply the same
/// fail-closed rules instead of duplicating them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuiltCatalogRow {
    Committed {
        checkpoint_id: String,
        session_id: String,
        parent_commit: Option<String>,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
        created_at: i64,
    },
    Subagent {
        checkpoint_id: String,
        session_id: String,
        parent_commit: Option<String>,
        parent_checkpoint_id: Option<String>,
        subagent_session_id: Option<String>,
        tool_use_id: Option<String>,
        description: Option<String>,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
        created_at: i64,
    },
}

/// Inputs for [`rebuild_catalog_row_from_traces_ref`] — the fields both a
/// checkpoint commit's `metadata.json` and its ref position provide.
#[derive(Debug, Clone, Default)]
pub struct RebuildCatalogRowInputs {
    pub scope: String,
    pub checkpoint_id: String,
    pub session_id: String,
    pub parent_commit: Option<String>,
    pub parent_checkpoint_id: Option<String>,
    pub subagent_session_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub description: Option<String>,
    pub tree_oid: String,
    pub metadata_blob_oid: String,
    pub traces_commit: String,
    pub created_at: i64,
}

/// Classify + assemble a rebuildable catalog row from traces-ref evidence.
/// Fail-closed: any scope other than `committed` / `subagent` is an error —
/// the caller must route it to manual review, never guess a row shape.
pub fn rebuild_catalog_row_from_traces_ref(
    inputs: RebuildCatalogRowInputs,
) -> Result<RebuiltCatalogRow> {
    match inputs.scope.as_str() {
        "committed" => Ok(RebuiltCatalogRow::Committed {
            checkpoint_id: inputs.checkpoint_id,
            session_id: inputs.session_id,
            parent_commit: inputs.parent_commit,
            tree_oid: inputs.tree_oid,
            metadata_blob_oid: inputs.metadata_blob_oid,
            traces_commit: inputs.traces_commit,
            created_at: inputs.created_at,
        }),
        "subagent" => Ok(RebuiltCatalogRow::Subagent {
            checkpoint_id: inputs.checkpoint_id,
            session_id: inputs.session_id,
            parent_commit: inputs.parent_commit,
            parent_checkpoint_id: inputs.parent_checkpoint_id,
            subagent_session_id: inputs.subagent_session_id,
            tool_use_id: inputs.tool_use_id,
            description: inputs.description,
            tree_oid: inputs.tree_oid,
            metadata_blob_oid: inputs.metadata_blob_oid,
            traces_commit: inputs.traces_commit,
            created_at: inputs.created_at,
        }),
        other => Err(anyhow!(
            "checkpoint scope '{other}' is not auto-rebuildable (fail-closed; manual review)"
        )),
    }
}

/// Transactional companion writes for a traces ref update (ADR-DR-10).
///
/// `apply` runs inside the SAME SQLite transaction as the successful ref
/// CAS, after the ref row write and before COMMIT. Returning an error rolls
/// the entire transaction back — the ref does not move, and the caller's
/// checkpoint write fails closed.
#[async_trait::async_trait]
pub trait TracesTxnExtra: Send + Sync {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()>;
}

impl std::fmt::Debug for dyn TracesTxnExtra + '_ {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TracesTxnExtra")
    }
}

/// Scope tag stamped on each checkpoint, mirroring the
/// `agent_checkpoint.scope` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointScope {
    Temporary,
    Committed,
    Subagent,
}

impl CheckpointScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Temporary => "temporary",
            Self::Committed => "committed",
            Self::Subagent => "subagent",
        }
    }
}

/// Output from [`HistoryManager::append_checkpoint_commit`]; what the caller
/// stores in `agent_checkpoint`.
///
/// Naming discipline (AG-20): `commit_hash` is the freshly-written commit on
/// `refs/libra/traces`; the DB column it lands in is
/// `agent_checkpoint.traces_commit`. Keep the two names distinct — the Rust
/// side never calls it `traces_commit` and the SQL side never `commit_hash`.
#[derive(Debug, Clone)]
pub struct CheckpointCommit {
    pub commit_hash: ObjectHash,
    pub tree_oid: ObjectHash,
    pub metadata_blob_oid: ObjectHash,
    /// Exact marker generation consumed by this append. Callers use it when
    /// retiring the marker after their catalog write completes.
    pub marker_generation: String,
    /// Number of head-conflict retries the ref CAS loop needed (0 = first
    /// attempt won). Recorded on the `agent.checkpoint.write` span.
    pub cas_retries: u64,
    /// Objects written/enqueued for this checkpoint (blobs + trees +
    /// commit, counted across CAS attempts). Recorded on the
    /// `agent.checkpoint.write` span.
    pub object_count: u64,
}

/// Outcome of [`HistoryManager::erase_session_local`] — the three-face
/// local erasure result for one session (AG-24a).
#[derive(Debug, Clone)]
pub struct SessionEraseOutcome {
    /// Whether an `agent_session` row was deleted.
    pub session_deleted: bool,
    /// Checkpoints removed from the catalog + ref.
    pub removed_checkpoints: u64,
    /// Whether `refs/libra/traces` was rewritten.
    pub ref_rewritten: bool,
    /// `object_index` rows dropped for now-unreachable OIDs.
    pub deleted_object_index_rows: u64,
}

/// Result of pruning checkpoint commits from `refs/libra/traces`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointPruneOutcome {
    pub removed_checkpoints: u64,
    pub rewritten_checkpoints: usize,
    pub ref_rewritten: bool,
    /// Which AG-20 window-guard path the prune took. `"noop"` when there
    /// was nothing to prune (guards skipped),
    /// `"markers_and_catalog_verified"` when both the live in-flight
    /// marker check and the ref-vs-catalog comparison ran and passed.
    /// Recorded on the `agent.clean.prune` span.
    pub window_guard: &'static str,
    /// `object_index` rows deleted for OIDs the prune made unreachable
    /// (conservative: only OIDs exclusively referenced by the removed
    /// checkpoints). Recorded on the `agent.clean.prune` span as
    /// `deleted_objects`.
    pub deleted_object_index_rows: u64,
    /// Import job identities physically deleted after their final coverage
    /// claim disappeared in this prune transaction.
    pub deleted_import_identities: u64,
}

/// Fail-closed refusals raised by [`HistoryManager::prune_checkpoint_commits`]
/// before it rewrites `refs/libra/traces` (AG-20 window A/B closure,
/// `agent.md` write-sequence matrix).
///
/// Callers can `downcast_ref` through the `anyhow` chain to distinguish a
/// deterministic guard refusal (retry later / run doctor) from a real
/// storage failure.
#[derive(Debug, thiserror::Error)]
#[error(
    "rejected checkpoint cleanup is deferred because {reason}; candidate ownership remains durable — inspect with `libra agent doctor`, run `libra agent doctor --repair` for safe repairs, and retry"
)]
struct RejectedCheckpointCleanupDeferred {
    reason: String,
}

/// M5 reservation→marker guard kept crate-private so adding this refusal does
/// not add a variant to the exhaustively matchable public guard enum.
#[derive(Debug, thiserror::Error)]
#[error(
    "refusing to prune traces checkpoints: a subagent content write is reserved \
     (session '{session_id}', attempt '{attempt_id}', lease expires at \
     {lease_expires_at}); retry once the writer finishes or the lease expires"
)]
pub(crate) struct SubagentContentReservationPruneGuard {
    session_id: String,
    attempt_id: String,
    lease_expires_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum CheckpointPruneGuardError {
    /// Window A/B: a writer's in-flight marker is still live. The prune is
    /// a whole-chain rewrite of the shared ref and catalog, so ANY live
    /// marker — regardless of which session it belongs to — blocks the
    /// prune (safest granularity: a concurrent writer between stages
    /// (a)–(d) may hold objects/commits that neither the ref nor the
    /// catalog reaches yet, and its parent head may be about to be
    /// rewritten away). Markers expire after
    /// [`AGENT_TRACES_INFLIGHT_TTL_MS`], so the refusal is temporary.
    #[error(
        "refusing to prune traces checkpoints: a checkpoint write is in flight \
         (session '{session_id}', attempt '{attempt_id}'); in-flight markers \
         expire {ttl_ms} ms after the write starts — retry once the writer \
         finishes or the marker expires"
    )]
    LiveWriterMarker {
        session_id: String,
        attempt_id: String,
        ttl_ms: i64,
    },
    /// Window B residue: `refs/libra/traces` reaches commits that have no
    /// `agent_checkpoint` catalog row. The prune rebuild is catalog-driven,
    /// so rewriting now would silently drop those legal checkpoints;
    /// repairing the catalog is doctor's job.
    #[error(
        "refusing to prune traces checkpoints: refs/libra/traces reaches \
         {orphan_count} commit(s) with no agent_checkpoint catalog row \
         (first: {first_commit}); run `libra agent doctor --repair` to \
         backfill the catalog, then retry"
    )]
    RefCatalogOrphans {
        orphan_count: usize,
        first_commit: String,
    },
}

// ---------------------------------------------------------------------------
// AG-20 E4-libra checkpoint layout: chunking, content hash, manifest
// ---------------------------------------------------------------------------

/// E5 transcript chunking threshold: transcripts strictly larger than this
/// split into line-boundary-safe `.jsonl.%03d` parts. Frozen wire value —
/// matches the entire.io archive envelope (`50 * 1024 * 1024`).
pub const TRANSCRIPT_CHUNK_THRESHOLD_BYTES: usize = 50 * 1024 * 1024;

/// File name of the canonical lifecycle event stream inside a checkpoint
/// tree (`events/lifecycle.jsonl`, E4-libra).
pub const CHECKPOINT_LIFECYCLE_EVENTS_FILE: &str = "lifecycle.jsonl";

/// `metadata.json` external schema version written by the AG-20 writer.
///
/// v1 (pre-AG-20): `schema_version`, `checkpoint_id`, `session_id`,
/// `agent_kind`, `scope`, `provider_session_id`, `working_dir`,
/// `redaction_report`, `created_at`.
/// v2 (AG-20): all v1 fields (strictly additive — v1 readers keep working)
/// plus `model` (from the triggering lifecycle event when present, else
/// `"unknown"`, mirroring the E4-entire missing-`model` tolerance).
pub const CHECKPOINT_METADATA_SCHEMA_VERSION: u32 = 2;

/// `manifest.json` external schema version (first version).
pub const CHECKPOINT_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Ordered coverage roles for `content_hash.txt` — the sha256 runs over the
/// concatenation of these manifest entries' bytes in exactly this order.
/// `manifest.json` (written after the hash) and `content_hash.txt` itself
/// are excluded by construction. The transcript role contributes its
/// logical byte stream (chunks concatenated in part order), so the hash is
/// invariant under re-chunking. Mirrored in the manifest's
/// `content_hash.coverage` array so every checkpoint self-describes the
/// definition.
pub const CHECKPOINT_CONTENT_HASH_COVERAGE: [&str; 4] = [
    "metadata",
    "lifecycle_events",
    "transcript",
    "redaction_report",
];

/// Resolve the effective E5 chunking threshold.
///
/// `LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD` (bytes, test-only — mirrors the
/// `LIBRA_TEST_*` convention) overrides the frozen 50 MiB constant so tests
/// can exercise the chunking path without allocating 50 MiB. Invalid or
/// zero values fall back to the constant rather than erroring: a stray env
/// var must never turn the writer into a per-byte chunker or a hard error.
pub fn transcript_chunk_threshold() -> usize {
    std::env::var("LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&threshold| threshold > 0)
        .unwrap_or(TRANSCRIPT_CHUNK_THRESHOLD_BYTES)
}

/// Split JSONL bytes into chunks of at most `max_size` bytes, cutting only
/// at line boundaries (`\n` stays with the line it terminates). E5 contract:
/// a single line whose bytes (including its terminator) exceed `max_size`
/// is a **hard error** — silently splitting mid-line would corrupt the JSONL
/// framing for every downstream reader.
///
/// Returns borrowed sub-slices (no copy); an empty input yields one empty
/// chunk so callers always have at least one part to name.
pub fn chunk_transcript_line_safe(content: &[u8], max_size: usize) -> Result<Vec<&[u8]>> {
    if max_size == 0 {
        return Err(anyhow!("transcript chunk size must be greater than zero"));
    }
    if content.len() <= max_size {
        return Ok(vec![content]);
    }

    let mut chunks = Vec::new();
    let mut chunk_start = 0usize;
    let mut line_start = 0usize;
    while line_start < content.len() {
        let line_end = match content[line_start..].iter().position(|&b| b == b'\n') {
            Some(offset) => line_start + offset + 1, // keep the terminator
            None => content.len(),                   // final unterminated line
        };
        let line_len = line_end - line_start;
        if line_len > max_size {
            return Err(anyhow!(
                "transcript line of {line_len} bytes exceeds the {max_size}-byte chunk \
                 threshold; refusing to split mid-line (E5). Raise the threshold or fix \
                 the producer emitting the oversized line"
            ));
        }
        if line_end - chunk_start > max_size {
            chunks.push(&content[chunk_start..line_start]);
            chunk_start = line_start;
        }
        line_start = line_end;
    }
    if chunk_start < content.len() {
        chunks.push(&content[chunk_start..]);
    }
    Ok(chunks)
}

/// Reassemble E5 chunks back into the logical transcript byte stream.
/// Inverse of [`chunk_transcript_line_safe`]: parts must be supplied in
/// manifest-declared order.
pub fn reassemble_transcript_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
    let total = chunks.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(total);
    for chunk in chunks {
        out.extend_from_slice(chunk);
    }
    out
}

/// Compute `content_hash.txt`'s value: `sha256:` + 64 lowercase hex over the
/// concatenation of `sections` in the order given (callers pass the
/// [`CHECKPOINT_CONTENT_HASH_COVERAGE`] roles' bytes). No trailing newline —
/// the string IS the file content, mirroring the E4-entire format.
pub fn checkpoint_content_hash(sections: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for section in sections {
        hasher.update(section);
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Parse a `content_hash.txt` payload into its 64-lowercase-hex digest.
///
/// Writer output always carries the `sha256:` prefix; the reader ALSO
/// accepts legacy bare hex (E4-entire compatibility table) and surrounding
/// whitespace/newline slack. Returns `None` for anything else, so callers
/// fail closed on garbage.
pub fn parse_content_hash(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let hex = trimmed.strip_prefix("sha256:").unwrap_or(trimmed);
    let normalized = hex.to_ascii_lowercase();
    (normalized.len() == 64 && normalized.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(normalized)
}

/// One written transcript part (single file or E5 chunk): tree-entry name,
/// blob OID, byte length.
#[derive(Debug, Clone)]
struct TranscriptPartRef {
    name: String,
    oid: ObjectHash,
    byte_len: usize,
}

/// (OID, byte length) pair for one single-blob manifest entry.
#[derive(Debug, Clone, Copy)]
struct ManifestBlobRef {
    oid: ObjectHash,
    byte_len: usize,
}

impl ManifestBlobRef {
    fn new(oid: ObjectHash, byte_len: usize) -> Self {
        Self { oid, byte_len }
    }
}

fn manifest_entry(
    path: &str,
    blob: ManifestBlobRef,
    media_type: &str,
    redaction: &str,
    schema_version: u32,
) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "oid": blob.oid.to_string(),
        "byte_len": blob.byte_len,
        "media_type": media_type,
        "compression": "none",
        "redaction": redaction,
        "schema_version": schema_version,
    })
}

/// Serialise `manifest.json` for one E4-libra checkpoint: logical role →
/// `{path, oid, byte_len, media_type, compression, redaction,
/// schema_version}`. Paths are manifest-relative (relative to the
/// checkpoint's inner tree). A chunked transcript omits the single-blob
/// `oid` and instead declares ordered `parts` (E5: doctor/export/transcript
/// readers resolve chunks ONLY through this list, never by globbing tree
/// names). `redaction` is `"redacted"` for entries carrying scrubbed
/// content, `"report"` for the rule-hit report, `"none"` for derived
/// artifacts with no user content (content_hash).
#[allow(clippy::too_many_arguments)]
fn build_checkpoint_manifest_json(
    checkpoint_id: &str,
    transcript_file_name: &str,
    metadata: ManifestBlobRef,
    lifecycle_events: ManifestBlobRef,
    transcript_parts: &[TranscriptPartRef],
    transcript_total_len: usize,
    redaction_report: ManifestBlobRef,
    content_hash: ManifestBlobRef,
) -> Result<Vec<u8>> {
    let transcript_logical_path = format!("transcript/{transcript_file_name}");
    let mut transcript_entry = serde_json::json!({
        "path": transcript_logical_path,
        "byte_len": transcript_total_len,
        "media_type": "application/x-ndjson",
        "compression": "none",
        "redaction": "redacted",
        "schema_version": 1,
    });
    // INVARIANT: transcript_entry is constructed as a JSON object above.
    let transcript_obj = transcript_entry
        .as_object_mut()
        .expect("transcript manifest entry is an object");
    if transcript_parts.len() == 1 {
        transcript_obj.insert(
            "oid".to_string(),
            serde_json::json!(transcript_parts[0].oid.to_string()),
        );
    } else {
        transcript_obj.insert("chunked".to_string(), serde_json::json!(true));
        transcript_obj.insert(
            "parts".to_string(),
            serde_json::json!(
                transcript_parts
                    .iter()
                    .map(|part| {
                        serde_json::json!({
                            "path": format!("transcript/{}", part.name),
                            "oid": part.oid.to_string(),
                            "byte_len": part.byte_len,
                        })
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }

    let manifest = serde_json::json!({
        "schema_version": CHECKPOINT_MANIFEST_SCHEMA_VERSION,
        "checkpoint_id": checkpoint_id,
        "content_hash": {
            "algorithm": "sha256",
            "path": "content_hash.txt",
            // Self-describing hash definition: sha256 over the
            // concatenation of these roles' bytes in THIS order (the
            // transcript contributes its logical, reassembled stream).
            "coverage": CHECKPOINT_CONTENT_HASH_COVERAGE,
        },
        "entries": {
            "metadata": manifest_entry(
                "metadata.json",
                metadata,
                "application/json",
                "redacted",
                CHECKPOINT_METADATA_SCHEMA_VERSION,
            ),
            "lifecycle_events": manifest_entry(
                "events/lifecycle.jsonl",
                lifecycle_events,
                "application/x-ndjson",
                "redacted",
                1,
            ),
            "transcript": transcript_entry,
            "redaction_report": manifest_entry(
                "redaction_report.json",
                redaction_report,
                "application/json",
                "report",
                1,
            ),
            "content_hash": manifest_entry(
                "content_hash.txt",
                content_hash,
                "text/plain",
                "none",
                1,
            ),
        },
    });
    serde_json::to_vec_pretty(&manifest).context("failed to serialize checkpoint manifest.json")
}

// ---------------------------------------------------------------------------
// AG-20 window A/B closure: traces writer in-flight markers
// ---------------------------------------------------------------------------

/// TTL for traces-writer in-flight markers: markers older than this are
/// considered stale leftovers of a crashed writer and stop protecting
/// their OIDs. Ten minutes comfortably bounds a checkpoint write (which is
/// local-only I/O) while keeping crashed-writer garbage collectable.
pub const AGENT_TRACES_INFLIGHT_TTL_MS: i64 = 10 * 60 * 1000;

/// One in-flight traces-writer marker (window A/B guard, AG-20).
///
/// Stored as JSON in `metadata_kv` under scope
/// [`crate::internal::metadata::MetadataScope::AgentTracesInflight`] with
/// `target` = the Libra agent session id and `key` = the write attempt's
/// checkpoint UUID. The writer creates the marker BEFORE stage (a) (blob
/// writes) and clears it AFTER stage (d) (`agent_checkpoint` INSERT), so a
/// live marker tells the prune side "objects for this attempt may exist
/// that neither the ref nor the catalog reaches yet — do not collect".
///
/// Marker registration is fail-closed and precedes object construction. A
/// cleanup-pending marker remains durable beyond its normal TTL until its
/// candidate OIDs have been checked against repository roots and ownership is
/// retired for later repository GC.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TracesInflightMarker {
    /// Marker JSON schema version (additive evolution only).
    pub schema_version: u32,
    /// Libra agent session id (`agent_session.session_id`).
    pub session_id: String,
    /// Attempt UUID — the checkpoint id this write will (try to) catalog.
    pub attempt_id: String,
    /// Unpredictable writer generation. The `(session_id, attempt_id)` key is
    /// stable across takeover, so every marker mutation and final CAS must
    /// additionally compare this token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    /// Unix epoch milliseconds when the writer created the marker.
    pub started_at_ms: i64,
    /// Time-to-live in milliseconds; `started_at_ms + ttl_ms <= now` means
    /// expired.
    pub ttl_ms: i64,
    /// Traces commit hash, filled in (best-effort) once the ref CAS
    /// succeeded — lets prune protect the exact commit during window B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// OIDs written for this attempt (tree/metadata-blob level), filled in
    /// best-effort after stage (b) — lets prune protect loose objects
    /// during window A.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oids: Vec<String>,
    /// OIDs this attempt is proven to have published with `create_new`
    /// semantics. Destructive recovery considers only this set. `oids`
    /// contains unresolved preclaims and is deliberately leak-safe after a
    /// crash between publish and ownership finalization.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_oids: Vec<String>,
    /// A rejected append owns the listed newly-created OIDs until a serialized
    /// reachability pass drains them. Pending cleanup ignores the ordinary
    /// marker TTL and blocks erasure from reporting success.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cleanup_pending: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl TracesInflightMarker {
    /// A fresh marker for a write attempt starting now.
    pub fn new(session_id: &str, attempt_id: &str, started_at_ms: i64) -> Self {
        Self {
            schema_version: 3,
            session_id: session_id.to_string(),
            attempt_id: attempt_id.to_string(),
            generation: Some(uuid::Uuid::new_v4().to_string()),
            started_at_ms,
            ttl_ms: AGENT_TRACES_INFLIGHT_TTL_MS,
            commit: None,
            oids: Vec::new(),
            created_oids: Vec::new(),
            cleanup_pending: false,
        }
    }

    /// Whether the marker is still live at `now_ms`.
    ///
    /// BOUNDED (W2 §C.4.3): a deterministic absolute deadline from the
    /// persisted fields with `ttl_ms` capped at
    /// [`TRACES_INFLIGHT_MAX_LIVE_MS`]. Future-dated rows are handled by
    /// [`Self::time_fields_trustworthy`] (the listing fails closed on them
    /// and doctor retires them) — this predicate never re-anchors to the
    /// reading clock. Every liveness consumer inherits these definitions.
    pub fn is_live(&self, now_ms: i64) -> bool {
        // DETERMINISTIC absolute deadline from the PERSISTED fields only —
        // never re-anchored to the reading clock. TTL is capped so nothing
        // counts as live more than 24h past its recorded start. Rows whose
        // start lies beyond clock-skew tolerance in the FUTURE never reach
        // this predicate through the listing: `time_fields_trustworthy`
        // fails the listing CLOSED for them (they are corrupt, and silently
        // dropping them would strip a possibly-writing session's only
        // protection).
        self.started_at_ms
            .saturating_add(self.ttl_ms.clamp(0, TRACES_INFLIGHT_MAX_LIVE_MS))
            > now_ms
    }

    /// Whether the persisted time fields are plausible at `now_ms`: a start
    /// more than [`TRACES_INFLIGHT_FUTURE_SKEW_MS`] in the future cannot
    /// come from a healthy writer — the row is corrupt and every
    /// destructive consumer must stop (fail closed) rather than guess.
    pub fn time_fields_trustworthy(&self, now_ms: i64) -> bool {
        self.started_at_ms <= now_ms.saturating_add(TRACES_INFLIGHT_FUTURE_SKEW_MS)
    }
}

/// Upper bound on how long ANY ordinary in-flight marker may count as live,
/// regardless of what its persisted `ttl_ms` claims (fail-safe clamp; the
/// ordinary writer TTL is 10 minutes, so 24h is generous headroom).
pub const TRACES_INFLIGHT_MAX_LIVE_MS: i64 = 24 * 60 * 60 * 1000;

/// Clock-skew tolerance for `started_at_ms` (a healthy writer stamps "now";
/// anything further in the future is a corrupt row, not a long-lived one).
pub const TRACES_INFLIGHT_FUTURE_SKEW_MS: i64 = 5 * 60 * 1000;

fn validate_traces_inflight_marker(
    marker: &TracesInflightMarker,
    expected_session_id: &str,
    expected_attempt_id: &str,
) -> Result<()> {
    if marker.session_id != expected_session_id || marker.attempt_id != expected_attempt_id {
        bail!(
            "traces in-flight marker identity does not match its metadata row (row {expected_session_id}/{expected_attempt_id}, value {}/{}); inspect it with `libra agent doctor` before retrying",
            marker.session_id,
            marker.attempt_id
        );
    }
    if marker.schema_version >= 3 {
        let generation = marker.generation.as_deref().ok_or_else(|| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} schema {} has no writer generation; inspect it with `libra agent doctor` before retrying",
                marker.schema_version
            )
        })?;
        uuid::Uuid::parse_str(generation).map_err(|error| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} has invalid writer generation '{generation}': {error}; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    for oid in &marker.oids {
        ObjectHash::from_str(oid).map_err(|error| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} contains invalid object id '{oid}': {error}; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    for oid in &marker.created_oids {
        ObjectHash::from_str(oid).map_err(|error| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} contains invalid created object id '{oid}': {error}; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    if let Some(commit) = marker.commit.as_deref() {
        ObjectHash::from_str(commit).map_err(|error| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} contains invalid commit id '{commit}': {error}; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    Ok(())
}

pub(crate) fn decode_and_validate_traces_inflight_marker(
    value: &str,
    expected_session_id: &str,
    expected_attempt_id: &str,
) -> Result<TracesInflightMarker> {
    let marker = serde_json::from_str::<TracesInflightMarker>(value).with_context(|| {
        format!(
            "decode traces in-flight marker for session {expected_session_id} attempt {expected_attempt_id}; inspect it with `libra agent doctor` before retrying"
        )
    })?;
    validate_traces_inflight_marker(&marker, expected_session_id, expected_attempt_id)?;
    Ok(marker)
}

/// Fence a stale writer marker without losing crash-recovery ownership.
/// Empty attempts can be removed immediately; attempts that pre-registered
/// any OID are promoted to a durable cleanup job for the normal reachability
/// drain.
pub async fn retire_stale_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    attempt_id: &str,
) -> Result<()> {
    let entry = crate::internal::metadata::MetadataKv::get_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
        session_id,
        attempt_id,
    )
    .await
    .context("load stale traces writer marker")?;
    let Some(entry) = entry else {
        return Ok(());
    };
    let mut marker =
        decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key)?;
    if marker.created_oids.is_empty() {
        clear_traces_inflight_marker(conn, session_id, attempt_id).await?;
    } else {
        marker.schema_version = marker.schema_version.max(2);
        marker.cleanup_pending = true;
        write_traces_inflight_marker(conn, &marker)
            .await
            .context("promote stale traces marker to durable cleanup")?;
    }
    Ok(())
}

/// Expected coverage fence for fail-closed writer registration. Empty plans
/// are valid for metadata/subagent checkpoints, but every claimed live/export
/// turn must still be owned before any object is built.
pub struct TracesCoverageFence<'a> {
    pub logical_turn_key: &'a str,
    pub owner: &'a str,
    pub fence_token: i64,
    pub reservation_state: &'a str,
}

/// Establish one writer attempt under the same SQLite writer lock used by
/// erasure. The session/tombstone barrier and any coverage fences are checked
/// in the marker transaction; a failed marker write aborts the checkpoint
/// before loose objects or object-index tasks can exist.
pub async fn register_traces_write_attempt(
    conn: &DatabaseConnection,
    marker: &TracesInflightMarker,
    coverage_fences: &[TracesCoverageFence<'_>],
) -> Result<()> {
    let txn = conn
        .begin()
        .await
        .context("begin traces writer-attempt registration")?;
    let writable = txn
        .query_one(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 AS writable
             FROM agent_session s
             WHERE s.session_id = ?
               AND NOT EXISTS (
                 SELECT 1 FROM agent_import_tombstone t
                 WHERE t.agent_kind = s.agent_kind
                   AND t.provider_session_id = s.provider_session_id
               )",
            [marker.session_id.clone().into()],
        ))
        .await
        .context("verify traces writer session/tombstone barrier")?;
    if writable.is_none() {
        txn.rollback().await.ok();
        bail!("agent session was erased or is unavailable for checkpoint writing");
    }
    for fence in coverage_fences {
        let owned = txn
            .query_one(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 AS owned FROM agent_coverage_claim
                 WHERE session_id = ? AND logical_turn_key = ?
                   AND coverage_schema_version = 1 AND state = ?
                   AND owner = ? AND fence_token = ?",
                [
                    marker.session_id.clone().into(),
                    fence.logical_turn_key.into(),
                    fence.reservation_state.into(),
                    fence.owner.into(),
                    fence.fence_token.into(),
                ],
            ))
            .await
            .context("verify traces writer coverage fence")?;
        if owned.is_none() {
            txn.rollback().await.ok();
            bail!(
                "coverage fence for turn '{}' is no longer owned; checkpoint write aborted",
                fence.logical_turn_key
            );
        }
    }
    write_traces_inflight_marker(&txn, marker)
        .await
        .context("register traces writer marker")?;
    txn.commit()
        .await
        .context("commit traces writer-attempt registration")?;
    Ok(())
}

/// Upsert an in-flight marker row. Exported (not a stable API) so the
/// writer, the prune side, and integration tests share one implementation.
pub async fn write_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    marker: &TracesInflightMarker,
) -> Result<()> {
    validate_traces_inflight_marker(marker, &marker.session_id, &marker.attempt_id)?;
    let value =
        serde_json::to_string(marker).context("failed to serialize traces in-flight marker")?;
    crate::internal::metadata::MetadataKv::set_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
        &marker.session_id,
        &marker.attempt_id,
        &value,
        crate::internal::metadata::MetadataValueType::Text,
    )
    .await
    .context("failed to persist traces in-flight marker")?;
    Ok(())
}

/// Update an existing marker without allowing a stale writer to overwrite a
/// replacement generation registered under the same stable metadata key.
pub async fn update_traces_inflight_marker_if_generation<C: ConnectionTrait>(
    conn: &C,
    marker: &TracesInflightMarker,
    expected_generation: &str,
) -> Result<bool> {
    validate_traces_inflight_marker(marker, &marker.session_id, &marker.attempt_id)?;
    if marker.generation.as_deref() != Some(expected_generation) {
        bail!("refusing to update a traces marker with a mismatched writer generation");
    }
    let value =
        serde_json::to_string(marker).context("failed to serialize traces in-flight marker")?;
    let result = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE metadata_kv
             SET value = ?, value_type = 'text', updated_at = ?
             WHERE scope = 'agent_traces_inflight' AND target = ? AND key = ?
               AND json_extract(value, '$.generation') = ?",
            [
                value.into(),
                chrono::Utc::now().to_rfc3339().into(),
                marker.session_id.clone().into(),
                marker.attempt_id.clone().into(),
                expected_generation.into(),
            ],
        ))
        .await
        .context("failed to update exact traces writer generation")?;
    Ok(result.rows_affected() == 1)
}

/// Remove one in-flight marker (stage (d) complete, or prune-side cleanup
/// of an expired marker). Returns whether a row was removed.
pub async fn clear_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    attempt_id: &str,
) -> Result<bool> {
    crate::internal::metadata::MetadataKv::unset_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
        session_id,
        attempt_id,
    )
    .await
    .context("failed to clear traces in-flight marker")
}

/// Remove one marker only when the metadata row still belongs to the exact
/// writer generation that the caller registered.
pub async fn clear_traces_inflight_marker_if_generation<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
) -> Result<bool> {
    let result = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM metadata_kv
             WHERE scope = 'agent_traces_inflight' AND target = ? AND key = ?
               AND json_extract(value, '$.generation') = ?",
            [session_id.into(), attempt_id.into(), generation.into()],
        ))
        .await
        .context("failed to clear exact traces writer generation")?;
    Ok(result.rows_affected() == 1)
}

/// Clear a writer marker only while it still represents an ordinary live
/// attempt. Rejected-append cleanup upgrades the same row to
/// `cleanup_pending`; error finalizers must never erase that durable job.
pub async fn clear_non_cleanup_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
) -> Result<bool> {
    let result = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM metadata_kv
             WHERE scope = 'agent_traces_inflight' AND target = ? AND key = ?
               AND json_extract(value, '$.generation') = ?
               AND COALESCE(json_extract(value, '$.cleanup_pending'), 0) = 0",
            [session_id.into(), attempt_id.into(), generation.into()],
        ))
        .await
        .context("failed to clear exact ordinary traces writer generation")?;
    Ok(result.rows_affected() == 1)
}

/// List the LIVE (non-expired at `now_ms`) or durable cleanup-pending
/// in-flight markers across all sessions — the prune-side entry point: any
/// OID/commit named by a returned marker must be treated as reachable, and
/// (fail-closed) either kind of marker for a session should defer pruning
/// that session's chain. Cleanup ownership deliberately outlives the ordinary
/// writer TTL until doctor/GC retires it.
///
/// Malformed rows fail the listing closed: their OID ownership and TTL cannot
/// be trusted, so destructive prune/erasure must stop until
/// `libra agent doctor` reports the row for manual recovery.
pub async fn list_live_traces_inflight_markers<C: ConnectionTrait>(
    conn: &C,
    now_ms: i64,
) -> Result<Vec<TracesInflightMarker>> {
    let entries = crate::internal::metadata::MetadataKv::list_scope_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
    )
    .await
    .context("failed to list traces in-flight markers")?;
    let mut live = Vec::new();
    for entry in entries {
        match decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key) {
            Ok(marker) => {
                // A future-dated start beyond skew tolerance is a CORRUPT
                // row, not an expired one: silently filtering it would strip
                // a possibly-still-writing session's only protection, so the
                // listing fails CLOSED and destructive consumers stop.
                if !marker.time_fields_trustworthy(now_ms) {
                    bail!(
                        "traces in-flight marker for session {} attempt {} carries a \
                         future-dated started_at_ms ({} vs now {now_ms}) — the row is \
                         corrupt; inspect it with `libra agent doctor` before destructive \
                         maintenance",
                        marker.session_id,
                        marker.attempt_id,
                        marker.started_at_ms
                    );
                }
                if marker.cleanup_pending || marker.is_live(now_ms) {
                    live.push(marker);
                }
            }
            Err(err) => return Err(err),
        }
    }
    Ok(live)
}

async fn list_all_traces_inflight_markers<C: ConnectionTrait>(
    conn: &C,
) -> Result<Vec<TracesInflightMarker>> {
    let entries = crate::internal::metadata::MetadataKv::list_scope_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
    )
    .await
    .context("failed to list traces in-flight markers")?;
    entries
        .into_iter()
        .map(|entry| {
            decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key)
        })
        .collect()
}

/// Probe the checkpoint catalog by traces commit hash: returns the
/// `checkpoint_id` of the row whose `traces_commit` equals `commit_hash`,
/// if any. The writer calls this between ref CAS and catalog INSERT so a
/// crash-retry (or a doctor repair that already backfilled the row from
/// the ref) skips the INSERT instead of duplicating the commit's catalog
/// entry; doctor's window-B repair uses the same probe for idempotency.
pub async fn agent_checkpoint_id_for_traces_commit<C: ConnectionTrait>(
    conn: &C,
    commit_hash: &str,
) -> Result<Option<String>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id FROM agent_checkpoint WHERE traces_commit = ? LIMIT 1",
            [Value::from(commit_hash)],
        ))
        .await
        .context("failed to probe agent_checkpoint by traces_commit")?;
    row.map(|row| {
        row.try_get_by("checkpoint_id")
            .context("decode agent_checkpoint.checkpoint_id")
    })
    .transpose()
}

#[cfg(test)]
pub(crate) async fn checkpoint_leaf_durable_oids<C: ConnectionTrait>(
    conn: &C,
    repo_path: &Path,
    checkpoint_id: &str,
    traces_commit: &str,
    tree_oid: &str,
    metadata_blob_oid: &str,
) -> Result<HashSet<String>> {
    checkpoint_snapshot_durable_oids(
        conn,
        repo_path,
        &[CheckpointDurabilitySpec {
            checkpoint_id,
            traces_commit,
            tree_oid,
            metadata_blob_oid,
        }],
        None,
    )
    .await
}

#[derive(Clone, Copy)]
pub(crate) struct CheckpointDurabilitySpec<'a> {
    pub checkpoint_id: &'a str,
    pub traces_commit: &'a str,
    pub tree_oid: &'a str,
    pub metadata_blob_oid: &'a str,
}

const CHECKPOINT_DURABILITY_MAX_REACHABLE_COMMITS: usize = 100_000;
const CHECKPOINT_DURABILITY_MAX_OBJECTS: usize = 100_000;

fn insert_durable_oid_bounded(
    durable_oids: &mut HashSet<String>,
    oid: String,
    max_objects: usize,
) -> Result<()> {
    if !durable_oids.contains(&oid) && durable_oids.len() >= max_objects {
        bail!("checkpoint durability verification exceeded its aggregate object limit");
    }
    durable_oids.insert(oid);
    Ok(())
}

/// Verify a whole capture snapshot with one first-parent traversal. The older
/// per-leaf probe is intentionally retained as a one-item wrapper above for
/// unchanged-replay callers, while cloud sync uses this bounded batch form to
/// avoid quadratic commit reads.
pub(crate) async fn checkpoint_snapshot_durable_oids<C: ConnectionTrait>(
    conn: &C,
    repo_path: &Path,
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    #[cfg(test)]
    TEST_CHECKPOINT_SNAPSHOT_VERIFY_COUNT
        .try_with(|count| count.set(count.get().saturating_add(1)))
        .ok();
    if checkpoints.is_empty() {
        return Ok(HashSet::new());
    }
    let catalog_rows = conn
        .query_all(Statement::from_string(
            conn.get_database_backend(),
            "SELECT traces_commit FROM agent_checkpoint ORDER BY traces_commit".to_string(),
        ))
        .await
        .context("load checkpoint catalog while verifying traces durability")?;
    if catalog_rows.len() > CHECKPOINT_DURABILITY_MAX_REACHABLE_COMMITS {
        bail!("checkpoint catalog exceeds its durability verification limit");
    }
    let cataloged_commits = catalog_rows
        .into_iter()
        .map(|row| row.try_get_by::<String, _>("traces_commit"))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("decode checkpoint catalog traces commits")?;
    checkpoint_snapshot_durable_oids_with_catalog(
        conn,
        repo_path,
        checkpoints,
        cataloged_commits,
        deadline,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn checkpoint_rows_snapshot_durable_oids<C: ConnectionTrait>(
    conn: &C,
    repo_path: &Path,
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    if checkpoints.is_empty() {
        return Ok(HashSet::new());
    }
    let cataloged_commits = checkpoints
        .iter()
        .map(|checkpoint| checkpoint.traces_commit.to_string())
        .collect::<Vec<_>>();
    checkpoint_snapshot_durable_oids_with_catalog(
        conn,
        repo_path,
        checkpoints,
        cataloged_commits,
        deadline,
    )
    .await
}

async fn checkpoint_snapshot_durable_oids_with_catalog<C: ConnectionTrait>(
    conn: &C,
    repo_path: &Path,
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    cataloged_commits: Vec<String>,
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    let head_row = conn
        .query_one(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT `commit` FROM reference
             WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
            [crate::internal::branch::TRACES_BRANCH.into()],
        ))
        .await
        .context("resolve traces ref while verifying unchanged checkpoint")?;
    let head = head_row
        .map(|row| {
            row.try_get_by::<Option<String>, _>("commit")
                .context("decode refs/libra/traces head")
        })
        .transpose()?
        .flatten()
        .context("refs/libra/traces is missing while verifying unchanged checkpoint")?;
    checkpoint_rows_snapshot_durable_oids_from_head(
        repo_path,
        &head,
        &cataloged_commits,
        checkpoints,
        deadline,
    )
    .await
}

/// Verify a supplied checkpoint catalog against an explicitly fenced traces
/// head. Cloud restore uses the head stored in the completed capture manifest
/// instead of trusting independently uploaded generic reference metadata.
pub(crate) async fn checkpoint_rows_snapshot_durable_oids_from_head(
    repo_path: &Path,
    head: &str,
    cataloged_commits: &[String],
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    let parsed_head = ObjectHash::from_str(head)
        .map_err(|error| anyhow!("invalid fenced refs/libra/traces head: {error}"))?;

    #[cfg(not(test))]
    if let Some(deadline) = deadline {
        let checkpoints = checkpoints
            .iter()
            .map(|checkpoint| CheckpointDurabilityHelperSpec {
                checkpoint_id: checkpoint.checkpoint_id.to_string(),
                traces_commit: checkpoint.traces_commit.to_string(),
                tree_oid: checkpoint.tree_oid.to_string(),
                metadata_blob_oid: checkpoint.metadata_blob_oid.to_string(),
            })
            .collect();
        return match invoke_checkpoint_object_helper(
            repo_path,
            CheckpointObjectIoOperation::VerifySnapshot {
                head: head.to_string(),
                cataloged_commits: cataloged_commits.to_vec(),
                checkpoints,
            },
            deadline,
        )
        .await?
        {
            CheckpointObjectIoHelperResponse::Verified { oids } => Ok(oids.into_iter().collect()),
            CheckpointObjectIoHelperResponse::Error { message } => bail!("{message}"),
            CheckpointObjectIoHelperResponse::Read { .. }
            | CheckpointObjectIoHelperResponse::Written { .. } => {
                bail!("checkpoint object-I/O helper returned a non-verify response")
            }
        };
    }

    let cataloged_commits = parse_cataloged_traces_commits(cataloged_commits)?;
    checkpoint_snapshot_durable_oids_from_head(
        repo_path,
        parsed_head,
        &cataloged_commits,
        checkpoints,
        deadline,
    )
}

fn parse_cataloged_traces_commits(commits: &[String]) -> Result<HashSet<String>> {
    if commits.len() > CHECKPOINT_DURABILITY_MAX_REACHABLE_COMMITS {
        bail!("checkpoint catalog exceeds its durability verification limit");
    }
    Ok(commits.iter().cloned().collect())
}

fn checkpoint_snapshot_durable_oids_from_head(
    repo_path: &Path,
    head: ObjectHash,
    cataloged_commits: &HashSet<String>,
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    const MAX_CHECKPOINT_OBJECTS: usize = 16_384;
    const MAX_COMMIT_OR_TREE_BYTES: u64 = 4 * 1024 * 1024;
    const MAX_CHECKPOINT_BLOB_BYTES: u64 = 32 * 1024 * 1024;

    let mut expected = HashMap::new();
    for checkpoint in checkpoints {
        let commit = ObjectHash::from_str(checkpoint.traces_commit)
            .map_err(|error| anyhow!("invalid checkpoint traces commit: {error}"))?;
        let tree = ObjectHash::from_str(checkpoint.tree_oid)
            .map_err(|error| anyhow!("invalid checkpoint root tree: {error}"))?;
        if expected.insert(commit, tree).is_some() {
            bail!("multiple checkpoints share one traces commit");
        }
    }

    let mut next = Some(head);
    let mut visited = HashSet::new();
    let mut durable_oids = HashSet::new();
    let mut found_trees = HashMap::new();
    while let Some(oid) = next {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("checkpoint snapshot durability verification exceeded its deadline");
        }
        if !visited.insert(oid) {
            bail!("refs/libra/traces contains a first-parent cycle");
        }
        let oid_text = oid.to_string();
        insert_durable_oid_bounded(
            &mut durable_oids,
            oid_text.clone(),
            CHECKPOINT_DURABILITY_MAX_OBJECTS,
        )?;
        if visited.len() > CHECKPOINT_DURABILITY_MAX_REACHABLE_COMMITS {
            bail!("refs/libra/traces reachability probe exceeded its commit limit");
        }
        if !cataloged_commits.contains(&oid_text) {
            bail!(
                "refs/libra/traces reaches uncataloged commit {oid}; run `libra agent doctor --repair` before cloud sync or replay"
            );
        }
        let (object_type, data) =
            read_git_object_bounded_validated(repo_path, &oid, MAX_COMMIT_OR_TREE_BYTES)
                .with_context(|| format!("read traces commit {oid}"))?;
        if object_type != "commit" {
            bail!("traces ref points through non-commit object {oid}");
        }
        let commit = Commit::from_bytes(&data, oid)
            .map_err(|error| anyhow!("parse traces commit {oid}: {error}"))?;
        if let Some(expected_tree) = expected.get(&oid) {
            if commit.tree_id != *expected_tree {
                bail!("checkpoint commit root tree no longer matches its catalog row");
            }
            found_trees.insert(oid, commit.tree_id);
        }
        next = commit.parent_commit_ids.first().copied();
    }
    if found_trees.len() != expected.len() {
        bail!("one or more checkpoint commits are no longer reachable from refs/libra/traces");
    }

    for checkpoint in checkpoints {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("checkpoint snapshot durability verification exceeded its deadline");
        }
        let expected_tree = ObjectHash::from_str(checkpoint.tree_oid)
            .map_err(|error| anyhow!("invalid checkpoint root tree: {error}"))?;
        let expected_metadata = ObjectHash::from_str(checkpoint.metadata_blob_oid)
            .map_err(|error| anyhow!("invalid checkpoint metadata blob: {error}"))?;
        let mut leaf_oids = checkpoint_leaf_tree_durable_oids(
            repo_path,
            checkpoint.checkpoint_id,
            expected_tree,
            expected_metadata,
            deadline,
            MAX_CHECKPOINT_OBJECTS,
            MAX_COMMIT_OR_TREE_BYTES,
            MAX_CHECKPOINT_BLOB_BYTES,
        )?;
        for oid in leaf_oids.drain() {
            insert_durable_oid_bounded(&mut durable_oids, oid, CHECKPOINT_DURABILITY_MAX_OBJECTS)?;
        }
    }
    Ok(durable_oids)
}

#[allow(clippy::too_many_arguments)]
fn checkpoint_leaf_tree_durable_oids(
    repo_path: &Path,
    checkpoint_id: &str,
    expected_tree: ObjectHash,
    expected_metadata: ObjectHash,
    deadline: Option<Instant>,
    max_checkpoint_objects: usize,
    max_commit_or_tree_bytes: u64,
    max_checkpoint_blob_bytes: u64,
) -> Result<HashSet<String>> {
    let mut durable_oids = HashSet::new();
    let (root_type, root_bytes) =
        read_git_object_bounded_validated(repo_path, &expected_tree, max_commit_or_tree_bytes)
            .context("read checkpoint root tree")?;
    if root_type != "tree" {
        bail!("checkpoint root object is not a tree");
    }
    durable_oids.insert(expected_tree.to_string());
    let root = Tree::from_bytes(&root_bytes, expected_tree)
        .map_err(|error| anyhow!("parse checkpoint root tree: {error}"))?;
    let (prefix, rest) = checkpoint_tree_path(checkpoint_id)?;
    let (checkpoint_root_oid, checkpoint_root) =
        tree_child(repo_path, &root.tree_items, "checkpoint")?
            .context("checkpoint root tree has no checkpoint directory")?;
    durable_oids.insert(checkpoint_root_oid.to_string());
    let (prefix_root_oid, prefix_root) = tree_child(repo_path, &checkpoint_root, &prefix)?
        .context("checkpoint root tree has no checkpoint prefix directory")?;
    durable_oids.insert(prefix_root_oid.to_string());
    let leaf_oid = prefix_root
        .iter()
        .find(|item| item.name == rest && item.mode == TreeItemMode::Tree)
        .map(|item| item.id)
        .context("checkpoint root tree has no durable leaf for the checkpoint id")?;

    let mut stack = vec![leaf_oid];
    let mut object_count = 0_usize;
    let mut metadata_matches = false;
    let mut seen = HashSet::new();
    while let Some(oid) = stack.pop() {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("checkpoint snapshot durability verification exceeded its deadline");
        }
        if !seen.insert(oid) {
            continue;
        }
        durable_oids.insert(oid.to_string());
        object_count = object_count.saturating_add(1);
        if object_count > max_checkpoint_objects {
            bail!("checkpoint object verification exceeded its object limit");
        }
        let (object_type, data) =
            read_git_object_bounded_validated(repo_path, &oid, max_checkpoint_blob_bytes)
                .with_context(|| format!("read checkpoint object {oid}"))?;
        if object_type != "tree" {
            bail!("checkpoint directory object {oid} is not a tree");
        }
        let tree = Tree::from_bytes(&data, oid)
            .map_err(|error| anyhow!("parse checkpoint tree {oid}: {error}"))?;
        for item in tree.tree_items {
            if oid == leaf_oid && item.name == "metadata.json" {
                if item.mode == TreeItemMode::Tree || item.id != expected_metadata {
                    bail!("checkpoint metadata blob no longer matches its catalog row");
                }
                metadata_matches = true;
            }
            if item.mode == TreeItemMode::Tree {
                stack.push(item.id);
                continue;
            }
            let (object_type, _) =
                read_git_object_bounded_validated(repo_path, &item.id, max_checkpoint_blob_bytes)
                    .with_context(|| format!("read checkpoint blob {}", item.id))?;
            if object_type != "blob" {
                bail!("checkpoint leaf {} is not a blob", item.id);
            }
            durable_oids.insert(item.id.to_string());
            object_count = object_count.saturating_add(1);
            if object_count > max_checkpoint_objects {
                bail!("checkpoint object verification exceeded its object limit");
            }
        }
    }
    if !metadata_matches {
        bail!("checkpoint leaf has no matching metadata.json blob");
    }
    Ok(durable_oids)
}

fn tree_child(
    repo_path: &Path,
    items: &[TreeItem],
    name: &str,
) -> Result<Option<(ObjectHash, Vec<TreeItem>)>> {
    let Some(entry) = items
        .iter()
        .find(|item| item.name == name && item.mode == TreeItemMode::Tree)
    else {
        return Ok(None);
    };
    let (object_type, data) =
        read_git_object_bounded_validated(repo_path, &entry.id, 4 * 1024 * 1024)
            .with_context(|| format!("read checkpoint tree component {name}"))?;
    if object_type != "tree" {
        bail!("checkpoint tree component {name} is not a tree");
    }
    let tree = Tree::from_bytes(&data, entry.id)
        .map_err(|error| anyhow!("parse checkpoint tree component {name}: {error}"))?;
    Ok(Some((entry.id, tree.tree_items)))
}

#[derive(Debug, Clone)]
struct CheckpointHistoryRow {
    checkpoint_id: String,
    session_id: String,
    agent_kind: String,
    scope: String,
    parent_commit: Option<String>,
    /// `agent_checkpoint.traces_commit` — the commit this row currently
    /// points at on `refs/libra/traces`. Consumed by the prune-side
    /// ref-vs-catalog window-B guard and the `object_index` cleanup.
    traces_commit: Option<String>,
    /// `agent_checkpoint.tree_oid` (root tree of `traces_commit`).
    tree_oid: Option<String>,
    /// `agent_checkpoint.metadata_blob_oid` (the checkpoint's
    /// `metadata.json` blob).
    metadata_blob_oid: Option<String>,
}

impl CheckpointHistoryRow {
    fn from_query_result(row: QueryResult) -> Result<Self> {
        Ok(Self {
            checkpoint_id: row
                .try_get_by("checkpoint_id")
                .context("decode agent_checkpoint.checkpoint_id")?,
            session_id: row
                .try_get_by("session_id")
                .context("decode agent_checkpoint.session_id")?,
            agent_kind: row
                .try_get_by("agent_kind")
                .context("decode agent_session.agent_kind")?,
            scope: row
                .try_get_by("scope")
                .context("decode agent_checkpoint.scope")?,
            parent_commit: row.try_get_by("parent_commit").ok().flatten(),
            traces_commit: row.try_get_by("traces_commit").ok().flatten(),
            tree_oid: row.try_get_by("tree_oid").ok().flatten(),
            metadata_blob_oid: row.try_get_by("metadata_blob_oid").ok().flatten(),
        })
    }
}

#[derive(Debug, Clone)]
struct RewrittenCheckpoint {
    checkpoint_id: String,
    traces_commit: ObjectHash,
    tree_oid: ObjectHash,
}

/// OIDs that a prune provably makes unreachable and that are exclusively
/// referenced by the removed checkpoints — the conservative candidate set
/// for `object_index` cleanup (AG-20; the pre-fix behaviour leaked every
/// row forever).
///
/// Included per removed catalog row: its `traces_commit` (the commit
/// object), `tree_oid` (the commit's root tree), and `metadata_blob_oid`
/// (its `metadata.json` blob). Each is referenced only by that
/// checkpoint's chain entry by construction, and anything still referenced
/// is excluded below.
///
/// Deliberately **excluded** (exclusivity is not cheaply provable from the
/// catalog, so we skip rather than risk deleting a shared OID):
/// - inner checkpoint subtrees and transcript/events/manifest blobs of the
///   removed checkpoints (their OIDs are not recorded in the catalog);
/// - the pre-rewrite commits/root trees of RETAINED checkpoints (they may
///   be byte-identical to their rewritten successors, and the leak is
///   bounded by the retained-row count).
///
/// The exclusion set covers every OID the catalog still references after
/// the prune: retained rows' current OIDs plus the freshly rewritten
/// commits/trees and the new head.
fn collect_exclusive_unreachable_oids(
    removed_rows: &[CheckpointHistoryRow],
    retained_rows: &[CheckpointHistoryRow],
    rewritten: &[RewrittenCheckpoint],
) -> Vec<String> {
    let mut still_referenced: HashSet<String> = HashSet::new();
    for row in retained_rows {
        still_referenced.extend(
            [&row.traces_commit, &row.tree_oid, &row.metadata_blob_oid]
                .into_iter()
                .filter_map(|oid| oid.clone()),
        );
    }
    for item in rewritten {
        still_referenced.insert(item.traces_commit.to_string());
        still_referenced.insert(item.tree_oid.to_string());
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut unreachable = Vec::new();
    for row in removed_rows {
        for oid in [&row.traces_commit, &row.tree_oid, &row.metadata_blob_oid]
            .into_iter()
            .filter_map(|oid| oid.clone())
        {
            // Legacy rows may spell "no traces commit" as an EMPTY string
            // rather than NULL — an empty id is not an object to deindex,
            // and passing it to the deletion fence aborts the whole prune.
            if oid.is_empty() {
                continue;
            }
            if !still_referenced.contains(&oid) && seen.insert(oid.clone()) {
                unreachable.push(oid);
            }
        }
    }
    unreachable
}

fn checkpoint_tree_path(checkpoint_id: &str) -> Result<(String, String)> {
    let prefix = checkpoint_id
        .get(..2)
        .ok_or_else(|| anyhow!("checkpoint_id must be at least 2 characters"))?
        .to_string();
    let rest = checkpoint_id
        .get(2..)
        .ok_or_else(|| anyhow!("checkpoint_id must be valid UTF-8 at byte 2"))?
        .to_string();
    Ok((prefix, rest))
}

fn format_libra_trailers(params: &CheckpointCommitParams<'_>) -> String {
    let mut buf = String::new();
    buf.push_str(&format!("Libra-Session: {}\n", params.session_id));
    buf.push_str(&format!("Libra-Agent: {}\n", params.agent_kind));
    if let Some(commit) = params.parent_commit {
        buf.push_str(&format!("Libra-Parent-Commit: {commit}\n"));
    }
    buf.push_str(&format!("Libra-Checkpoint-ID: {}\n", params.checkpoint_id));
    buf.push_str(&format!("Libra-Scope: {}\n", params.scope.as_str()));
    if let Some(tool) = params.tool_use_id {
        buf.push_str(&format!("Libra-Tool-Use-ID: {tool}\n"));
    }
    buf
}

fn format_rewritten_checkpoint_trailers(row: &CheckpointHistoryRow) -> String {
    let mut buf = String::new();
    buf.push_str(&format!("Libra-Session: {}\n", row.session_id));
    buf.push_str(&format!("Libra-Agent: {}\n", row.agent_kind));
    if let Some(commit) = &row.parent_commit {
        buf.push_str(&format!("Libra-Parent-Commit: {commit}\n"));
    }
    buf.push_str(&format!("Libra-Checkpoint-ID: {}\n", row.checkpoint_id));
    buf.push_str(&format!("Libra-Scope: {}\n", row.scope));
    buf
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, Schema, Statement};
    use tempfile::tempdir;
    use tokio::time::sleep;

    #[test]
    fn checkpoint_durability_aggregate_object_bound_is_fail_closed() {
        let mut durable = HashSet::new();
        insert_durable_oid_bounded(&mut durable, "one".to_string(), 2).expect("first object");
        insert_durable_oid_bounded(&mut durable, "two".to_string(), 2).expect("second object");
        insert_durable_oid_bounded(&mut durable, "two".to_string(), 2)
            .expect("duplicate does not consume the bound");
        let error = insert_durable_oid_bounded(&mut durable, "three".to_string(), 2)
            .expect_err("a distinct object beyond the aggregate bound must fail");
        assert!(error.to_string().contains("aggregate object limit"));
        assert_eq!(durable.len(), 2);
    }

    #[test]
    fn subagent_content_reservation_error_display_is_stable_and_actionable() {
        let error = SubagentContentReservationPruneGuard {
            session_id: "session-7".to_string(),
            attempt_id: "checkpoint-9".to_string(),
            lease_expires_at: 1_700_000_123_456,
        };
        assert_eq!(
            error.to_string(),
            "refusing to prune traces checkpoints: a subagent content write is reserved \
             (session 'session-7', attempt 'checkpoint-9', lease expires at \
             1700000123456); retry once the writer finishes or the lease expires"
        );
    }

    #[test]
    fn checkpoint_object_helper_rejects_compression_bomb_before_unbounded_inflate() {
        let repo = tempdir().unwrap();
        let oversized = vec![0_u8; CHECKPOINT_OBJECT_READ_MAX_INFLATED_BYTES as usize + 1];
        let (oid, _) = write_git_object_with_status(repo.path(), "tree", &oversized).unwrap();
        let request = CheckpointObjectIoHelperRequest {
            repo_path_base64: encode_checkpoint_object_path(repo.path()).unwrap(),
            operation: CheckpointObjectIoOperation::Read {
                oid: oid.to_string(),
                expected_type: "tree".to_string(),
            },
        };
        let frame = serde_json::to_vec(&request).unwrap();
        let response: CheckpointObjectIoHelperResponse =
            serde_json::from_slice(&run_checkpoint_object_io_helper(&frame).unwrap()).unwrap();
        let CheckpointObjectIoHelperResponse::Error { message } = response else {
            panic!("oversized compressed object was accepted")
        };
        assert!(
            message.contains("exceeding") && message.contains("checkpoint read limit"),
            "unexpected oversized-object error: {message}"
        );
    }

    /// plan-20260713 DR-05c-0: the shared rebuild boundary classifies both
    /// auto-rebuildable scopes and fails closed on anything else.
    #[test]
    fn rebuilt_catalog_row_committed_and_subagent() {
        let base = RebuildCatalogRowInputs {
            scope: "committed".to_string(),
            checkpoint_id: "cp1".to_string(),
            session_id: "s1".to_string(),
            parent_commit: Some("p".to_string()),
            tree_oid: "t".to_string(),
            metadata_blob_oid: "m".to_string(),
            traces_commit: "c".to_string(),
            created_at: 7,
            ..Default::default()
        };
        match rebuild_catalog_row_from_traces_ref(base.clone()).expect("committed rebuilds") {
            RebuiltCatalogRow::Committed {
                checkpoint_id,
                session_id,
                created_at,
                ..
            } => {
                assert_eq!(checkpoint_id, "cp1");
                assert_eq!(session_id, "s1");
                assert_eq!(created_at, 7);
            }
            other => panic!("expected Committed, got {other:?}"),
        }

        let sub = RebuildCatalogRowInputs {
            scope: "subagent".to_string(),
            parent_checkpoint_id: Some("parent-cp".to_string()),
            tool_use_id: Some("tool-1".to_string()),
            ..base.clone()
        };
        match rebuild_catalog_row_from_traces_ref(sub).expect("subagent rebuilds") {
            RebuiltCatalogRow::Subagent {
                parent_checkpoint_id,
                tool_use_id,
                ..
            } => {
                assert_eq!(parent_checkpoint_id.as_deref(), Some("parent-cp"));
                assert_eq!(tool_use_id.as_deref(), Some("tool-1"));
            }
            other => panic!("expected Subagent, got {other:?}"),
        }

        // Fail-closed: unknown scopes are an error, never a guessed shape.
        let weird = RebuildCatalogRowInputs {
            scope: "temporary".to_string(),
            ..base
        };
        assert!(rebuild_catalog_row_from_traces_ref(weird).is_err());
    }

    use super::*;

    /// W2 §C.4.3 end-to-end unblock: an `i64::MAX`-dated marker row blocks
    /// the listing fail-closed, and `agent doctor --repair`'s retirement
    /// entry point RETIRES it (the drain classifies untrustworthy rows as
    /// retirable), after which the listing succeeds again.
    #[tokio::test]
    #[serial_test::serial]
    async fn doctor_repair_retires_future_dated_marker_and_unblocks_listing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = crate::utils::test::ChangeDirGuard::new(tmp.path());
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        let db = crate::internal::db::get_db_conn_instance().await;
        let now = chrono::Utc::now().timestamp_millis();
        let mut marker = TracesInflightMarker::new("sess-max", "attempt-max", now);
        marker.started_at_ms = i64::MAX;
        crate::internal::metadata::MetadataKv::set_with_conn(
            &db,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            "sess-max",
            "attempt-max",
            &serde_json::to_string(&marker).expect("encode"),
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("seed i64::MAX marker");

        list_live_traces_inflight_markers(&db, now)
            .await
            .expect_err("the corrupt row blocks the listing");

        let storage: Arc<dyn crate::utils::storage::Storage + Send + Sync> =
            Arc::new(crate::utils::storage::local::LocalStorage::new(
                crate::utils::util::storage_path().join("objects"),
            ));
        let history = HistoryManager::new_with_ref(
            storage,
            crate::utils::util::storage_path(),
            Arc::new(db.clone()),
            "refs/libra/traces",
        );
        let retired = history
            .repair_expired_traces_inflight_marker("sess-max", "attempt-max", now)
            .await
            .expect("doctor repair retires the untrustworthy marker");
        assert!(retired, "the marker row was fully retired");

        let live = list_live_traces_inflight_markers(&db, now)
            .await
            .expect("the listing is unblocked after retirement");
        assert!(live.is_empty(), "no marker remains: {live:?}");
    }

    /// W2 §C.4.3: the LISTING fails closed on a future-dated marker row —
    /// every destructive consumer (gc defer/roots, prune, erasure) stops
    /// instead of silently losing or trusting the row.
    #[tokio::test]
    #[serial_test::serial]
    async fn listing_fails_closed_on_future_dated_marker_row() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = crate::utils::test::ChangeDirGuard::new(tmp.path());
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        let db = crate::internal::db::get_db_conn_instance().await;
        let now = chrono::Utc::now().timestamp_millis();
        let marker = TracesInflightMarker::new("sess-f", "attempt-f", now + 48 * 60 * 60 * 1000);
        crate::internal::metadata::MetadataKv::set_with_conn(
            &db,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            "sess-f",
            "attempt-f",
            &serde_json::to_string(&marker).expect("encode"),
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("seed future-dated marker");

        let err = list_live_traces_inflight_markers(&db, now)
            .await
            .expect_err("future-dated row must fail the listing closed");
        assert!(
            format!("{err:#}").contains("future-dated"),
            "actionable corruption error: {err:#}"
        );
    }

    /// W2 §C.4.3: marker liveness is a DETERMINISTIC absolute deadline from
    /// the persisted fields (TTL capped at 24h, no per-read re-anchoring),
    /// and a future-dated start beyond skew tolerance marks the ROW ITSELF
    /// untrustworthy — the listing fails closed on it instead of silently
    /// filtering or trusting it.
    #[test]
    fn inflight_marker_liveness_is_bounded_and_deterministic() {
        let now = 1_700_000_000_000_i64;
        let mut marker = TracesInflightMarker::new("s", "a", now - 60_000);
        // Normal recent marker with its ordinary TTL: live and trustworthy.
        assert!(marker.time_fields_trustworthy(now));
        assert!(marker.is_live(now));
        // Absurd TTL is capped: dead 24h past start no matter the claim.
        marker.ttl_ms = i64::MAX;
        assert!(marker.is_live(now));
        assert!(!marker.is_live(marker.started_at_ms + TRACES_INFLIGHT_MAX_LIVE_MS + 1));
        // Future-dated start beyond skew: UNTRUSTWORTHY at any read time
        // before the claimed start — the listing bails rather than judging
        // liveness; once real time catches up the row is trustworthy again
        // and the ordinary capped deadline applies.
        marker.started_at_ms = now + 48 * 60 * 60 * 1000;
        marker.ttl_ms = 600_000;
        assert!(!marker.time_fields_trustworthy(now));
        assert!(!marker.time_fields_trustworthy(now + 60 * 60 * 1000));
        assert!(marker.time_fields_trustworthy(marker.started_at_ms));
        assert!(marker.is_live(marker.started_at_ms + 1));
        assert!(!marker.is_live(marker.started_at_ms + 600_001));
        // Small clock skew is tolerated deterministically.
        marker.started_at_ms = now + 60_000;
        assert!(marker.time_fields_trustworthy(now));
        assert!(marker.is_live(now));
        assert!(!marker.is_live(now + TRACES_INFLIGHT_MAX_LIVE_MS + 120_000));
    }
    use crate::{internal::db, utils::storage::local::LocalStorage};

    #[cfg(unix)]
    #[test]
    fn cleanup_index_snapshot_rejects_fifo_and_symlink_inputs_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

        let dir = tempdir().unwrap();
        let fifo = dir.path().join("index-fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_name is NUL-terminated inside this test's tempdir.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        let error = read_cleanup_regular_file(&fifo, 1024, 1024, "test cleanup index")
            .expect_err("FIFO index must fail closed");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(format!("{error:#}").contains("not a regular file"));

        let target = dir.path().join("real-index");
        std::fs::write(&target, b"index").unwrap();
        let symlink = dir.path().join("index-symlink");
        std::os::unix::fs::symlink(&target, &symlink).unwrap();
        let error = read_cleanup_regular_file(&symlink, 1024, 1024, "test cleanup index")
            .expect_err("symlink index must fail closed");
        assert!(format!("{error:#}").contains("without following symlinks"));
    }

    #[test]
    fn cleanup_index_snapshot_rejects_growth_after_held_descriptor_metadata() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("growing-index");
        std::fs::write(&path, b"1234").unwrap();
        let path_for_hook = path.clone();
        let error =
            read_cleanup_regular_file_inner(&path, 4, 4, "test growing cleanup index", move || {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(path_for_hook)
                    .unwrap()
                    .write_all(b"5")
                    .unwrap();
            })
            .expect_err("post-metadata growth must fail closed");
        assert!(format!("{error:#}").contains("grew beyond"));
    }

    #[test]
    fn cleanup_helper_child_sleeper_process() {
        if std::env::var_os("LIBRA_TEST_CLEANUP_HELPER_CHILD_SLEEPER").is_some() {
            std::thread::sleep(Duration::from_secs(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_helper_guard_returns_promptly_and_reaps_repeated_timeouts() {
        use std::sync::atomic::Ordering;

        let executable = std::env::current_exe().expect("resolve test executable");
        let reaper = cleanup_helper_reaper_sender().expect("start cleanup child reaper");
        let reaped_before = CLEANUP_HELPER_REAPED_CHILDREN.load(Ordering::SeqCst);
        let mut pids = Vec::new();
        for _ in 0..3 {
            let child = Command::new(&executable)
                .arg("cleanup_helper_child_sleeper_process")
                .arg("--nocapture")
                .env("LIBRA_TEST_CLEANUP_HELPER_CHILD_SLEEPER", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start sleeper child");
            pids.push(child.id());
            let guard = CleanupHelperChild::new(child, reaper.clone());
            std::thread::sleep(Duration::from_millis(75));
            let started = Instant::now();
            drop(guard);
            assert!(
                started.elapsed() < Duration::from_millis(250),
                "timeout cleanup blocked while waiting for a killed helper"
            );
        }

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let all_reaped = pids
                .iter()
                .all(|pid| !Path::new(&format!("/proc/{pid}")).exists());
            let observed = CLEANUP_HELPER_REAPED_CHILDREN.load(Ordering::SeqCst);
            if all_reaped && observed >= reaped_before + pids.len() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "killed cleanup helpers were not reaped before the test deadline"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    async fn setup_test_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let builder = db.get_database_backend();
        let schema = Schema::new(builder);
        let stmt = schema.create_table_from_entity(reference::Entity);
        db.execute(builder.build(&stmt)).await.unwrap();
        db
    }

    #[tokio::test]
    async fn test_history_append_simple() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage.clone(), repo_path.clone(), db_conn.clone());

        // 1. Append first object
        let blob_hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        manager.append("task", "task-1", blob_hash).await.unwrap();

        // Verify ref exists in DB
        let ref_model = reference::Entity::find()
            .filter(reference::Column::Name.eq(AI_REF))
            .filter(reference::Column::Kind.eq(ConfigKind::Branch))
            .one(&*db_conn)
            .await
            .unwrap()
            .expect("Reference should exist");

        let commit_hash_str = ref_model.commit.expect("Commit hash should exist");
        let commit_hash = ObjectHash::from_str(&commit_hash_str).unwrap();

        // Verify we can load commit
        let data = read_git_object(&repo_path, &commit_hash).unwrap();
        let content = String::from_utf8_lossy(&data);
        assert!(content.contains("tree "));
        assert!(content.contains("Update task/task-1"));

        // 2. Append second object (same type)
        let blob_hash_2 = ObjectHash::from_str("f4e6d0434b8b29ae775ad8c2e48c5391e69de29b").unwrap();
        manager.append("task", "task-2", blob_hash_2).await.unwrap();

        // 3. Append third object (different type)
        manager.append("run", "run-1", blob_hash).await.unwrap();

        // Load Head Commit from DB
        let head = manager.resolve_history_head().await.unwrap().unwrap();

        // Verify we can load commit
        let data = read_git_object(&repo_path, &head).unwrap();
        let content = String::from_utf8_lossy(&data);
        assert!(content.contains("tree "));
        assert!(content.contains("Update run/run-1"));
    }

    #[tokio::test]
    async fn test_find_object_hashes_returns_all_matching_types() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage.clone(), repo_path.clone(), db_conn.clone());

        let blob_hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        let other_hash = ObjectHash::from_str("f4e6d0434b8b29ae775ad8c2e48c5391e69de29b").unwrap();

        manager
            .append("patchset", "shared-id", blob_hash)
            .await
            .unwrap();
        manager
            .append("event", "shared-id", other_hash)
            .await
            .unwrap();

        let matches = manager.find_object_hashes("shared-id").await.unwrap();
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().any(|(_, kind)| kind == "patchset"));
        assert!(matches.iter().any(|(_, kind)| kind == "event"));
    }

    #[tokio::test]
    async fn test_list_object_types_returns_sorted_types() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage.clone(), repo_path.clone(), db_conn.clone());

        let blob_hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        manager
            .append("run_event", "run-event-1", blob_hash)
            .await
            .unwrap();
        manager
            .append("patchset", "patchset-1", blob_hash)
            .await
            .unwrap();

        let types = manager.list_object_types().await.unwrap();
        assert_eq!(types, vec!["patchset".to_string(), "run_event".to_string()]);
    }

    #[tokio::test]
    async fn test_update_ref_retries_when_sqlite_is_locked() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");
        std::fs::create_dir(&objects_dir).unwrap();
        let db_path = repo_path.join("libra.db");

        let db_conn = Arc::new(
            db::create_database(db_path.to_str().unwrap())
                .await
                .expect("failed to create sqlite database"),
        );
        let storage = Arc::new(LocalStorage::new(objects_dir));
        let manager = HistoryManager::new(storage, repo_path.clone(), db_conn.clone());

        let locker = db::establish_connection_with_busy_timeout(
            db_path.to_str().unwrap(),
            Duration::from_millis(50),
        )
        .await
        .expect("failed to open lock holder connection");
        let backend = locker.get_database_backend();
        locker
            .execute(Statement::from_string(backend, "BEGIN EXCLUSIVE"))
            .await
            .expect("failed to acquire sqlite exclusive lock");

        let release = {
            let locker = locker.clone();
            tokio::spawn(async move {
                sleep(Duration::from_millis(250)).await;
                let backend = locker.get_database_backend();
                locker
                    .execute(Statement::from_string(backend, "COMMIT"))
                    .await
                    .expect("failed to release sqlite exclusive lock");
            })
        };

        let hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        manager
            .update_ref(AI_REF, hash)
            .await
            .expect("update_ref should retry through a transient sqlite lock");
        release.await.unwrap();

        let resolved = manager
            .resolve_history_head()
            .await
            .expect("history head should be readable after retry")
            .expect("history head should exist");
        assert_eq!(resolved, hash);
    }

    // -- plan-20260713 DR-05c-0 required M1 tests ---------------------------

    fn traces_manager(dir: &tempfile::TempDir, db_conn: Arc<DatabaseConnection>) -> HistoryManager {
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir_all(repo_path.join("objects")).unwrap();
        let storage = Arc::new(LocalStorage::new(repo_path.join("objects")));
        HistoryManager::new_with_ref(
            storage,
            repo_path,
            db_conn,
            crate::internal::branch::TRACES_BRANCH,
        )
    }

    fn checkpoint_params<'a>(
        checkpoint_id: &'a str,
        marker_generation: &'a str,
        blobs: &'a RedactedBytes,
        txn_extra: Option<&'a dyn TracesTxnExtra>,
    ) -> CheckpointCommitParams<'a> {
        CheckpointCommitParams {
            checkpoint_id,
            session_id: "claude_code__s1",
            marker_generation,
            agent_kind: "claude_code",
            parent_commit: None,
            scope: CheckpointScope::Committed,
            tool_use_id: None,
            metadata_json: blobs,
            transcript_redacted: blobs,
            lifecycle_events_jsonl: blobs,
            redaction_report_json: blobs,
            txn_extra,
            deadline: None,
        }
    }

    async fn prepare_checkpoint_test_schema(conn: &DatabaseConnection) {
        conn.execute(Statement::from_string(
            conn.get_database_backend(),
            include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
        ))
        .await
        .expect("create checkpoint marker registry");
        conn.execute(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE IF NOT EXISTS agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)"
                .to_string(),
        ))
        .await
        .expect("create checkpoint cleanup catalog probe");
    }

    async fn seed_test_writer_fence(
        conn: &DatabaseConnection,
        session_id: &str,
        attempt_id: &str,
    ) -> TracesWriterFence {
        let marker = TracesInflightMarker::new(
            session_id,
            attempt_id,
            chrono::Utc::now().timestamp_millis(),
        );
        write_traces_inflight_marker(conn, &marker)
            .await
            .expect("seed writer marker generation");
        TracesWriterFence {
            session_id: session_id.to_string(),
            attempt_id: attempt_id.to_string(),
            generation: marker.generation.expect("new marker generation"),
        }
    }

    async fn append_test_checkpoint(
        manager: &HistoryManager,
        checkpoint_id: &str,
        blobs: &RedactedBytes,
        txn_extra: Option<&dyn TracesTxnExtra>,
    ) -> Result<CheckpointCommit> {
        let marker = TracesInflightMarker::new(
            "claude_code__s1",
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        write_traces_inflight_marker(manager.db_conn.as_ref(), &marker).await?;
        let marker_generation = marker
            .generation
            .as_deref()
            .context("new test marker has no writer generation")?;
        let result = manager
            .append_checkpoint_commit(checkpoint_params(
                checkpoint_id,
                marker_generation,
                blobs,
                txn_extra,
            ))
            .await;
        if let Ok(written) = &result {
            clear_traces_inflight_marker_if_generation(
                manager.db_conn.as_ref(),
                "claude_code__s1",
                checkpoint_id,
                &written.marker_generation,
            )
            .await?;
        }
        result
    }

    /// ref_cas_head_changed_rebuilds_commit_before_retry: a competing commit
    /// lands BETWEEN the loop's head read and its CAS (deterministically, via
    /// the test-only injection hook) — the CAS must reject the stale attempt,
    /// the loop must RETRY (cas_retries > 0) and REBUILD the commit parented
    /// on the freshly-read head, keeping the chain linear.
    #[tokio::test]
    async fn ref_cas_head_changed_rebuilds_commit_before_retry() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let mut manager = traces_manager(&dir, db_conn.clone());
        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());

        // Seed head H0.
        let seeded = append_test_checkpoint(
            &manager,
            "aaaa0000-0000-0000-0000-000000000001",
            &blobs,
            None,
        )
        .await
        .expect("seed checkpoint");
        let h0 = seeded.commit_hash;

        // Competing writer, fired from INSIDE the tested append's
        // read→CAS window (first attempt only) via the injection hook.
        let interloper = Arc::new(traces_manager(&dir, db_conn.clone()));
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let interloper_commit: Arc<std::sync::Mutex<Option<ObjectHash>>> =
            Arc::new(std::sync::Mutex::new(None));
        {
            let interloper = interloper.clone();
            let fired = fired.clone();
            let interloper_commit = interloper_commit.clone();
            manager.test_after_head_read = Some(Arc::new(move || {
                let interloper = interloper.clone();
                let fired = fired.clone();
                let interloper_commit = interloper_commit.clone();
                Box::pin(async move {
                    if fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        return Ok(()); // only the first attempt races
                    }
                    let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
                    let won = append_test_checkpoint(
                        &interloper,
                        "bbbb0000-0000-0000-0000-000000000002",
                        &blobs,
                        None,
                    )
                    .await?;
                    *interloper_commit.lock().unwrap() = Some(won.commit_hash);
                    Ok(())
                })
            }));
        }

        let rebuilt = append_test_checkpoint(
            &manager,
            "cccc0000-0000-0000-0000-000000000003",
            &blobs,
            None,
        )
        .await
        .expect("append survives the mid-window head move");
        let h1 = interloper_commit
            .lock()
            .unwrap()
            .expect("interloper committed");

        // A real retry happened …
        assert!(
            rebuilt.cas_retries > 0,
            "the first attempt must lose the CAS and retry, got cas_retries = {}",
            rebuilt.cas_retries
        );
        // … and the rebuilt commit parents the interloper's head, not H0.
        let data = read_git_object(&manager.repo_path, &rebuilt.commit_hash).unwrap();
        let content = String::from_utf8_lossy(&data);
        assert!(
            content.contains(&format!("parent {h1}")),
            "rebuilt commit must parent the NEW head {h1}, got:\n{content}"
        );
        assert!(
            !content.contains(&format!("parent {h0}")),
            "rebuilt commit must not still parent the stale head {h0}"
        );
        let head = manager.resolve_history_head().await.unwrap().unwrap();
        assert_eq!(head, rebuilt.commit_hash, "chain stays linear");
    }

    /// crash_after_objects_before_ref_leaves_only_gc_objects AND
    /// crash_after_ref_before_catalog_is_impossible_or_atomically_recovers:
    /// with a transactional extra, ref + companion writes are one atomic
    /// unit. A failing extra (simulating the claim/catalog write dying after
    /// objects were built) must leave the ref UNMOVED — only unreachable
    /// loose objects remain; the success path lands ref + companion row
    /// together, so a "ref moved but catalog missing" window cannot exist.
    #[tokio::test]
    async fn crash_between_objects_ref_and_catalog_is_atomic() {
        struct FailingExtra;
        #[async_trait::async_trait]
        impl TracesTxnExtra for FailingExtra {
            async fn apply(
                &self,
                _txn: &DatabaseTransaction,
                _ctx: &TracesCommitCtx,
            ) -> Result<()> {
                anyhow::bail!("simulated crash after objects, inside the final transaction")
            }
        }
        struct MarkerExtra;
        #[async_trait::async_trait]
        impl TracesTxnExtra for MarkerExtra {
            async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
                // Stand-in for the catalog INSERT: a reference row keyed by
                // the commit, written in the SAME transaction as the ref.
                let marker = reference::ActiveModel {
                    name: Set(Some(format!("marker/{}", ctx.commit_hash))),
                    kind: Set(ConfigKind::Branch),
                    commit: Set(Some(ctx.commit_hash.clone())),
                    remote: Set(None),
                    ..Default::default()
                };
                marker.insert(txn).await?;
                Ok(())
            }
        }

        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let manager = traces_manager(&dir, db_conn.clone());
        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());

        // Seed head H0.
        let seeded = append_test_checkpoint(
            &manager,
            "aaaa0000-0000-0000-0000-00000000000a",
            &blobs,
            None,
        )
        .await
        .expect("seed");
        let h0 = seeded.commit_hash;

        // Failing extra: append errors, ref must NOT move (objects on disk
        // are the only residue — the documented GC-only window).
        let failing = FailingExtra;
        let err = append_test_checkpoint(
            &manager,
            "bbbb0000-0000-0000-0000-00000000000b",
            &blobs,
            Some(&failing),
        )
        .await
        .expect_err("failing extra must fail the append closed");
        assert!(
            format!("{err:#}").contains("simulated crash"),
            "got {err:#}"
        );
        assert_eq!(
            manager.resolve_history_head().await.unwrap().unwrap(),
            h0,
            "ref must not move when the companion transaction fails"
        );

        // Success path: ref + companion row land atomically.
        let marker = MarkerExtra;
        let committed = append_test_checkpoint(
            &manager,
            "cccc0000-0000-0000-0000-00000000000c",
            &blobs,
            Some(&marker),
        )
        .await
        .expect("append with marker extra");
        assert_eq!(
            manager.resolve_history_head().await.unwrap().unwrap(),
            committed.commit_hash
        );
        let marker_row = reference::Entity::find()
            .filter(reference::Column::Name.eq(format!("marker/{}", committed.commit_hash)))
            .one(&*db_conn)
            .await
            .unwrap();
        assert!(
            marker_row.is_some(),
            "companion row must exist the instant the ref moved (same txn)"
        );
    }

    #[tokio::test]
    async fn test_update_ref_if_matches_rejects_stale_history_head() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage, repo_path, db_conn);

        let task_hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        let plan_hash = ObjectHash::from_str("f4e6d0434b8b29ae775ad8c2e48c5391e69de29b").unwrap();
        let frame_hash = ObjectHash::from_str("a4e6d0434b8b29ae775ad8c2e48c5391e69de29b").unwrap();

        manager.append("task", "task-1", task_hash).await.unwrap();
        let stale_head = manager.resolve_history_head().await.unwrap();
        let stale_commit = manager
            .create_append_commit(stale_head, "plan", "plan-1", plan_hash)
            .expect("stale append commit should be created");

        manager
            .append("context_frame", "frame-1", frame_hash)
            .await
            .unwrap();

        let outcome = manager
            .update_ref_if_matches(AI_REF, stale_head, stale_commit)
            .await
            .expect("stale ref update should not error");
        assert_eq!(outcome, RefUpdateOutcome::HeadChanged);

        manager.append("plan", "plan-1", plan_hash).await.unwrap();

        assert!(
            manager
                .get_object_hash("context_frame", "frame-1")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            manager
                .get_object_hash("plan", "plan-1")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn fenced_expired_writer_cannot_adopt_same_id_takeover_marker() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let manager = traces_manager(&dir, db_conn.clone());
        let marker = TracesInflightMarker::new("fenced-session", "fenced-attempt", 0);
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("write expired writer marker");
        let stale_fence = TracesWriterFence {
            session_id: marker.session_id.clone(),
            attempt_id: marker.attempt_id.clone(),
            generation: marker.generation.clone().expect("stale marker generation"),
        };
        let replacement = TracesInflightMarker::new(
            "fenced-session",
            "fenced-attempt",
            chrono::Utc::now().timestamp_millis(),
        );
        write_traces_inflight_marker(&*db_conn, &replacement)
            .await
            .expect("replace marker with takeover generation");
        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
        let public_error = manager
            .append_checkpoint_commit(CheckpointCommitParams {
                checkpoint_id: "fenced-attempt",
                session_id: "fenced-session",
                marker_generation: &stale_fence.generation,
                agent_kind: "claude_code",
                parent_commit: None,
                scope: CheckpointScope::Subagent,
                tool_use_id: None,
                metadata_json: &blobs,
                transcript_redacted: &blobs,
                lifecycle_events_jsonl: &blobs,
                redaction_report_json: &blobs,
                txn_extra: None,
                deadline: None,
            })
            .await
            .expect_err("public append must not adopt a takeover marker generation");
        assert!(
            format!("{public_error:#}").contains("fenced or replaced before append"),
            "unexpected error: {public_error:#}"
        );
        let public_fence = manager
            .load_traces_writer_fence("fenced-session", "fenced-attempt")
            .await
            .expect("load takeover writer fence after rejected public append");
        assert_eq!(
            public_fence.generation,
            replacement
                .generation
                .as_deref()
                .expect("takeover marker generation"),
            "stale public append mutated the takeover marker"
        );
        let new_head = write_git_object(&manager.repo_path, "blob", b"stalled writer commit")
            .expect("write stalled writer object");

        let preclaim_error = manager
            .persist_attempt_oid_before_write(&stale_fence, &new_head, None)
            .await
            .expect_err("replacement generation must fence stale object preclaims");
        assert!(
            format!("{preclaim_error:#}").contains("generation was fenced or replaced"),
            "unexpected error: {preclaim_error:#}"
        );
        let current_fence = manager
            .load_traces_writer_fence("fenced-session", "fenced-attempt")
            .await
            .expect("load takeover writer fence after rejected preclaim");
        assert_eq!(
            current_fence.generation,
            replacement
                .generation
                .as_deref()
                .expect("takeover marker generation"),
            "stale object preclaim mutated the takeover marker"
        );

        let error = manager
            .update_ref_if_matches_with_extra(
                crate::internal::branch::TRACES_BRANCH,
                None,
                new_head,
                None,
                None,
                Some(&stale_fence),
            )
            .await
            .expect_err("replacement generation must fence a resumed writer");
        assert!(
            format!("{error:#}").contains("generation was fenced or replaced"),
            "unexpected error: {error:#}"
        );
        assert!(
            manager
                .resolve_history_head()
                .await
                .expect("read traces head")
                .is_none(),
            "fenced writer moved the traces ref"
        );
    }

    // -------------------------------------------------------------------
    // AG-20: E5 line-safe chunking
    // -------------------------------------------------------------------

    /// Small inputs (≤ max) come back as one borrowed chunk, unsplit.
    #[test]
    fn chunker_returns_single_chunk_at_or_below_threshold() {
        let content = b"line-1\nline-2\n";
        let chunks = chunk_transcript_line_safe(content, content.len()).unwrap();
        assert_eq!(chunks, vec![&content[..]]);
        // Empty input still yields one (empty) chunk to name.
        let empty = chunk_transcript_line_safe(b"", 16).unwrap();
        assert_eq!(empty, vec![&b""[..]]);
    }

    /// Chunks cut ONLY at line boundaries, each stays within the limit,
    /// and concatenating them reproduces the input byte-for-byte.
    #[test]
    fn chunker_splits_on_line_boundaries_and_roundtrips() {
        let mut content = Vec::new();
        for index in 0..100 {
            content.extend_from_slice(format!("{{\"line\":{index}}}\n").as_bytes());
        }
        let max = 64;
        let chunks = chunk_transcript_line_safe(&content, max).unwrap();
        assert!(chunks.len() > 1, "must actually chunk");
        for chunk in &chunks {
            assert!(chunk.len() <= max, "chunk of {} exceeds {max}", chunk.len());
            assert!(
                chunk.ends_with(b"\n"),
                "every newline-terminated input chunk must end at a line boundary"
            );
        }
        let owned: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
        assert_eq!(reassemble_transcript_chunks(&owned), content);
    }

    /// A final unterminated line is preserved verbatim (no invented `\n`).
    #[test]
    fn chunker_preserves_final_unterminated_line() {
        let content = b"aaaa\nbbbb\ncccc-tail";
        let chunks = chunk_transcript_line_safe(content, 10).unwrap();
        let owned: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
        assert_eq!(reassemble_transcript_chunks(&owned), content.to_vec());
        assert!(chunks.last().unwrap().ends_with(b"cccc-tail"));
    }

    /// E5 hard error: a single line larger than the threshold refuses to
    /// split mid-line.
    #[test]
    fn chunker_rejects_single_line_over_threshold() {
        let long_line = vec![b'x'; 100];
        let err = chunk_transcript_line_safe(&long_line, 64).unwrap_err();
        assert!(
            err.to_string().contains("exceeds"),
            "error must explain the oversized line: {err}"
        );
        // Terminated variant errors too.
        let mut terminated = long_line.clone();
        terminated.push(b'\n');
        assert!(chunk_transcript_line_safe(&terminated, 64).is_err());
        // Zero max is rejected outright.
        assert!(chunk_transcript_line_safe(b"x", 0).is_err());
    }

    // -------------------------------------------------------------------
    // AG-20: content hash format + reader tolerance
    // -------------------------------------------------------------------

    /// Writer format is `sha256:` + 64 lowercase hex, no trailing newline,
    /// and equals the sha256 of the concatenated sections.
    #[test]
    fn content_hash_has_pinned_format_and_value() {
        let hash = checkpoint_content_hash(&[b"alpha", b"beta"]);
        assert!(hash.starts_with("sha256:"));
        let hex = &hash["sha256:".len()..];
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!hash.ends_with('\n'));
        // Concatenation order matters and is deterministic.
        assert_eq!(hash, checkpoint_content_hash(&[b"alphabeta"]));
        assert_ne!(hash, checkpoint_content_hash(&[b"beta", b"alpha"]));
    }

    /// Reader tolerance (E4-entire table): the prefix form and legacy bare
    /// hex both parse to the same digest; garbage does not parse.
    #[test]
    fn parse_content_hash_accepts_prefix_and_legacy_bare_hex() {
        let digest = "a".repeat(64);
        assert_eq!(
            parse_content_hash(&format!("sha256:{digest}")),
            Some(digest.clone())
        );
        assert_eq!(parse_content_hash(&digest), Some(digest.clone()));
        // Whitespace slack (e.g. a stray trailing newline) is tolerated.
        assert_eq!(
            parse_content_hash(&format!("sha256:{digest}\n")),
            Some(digest.clone())
        );
        // Uppercase hex normalises to lowercase.
        assert_eq!(
            parse_content_hash(&digest.to_uppercase()),
            Some(digest.clone())
        );
        assert_eq!(parse_content_hash("sha256:tooshort"), None);
        assert_eq!(parse_content_hash(&"z".repeat(64)), None);
        assert_eq!(parse_content_hash(""), None);
    }

    // -------------------------------------------------------------------
    // AG-20: in-flight marker liveness math
    // -------------------------------------------------------------------

    #[test]
    fn inflight_marker_liveness_respects_ttl() {
        let marker = TracesInflightMarker::new("session-a", "attempt-1", 1_000);
        assert!(marker.is_live(1_000));
        assert!(marker.is_live(1_000 + AGENT_TRACES_INFLIGHT_TTL_MS - 1));
        assert!(!marker.is_live(1_000 + AGENT_TRACES_INFLIGHT_TTL_MS));
        // Marker JSON round-trips (schema pin for the prune side).
        let json = serde_json::to_string(&marker).unwrap();
        let back: TracesInflightMarker = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "session-a");
        assert_eq!(back.attempt_id, "attempt-1");
        assert_eq!(back.ttl_ms, AGENT_TRACES_INFLIGHT_TTL_MS);
        assert_eq!(back.schema_version, 3);
        assert!(
            back.generation
                .as_deref()
                .is_some_and(|generation| uuid::Uuid::parse_str(generation).is_ok())
        );
        assert!(back.commit.is_none());
        assert!(back.oids.is_empty());
        assert!(back.created_oids.is_empty());
        assert!(!back.cleanup_pending);
    }

    #[tokio::test]
    async fn rejected_cleanup_job_survives_an_unrelated_live_writer() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        let manager = traces_manager(&dir, db_conn.clone());
        let active = TracesInflightMarker::new(
            "session-active",
            "attempt-active",
            chrono::Utc::now().timestamp_millis(),
        );
        write_traces_inflight_marker(&*db_conn, &active)
            .await
            .expect("write unrelated active marker");

        let (oid, created) = write_git_object_with_status(
            &dir.path().join(".libra"),
            "blob",
            b"rejected-cleanup-candidate",
        )
        .expect("write cleanup candidate");
        assert!(created);
        let candidates = HashSet::from([oid.to_string()]);
        let rejected_fence =
            seed_test_writer_fence(&db_conn, "session-rejected", "attempt-rejected").await;
        manager
            .cleanup_rejected_checkpoint_objects(&rejected_fence, &candidates)
            .await
            .expect("live peer should defer, not discard, cleanup");
        let markers = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list durable cleanup markers");
        let pending = markers
            .iter()
            .find(|marker| marker.attempt_id == "attempt-rejected")
            .expect("cleanup job must remain durable");
        assert!(pending.cleanup_pending);
        assert_eq!(pending.created_oids, vec![oid.to_string()]);
        let object_path = dir
            .path()
            .join(".libra/objects")
            .join(&oid.to_string()[..2])
            .join(&oid.to_string()[2..]);
        assert!(object_path.exists(), "live peer must defer deletion");

        let mut expired = active;
        expired.started_at_ms = 0;
        expired.ttl_ms = 0;
        write_traces_inflight_marker(&*db_conn, &expired)
            .await
            .expect("expire unrelated marker");
        manager
            .drain_rejected_checkpoint_cleanup_jobs()
            .await
            .expect("drain persisted cleanup after live peer exits");
        assert!(
            object_path.exists(),
            "inline cleanup must leave shared object reclamation to repository GC"
        );
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list post-drain markers")
                .iter()
                .all(|marker| marker.attempt_id != "attempt-rejected"),
            "cleanup ownership marker survived successful drain"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_never_deletes_an_unresolved_preclaim() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let manager = traces_manager(&dir, db_conn.clone());
        let (oid, created) = write_git_object_with_status(
            &manager.repo_path,
            "blob",
            b"published by a concurrent writer",
        )
        .expect("write concurrent object");
        assert!(created);

        let mut marker = TracesInflightMarker::new("preclaim-session", "preclaim-attempt", 0);
        marker.ttl_ms = 0;
        marker.oids.push(oid.to_string());
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("write unresolved preclaim marker");

        manager
            .drain_rejected_checkpoint_cleanup_jobs()
            .await
            .expect("retire unresolved preclaim without deleting payload");
        let oid_text = oid.to_string();
        assert!(
            manager
                .repo_path
                .join("objects")
                .join(&oid_text[..2])
                .join(&oid_text[2..])
                .exists(),
            "an unresolved preclaim deleted an object that may belong to another writer"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_preserves_reflog_only_root() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let manager = traces_manager(&dir, db_conn.clone());
        let (candidate, created) =
            write_git_object_with_status(&manager.repo_path, "blob", b"reflog-only root")
                .expect("write reflog candidate");
        assert!(created);
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE reflog (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ref_name TEXT NOT NULL, old_oid TEXT NOT NULL,
                    new_oid TEXT NOT NULL, timestamp INTEGER NOT NULL,
                    committer_name TEXT NOT NULL, committer_email TEXT NOT NULL,
                    action TEXT NOT NULL, message TEXT NOT NULL,
                    worktree_id TEXT
                 )"
                .to_string(),
            ))
            .await
            .expect("create reflog root table");
        db_conn
            .execute(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO reflog (
                    ref_name, old_oid, new_oid, timestamp, committer_name,
                    committer_email, action, message, worktree_id
                 ) VALUES ('HEAD', ?, ?, 0, 'Libra', 'history@libra',
                           'test', 'protect candidate', NULL)",
                [
                    candidate.to_string().into(),
                    "0000000000000000000000000000000000000000".into(),
                ],
            ))
            .await
            .expect("seed reflog-only root");

        let reflog_fence =
            seed_test_writer_fence(&db_conn, "reflog-session", "reflog-attempt").await;
        manager
            .cleanup_rejected_checkpoint_objects(
                &reflog_fence,
                &HashSet::from([candidate.to_string()]),
            )
            .await
            .expect("cleanup with reflog root");
        manager
            .drain_rejected_checkpoint_cleanup_jobs()
            .await
            .expect("drain reflog-root cleanup job");
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list reflog cleanup markers")
                .iter()
                .all(|marker| marker.attempt_id != "reflog-attempt")
        );
        let candidate = candidate.to_string();
        assert!(
            manager
                .repo_path
                .join("objects")
                .join(&candidate[..2])
                .join(&candidate[2..])
                .exists(),
            "reflog-only candidate was deleted"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_preserves_worktree_index_only_root() {
        use git_internal::internal::index::{Index, IndexEntry};

        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let index_fence = seed_test_writer_fence(&db_conn, "index-session", "index-attempt").await;
        let manager = traces_manager(&dir, db_conn.clone());
        let (candidate, created) =
            write_git_object_with_status(&manager.repo_path, "blob", b"index-only root")
                .expect("write index candidate");
        assert!(created);
        let mut index = Index::new();
        index.add(IndexEntry::new_from_blob(
            "staged.txt".to_string(),
            candidate,
            15,
        ));
        index
            .save(manager.repo_path.join("index"))
            .expect("write worktree index");

        manager
            .cleanup_rejected_checkpoint_objects(
                &index_fence,
                &HashSet::from([candidate.to_string()]),
            )
            .await
            .expect("cleanup with index root");
        manager
            .drain_rejected_checkpoint_cleanup_jobs()
            .await
            .expect("drain index-root cleanup job");
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list index cleanup markers")
                .iter()
                .all(|marker| marker.attempt_id != "index-attempt")
        );
        let candidate = candidate.to_string();
        assert!(
            manager
                .repo_path
                .join("objects")
                .join(&candidate[..2])
                .join(&candidate[2..])
                .exists(),
            "index-only candidate was deleted"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_rejects_malformed_durable_object_ids_without_panicking() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        db_conn
            .execute(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO metadata_kv (
                    scope, target, `key`, value, value_type, created_at, updated_at
                 ) VALUES ('agent_traces_inflight', ?, ?, ?, 'text', 0, 0)",
                [
                    "damaged-session".into(),
                    "damaged-attempt".into(),
                    serde_json::json!({
                        "schema_version": 1,
                        "session_id": "damaged-session",
                        "attempt_id": "damaged-attempt",
                        "started_at_ms": 0,
                        "ttl_ms": 0,
                        "oids": ["a"],
                        "cleanup_pending": true,
                    })
                    .to_string()
                    .into(),
                ],
            ))
            .await
            .expect("seed malformed durable cleanup marker");

        let error = traces_manager(&dir, db_conn)
            .drain_rejected_checkpoint_cleanup_jobs()
            .await
            .expect_err("malformed cleanup marker must fail closed");
        let message = format!("{error:#}");
        assert!(message.contains("invalid object id"), "{message}");
        assert!(message.contains("libra agent doctor"), "{message}");
    }

    #[tokio::test]
    async fn expired_empty_marker_is_reaped_without_ref_traversal() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        let marker = TracesInflightMarker::new("expired-session", "expired-attempt", 0);
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("seed expired empty marker");
        let manager = traces_manager(&dir, db_conn.clone());

        assert!(
            manager
                .repair_expired_traces_inflight_marker(
                    "expired-session",
                    "expired-attempt",
                    chrono::Utc::now().timestamp_millis(),
                )
                .await
                .expect("repair expired empty marker")
        );
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list markers after repair")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn corrupt_unrelated_ref_does_not_block_nondestructive_rejected_cleanup() {
        use std::io::Write as _;

        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        let repo_path = dir.path().join(".libra");
        let (candidate, _) = write_git_object_with_status(
            &repo_path,
            "blob",
            b"candidate held while a ref is corrupt",
        )
        .expect("write cleanup candidate");

        let corrupt_oid = git_object_hash("blob", b"expected bytes");
        let corrupt_text = corrupt_oid.to_string();
        let corrupt_path = repo_path
            .join("objects")
            .join(&corrupt_text[..2])
            .join(&corrupt_text[2..]);
        std::fs::create_dir_all(corrupt_path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&corrupt_path).unwrap();
        let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
        encoder.write_all(b"blob 15\0different bytes").unwrap();
        encoder.finish().unwrap();
        db_conn
            .execute(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO reference (name, kind, `commit`, remote, worktree_id)
                 VALUES ('broken-ref', 'Branch', ?, NULL, NULL)",
                [corrupt_text.into()],
            ))
            .await
            .expect("seed corrupt ref");

        let manager = traces_manager(&dir, db_conn.clone());
        let deferred_fence =
            seed_test_writer_fence(&db_conn, "deferred-session", "deferred-attempt").await;
        manager
            .cleanup_rejected_checkpoint_objects(
                &deferred_fence,
                &HashSet::from([candidate.to_string()]),
            )
            .await
            .expect("ordinary append cleanup should defer a corrupt unrelated ref");
        let pending = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list deferred marker");
        assert!(
            pending.iter().any(|marker| {
                marker.attempt_id == "deferred-attempt" && marker.cleanup_pending
            })
        );
        assert!(
            repo_path
                .join("objects")
                .join(&candidate.to_string()[..2])
                .join(&candidate.to_string()[2..])
                .exists(),
            "fail-closed cleanup deleted a candidate"
        );
        manager
            .drain_rejected_checkpoint_cleanup_jobs()
            .await
            .expect("non-destructive marker retirement must not read an unrelated corrupt ref");
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list markers after non-destructive cleanup")
                .is_empty(),
            "cleanup ownership marker survived successful retirement"
        );
        let candidate = candidate.to_string();
        assert!(
            repo_path
                .join("objects")
                .join(&candidate[..2])
                .join(&candidate[2..])
                .exists(),
            "non-destructive cleanup removed a rejected payload"
        );
    }

    struct SlowBoundedStorage;

    #[async_trait::async_trait]
    impl Storage for SlowBoundedStorage {
        async fn get(
            &self,
            _hash: &ObjectHash,
        ) -> std::result::Result<(Vec<u8>, ObjectType), git_internal::errors::GitError> {
            Err(git_internal::errors::GitError::InvalidObjectInfo(
                "unused slow storage read".to_string(),
            ))
        }

        async fn get_with_limit(
            &self,
            _hash: &ObjectHash,
            _limit: u64,
        ) -> std::result::Result<(Vec<u8>, ObjectType), git_internal::errors::GitError> {
            sleep(Duration::from_secs(5)).await;
            Err(git_internal::errors::GitError::InvalidObjectInfo(
                "slow storage read completed unexpectedly".to_string(),
            ))
        }

        async fn put(
            &self,
            hash: &ObjectHash,
            _data: &[u8],
            _obj_type: ObjectType,
        ) -> std::result::Result<String, git_internal::errors::GitError> {
            Ok(hash.to_string())
        }

        async fn exist(&self, _hash: &ObjectHash) -> bool {
            false
        }

        async fn search(&self, _prefix: &str) -> Vec<ObjectHash> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn rejected_reachability_read_is_bounded() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        let manager = traces_manager(&dir, db_conn.clone());
        let root = write_git_object(&manager.repo_path, "blob", &[b'x'; 128])
            .expect("write oversized ref root");
        let error = manager
            .reachable_rejected_objects_with_limit(vec![root], &HashSet::new(), 32)
            .await
            .expect_err("bounded reachability must reject an oversized root");
        assert!(
            format!("{error:#}").contains("exceeds preview limit of 32 bytes"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn rejected_reachability_deadline_interrupts_one_slow_storage_read() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new_with_ref(
            Arc::new(SlowBoundedStorage),
            dir.path().join(".libra"),
            db_conn,
            crate::internal::branch::TRACES_BRANCH,
        );
        let root = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")
            .expect("valid test oid");
        let started = Instant::now();
        let error = manager
            .reachable_rejected_objects_with_limits(
                vec![root],
                &HashSet::new(),
                1024,
                10,
                Instant::now() + Duration::from_millis(25),
            )
            .await
            .expect_err("slow individual read must honor the traversal deadline");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "one storage read escaped the traversal deadline"
        );
        assert!(
            format!("{error:#}").contains("traversal deadline"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejected_reachability_production_client_rejects_fifo_without_blocking() {
        use std::{ffi::CString, io::Write as _, os::unix::ffi::OsStrExt as _};

        use crate::utils::client_storage::ClientStorage;

        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        let objects_dir = repo_path.join("objects");
        let root = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")
            .expect("valid FIFO object id");
        let root_text = root.to_string();
        let shard = objects_dir.join(&root_text[..2]);
        std::fs::create_dir_all(&shard).expect("create FIFO object shard");
        let fifo = shard.join(&root_text[2..]);
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path has no NUL");
        // SAFETY: fifo_name is NUL-terminated and points to a path owned by
        // this test's temporary directory.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

        // Release the intentionally blocked local read after the deadline
        // assertion. This lets Tokio's cancelled spawn_blocking task finish so
        // the test runtime can shut down cleanly.
        let release_fifo = fifo.clone();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            let mut writer = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(release_fifo)
                .expect("open self-held FIFO writer after reader blocks or rejects the FIFO");
            writer.write_all(b"not-zlib").expect("release FIFO reader");
        });
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new_with_ref(
            Arc::new(ClientStorage::init(objects_dir)),
            repo_path,
            db_conn,
            crate::internal::branch::TRACES_BRANCH,
        );
        let started = Instant::now();
        let error = manager
            .reachable_rejected_objects_with_limits(
                vec![root],
                &HashSet::new(),
                1024,
                10,
                Instant::now() + Duration::from_millis(25),
            )
            .await
            .expect_err("production ClientStorage must reject a non-regular loose object");
        let elapsed = started.elapsed();
        release.join().expect("join FIFO release writer");
        assert!(
            elapsed < Duration::from_millis(150),
            "ClientStorage blocked while rejecting a FIFO for {elapsed:?}"
        );
        assert!(
            format!("{error:#}").contains("is not a regular file"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn rejected_reachability_reads_bounded_objects_from_alternates() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        let objects_dir = repo_path.join("objects");
        std::fs::create_dir_all(objects_dir.join("info")).unwrap();

        let alternate_repo = dir.path().join("alternate");
        std::fs::create_dir_all(&alternate_repo).unwrap();
        let root = write_git_object(&alternate_repo, "blob", b"alternate root")
            .expect("write alternate-only ref root");
        let alternate_objects = alternate_repo.join("objects");
        std::fs::write(
            objects_dir.join("info/alternates"),
            format!("{}\n", alternate_objects.display()),
        )
        .expect("configure alternate object store");

        let storage = Arc::new(LocalStorage::new_with_alternates(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new_with_ref(
            storage,
            repo_path,
            db_conn,
            crate::internal::branch::TRACES_BRANCH,
        );
        let reachable = manager
            .reachable_rejected_objects_with_limit(
                vec![root],
                &HashSet::from([root.to_string()]),
                1024,
            )
            .await
            .expect("bounded alternate read should prove reachability");
        assert_eq!(reachable, HashSet::from([root.to_string()]));
    }

    #[tokio::test]
    async fn rejected_reachability_total_work_is_bounded() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        let manager = traces_manager(&dir, db_conn.clone());
        let first = write_git_object(&manager.repo_path, "blob", b"first").unwrap();
        let second = write_git_object(&manager.repo_path, "blob", b"second").unwrap();

        let count_error = manager
            .reachable_rejected_objects_with_limits(
                vec![first, second],
                &HashSet::new(),
                1024,
                1,
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect_err("visited-object cap must fail closed");
        assert!(
            format!("{count_error:#}").contains("1 object traversal limit"),
            "unexpected error: {count_error:#}"
        );

        let deadline_error = manager
            .reachable_rejected_objects_with_limits(
                vec![first],
                &HashSet::new(),
                1024,
                10,
                Instant::now(),
            )
            .await
            .expect_err("expired traversal deadline must fail closed");
        assert!(
            format!("{deadline_error:#}").contains("traversal deadline"),
            "unexpected error: {deadline_error:#}"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_preserves_candidates_reachable_only_from_annotated_tag() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        let repo_path = dir.path().join(".libra");
        let (candidate, created) = write_git_object_with_status(
            &repo_path,
            "blob",
            b"candidate preserved by annotated tag",
        )
        .expect("write tagged cleanup candidate");
        assert!(created);
        let tag_data = format!(
            "object {candidate}\ntype blob\ntag keep-candidate\ntagger Libra <history@libra> 0 +0000\n\nkeep\n"
        );
        let tag = write_git_object(&repo_path, "tag", tag_data.as_bytes())
            .expect("write annotated tag object");
        db_conn
            .execute(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO reference (name, kind, `commit`, remote, worktree_id)
                 VALUES ('keep-candidate', 'Tag', ?, NULL, NULL)",
                [tag.to_string().into()],
            ))
            .await
            .expect("seed annotated tag ref");

        let tag_fence = seed_test_writer_fence(&db_conn, "tag-session", "tag-attempt").await;
        let manager = traces_manager(&dir, db_conn.clone());
        manager
            .cleanup_rejected_checkpoint_objects(
                &tag_fence,
                &HashSet::from([candidate.to_string()]),
            )
            .await
            .expect("annotated tag reachability should protect candidate");
        manager
            .drain_rejected_checkpoint_cleanup_jobs()
            .await
            .expect("drain annotated-tag cleanup job");
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list annotated-tag cleanup markers")
                .iter()
                .all(|marker| marker.attempt_id != "tag-attempt")
        );
        let candidate_string = candidate.to_string();
        assert!(
            repo_path
                .join("objects")
                .join(&candidate_string[..2])
                .join(&candidate_string[2..])
                .exists(),
            "candidate reachable only through an annotated tag was deleted"
        );
    }
}
