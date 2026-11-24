//! Repository-local recovery evidence for commit links without a trailer.
//! Callers hold the repository SQLite writer transaction while appending or
//! compacting, so erasure and ref updates cannot overtake one another.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::internal::ai::capture_scope::CaptureScope;

const JOURNAL_FILE: &str = "agent-commit-links.v1";
const MAX_JOURNAL_BYTES: u64 = 10 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 32 * 1024;

#[derive(Debug, thiserror::Error)]
#[error(
    "agent commit journal has an unsupported record schema; use a compatible Libra version to inspect it"
)]
struct UnsupportedJournalSchema;

/// Body-free category for doctor diagnostics and explicit reset authorization.
pub fn read_issue(storage_root: &Path) -> Option<&'static str> {
    let path = journal_path(storage_root);
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some("io"),
    };
    if !meta.file_type().is_file() || meta.file_type().is_symlink() {
        return Some("unsafe_entry");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Some("permissions");
        }
    }
    if meta.len() > MAX_JOURNAL_BYTES {
        return Some("oversized");
    }
    match read(storage_root) {
        Ok(_) => None,
        Err(error)
            if error
                .chain()
                .any(|cause| cause.is::<UnsupportedJournalSchema>()) =>
        {
            Some("future_schema")
        }
        Err(error) if error.chain().any(|cause| cause.is::<std::io::Error>()) => Some("io"),
        Err(_) => Some("corrupt"),
    }
}

/// Conservative capacity pressure used only to decide whether a requested
/// reset remains necessary after doctor has replayed and compacted evidence.
pub fn near_capacity(storage_root: &Path) -> bool {
    fs::metadata(journal_path(storage_root)).is_ok_and(|meta| {
        meta.len() >= MAX_JOURNAL_BYTES.saturating_sub(MAX_RECORD_BYTES as u64 + 128)
    })
}

/// A reset requested on a full but readable journal may run after doctor
/// replay. Refuse it if another writer added evidence while the doctor was
/// scanning or between its repair and cleanup transactions.
pub fn verify_no_new_records(storage_root: &Path, initial: &[CommitJournalRecord]) -> Result<()> {
    let (current, bad_tail) = read(storage_root)?;
    if bad_tail {
        bail!(
            "agent commit journal changed while doctor was repairing it; retry without resetting"
        );
    }
    let mut counts = HashMap::<Vec<u8>, usize>::new();
    for record in initial {
        *counts.entry(serde_json::to_vec(record)?).or_default() += 1;
    }
    for record in current {
        let key = serde_json::to_vec(&record)?;
        let Some(count) = counts.get_mut(&key) else {
            bail!(
                "agent commit journal gained new evidence during repair; retry doctor without resetting it"
            );
        };
        if *count == 0 {
            bail!(
                "agent commit journal gained new evidence during repair; retry doctor without resetting it"
            );
        }
        *count -= 1;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitJournalStage {
    Prepared,
    Linked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitJournalRecord {
    pub version: u8,
    pub stage: CommitJournalStage,
    pub scope: CaptureScope,
    pub hash_kind: String,
    pub commit_oid: String,
    pub session_ids: Vec<String>,
}

impl CommitJournalRecord {
    pub fn prepared(
        scope: CaptureScope,
        hash_kind: &str,
        commit_oid: &str,
        session_ids: &[String],
    ) -> Self {
        Self {
            version: 1,
            stage: CommitJournalStage::Prepared,
            scope,
            hash_kind: hash_kind.to_string(),
            commit_oid: commit_oid.to_string(),
            session_ids: session_ids.to_vec(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.session_ids.is_empty() || self.session_ids.len() > 64 {
            bail!("unsupported or oversized agent commit journal record");
        }
        let hex_len = match self.hash_kind.as_str() {
            "sha1" => 40,
            "sha256" | "blake3" => 64,
            _ => bail!("unsupported agent commit journal hash kind"),
        };
        if self.commit_oid.len() != hex_len
            || !self.commit_oid.bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("invalid agent commit journal object ID");
        }
        if self.scope.repo_id.is_empty()
            || self.scope.repo_id.len() > 256
            || self.scope.worktree_id.len() > 256
            || self
                .scope
                .workspace_id
                .as_ref()
                .is_some_and(|id| id.len() > 256)
            || self.session_ids.iter().any(|id| {
                id.is_empty()
                    || id.len() > 256
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
            })
        {
            bail!("invalid agent commit journal scope or session ID");
        }
        Ok(())
    }
}

pub fn journal_path(storage_root: &Path) -> PathBuf {
    storage_root.join(JOURNAL_FILE)
}

fn open_journal(storage_root: &Path, create: bool, write: bool) -> Result<Option<(File, bool)>> {
    let path = journal_path(storage_root);
    let mut absent = false;
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            if !meta.file_type().is_file() || meta.file_type().is_symlink() {
                bail!(
                    "agent commit journal is not a regular file: {}",
                    path.display()
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o077 != 0 {
                    bail!(
                        "agent commit journal permissions are too broad: {}",
                        path.display()
                    );
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Ok(None);
            }
            absent = true;
        }
        Err(error) => return Err(error).context("inspect agent commit journal"),
    }
    #[cfg(windows)]
    if absent {
        // Windows has no portable directory fsync. A write-through atomic
        // move makes the new directory entry durable before a ref can move.
        crate::utils::atomic_stream::StreamingAtomicFile::new_in(storage_root, true)
            .context("stage initial agent commit journal on Windows")?
            .persist(&path)
            .context("durably publish initial agent commit journal on Windows")?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(windows)]
    let create_new = false;
    #[cfg(not(windows))]
    let create_new = create;
    let (file, created) = match options.create_new(create_new).open(&path) {
        Ok(file) => (file, absent),
        Err(error) if create_new && error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = options
                .create_new(false)
                .open(&path)
                .with_context(|| format!("open agent commit journal {}", path.display()))?;
            (file, false)
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open agent commit journal {}", path.display()));
        }
    };
    let opened_type = file.metadata()?.file_type();
    if !opened_type.is_file() || opened_type.is_symlink() {
        bail!(
            "agent commit journal is not a regular file: {}",
            path.display()
        );
    }
    Ok(Some((file, created)))
}

fn sync_directory(storage_root: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        // Initial creation and compaction use the write-through atomic move
        // above/below; Windows has no reliable directory sync equivalent.
        let _ = storage_root;
        return Ok(());
    }
    #[cfg(not(windows))]
    File::open(storage_root)
        .with_context(|| {
            format!(
                "open agent commit journal directory {}",
                storage_root.display()
            )
        })?
        .sync_all()
        .context("sync agent commit journal directory")
}

fn parse_records(bytes: &[u8]) -> Result<(Vec<CommitJournalRecord>, usize, bool)> {
    let mut records = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let Some(header_end) = bytes[pos..].iter().position(|byte| *byte == b'\n') else {
            let tail = &bytes[pos..];
            if tail.len() > 100 || !(b"LJ1 ".starts_with(tail) || tail.starts_with(b"LJ1 ")) {
                bail!(
                    "agent commit journal has a malformed final frame; preserve the file for inspection"
                );
            }
            break;
        };
        if header_end > 100 {
            bail!(
                "agent commit journal has a malformed record header; preserve the file for inspection"
            );
        }
        let header = &bytes[pos..pos + header_end];
        let Ok(header) = std::str::from_utf8(header) else {
            bail!(
                "agent commit journal has a non-UTF8 record header; preserve the file for inspection"
            );
        };
        let mut fields = header.split(' ');
        if fields.next() != Some("LJ1") {
            bail!(
                "agent commit journal has an unknown record header; preserve the file for inspection"
            );
        }
        let Some(size) = fields.next().and_then(|size| size.parse::<usize>().ok()) else {
            bail!(
                "agent commit journal has an invalid record length; preserve the file for inspection"
            );
        };
        let Some(checksum) = fields.next() else {
            bail!("agent commit journal has no record checksum; preserve the file for inspection")
        };
        if fields.next().is_some() || size > MAX_RECORD_BYTES || checksum.len() != 64 {
            bail!(
                "agent commit journal has a malformed record frame; preserve the file for inspection"
            );
        }
        let start = pos + header_end + 1;
        let Some(end) = start.checked_add(size) else {
            break;
        };
        if end >= bytes.len() {
            if bytes[start..].windows(5).any(|window| window == b"\nLJ1 ") {
                bail!(
                    "agent commit journal has corruption before a later record; preserve the file for inspection"
                );
            }
            break;
        }
        if bytes[end] != b'\n' {
            bail!(
                "agent commit journal has a corrupt record boundary; preserve the file for inspection"
            );
        }
        let payload = &bytes[start..end];
        if hex::encode(Sha256::digest(payload)) != checksum {
            bail!(
                "agent commit journal has a corrupt record checksum; preserve the file for inspection"
            );
        }
        // A checksum-valid but unknown schema is not a torn write. Never
        // truncate it: an older binary must not erase a newer journal record.
        let record: CommitJournalRecord =
            serde_json::from_slice(payload).map_err(|_| UnsupportedJournalSchema)?;
        record.validate().map_err(|_| UnsupportedJournalSchema)?;
        records.push(record);
        pos = end + 1;
    }
    let bad_tail = pos != bytes.len();
    Ok((records, pos, bad_tail))
}

fn bounded_contents(file: &mut File) -> Result<Vec<u8>> {
    let size = file.metadata()?.len();
    if size > MAX_JOURNAL_BYTES {
        bail!(
            "agent commit journal exceeds 10 MiB; inspect it, then explicitly run 'libra agent doctor --repair --reset-commit-link-journal' if recovery evidence must be discarded"
        );
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("read bounded agent commit journal")?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        bail!("agent commit journal grew beyond 10 MiB while reading");
    }
    Ok(bytes)
}

/// Read only checksummed, schema-valid records. A torn tail is returned to
/// doctor and never interpreted as an instruction or a session identity.
pub fn read(storage_root: &Path) -> Result<(Vec<CommitJournalRecord>, bool)> {
    let Some((mut file, _)) = open_journal(storage_root, false, false)? else {
        return Ok((Vec::new(), false));
    };
    let bytes = bounded_contents(&mut file)?;
    let (records, _, bad_tail) = parse_records(&bytes)?;
    Ok((records, bad_tail))
}

/// Drop only a torn, non-checksummed tail during explicit repair. The caller
/// holds the repository writer lock; a checksum-valid unknown schema returns
/// an error without changing any bytes.
pub fn repair_torn_tail(storage_root: &Path) -> Result<bool> {
    let Some((mut file, _)) = open_journal(storage_root, false, true)? else {
        return Ok(false);
    };
    let bytes = bounded_contents(&mut file)?;
    let (_, valid_len, bad_tail) = parse_records(&bytes)?;
    if bad_tail {
        file.set_len(valid_len as u64)
            .context("truncate torn agent commit journal tail")?;
        file.sync_data()
            .context("sync repaired agent commit journal tail")?;
    }
    Ok(bad_tail)
}

/// Append under the caller's SQLite writer lock before the ref transaction
/// moves HEAD. A partial tail from a crashed write is truncated to its last
/// fully checksummed record before appending.
pub fn append(storage_root: &Path, record: &CommitJournalRecord) -> Result<()> {
    append_inner(storage_root, record, AppendFault::None)
}

#[derive(Clone, Copy)]
enum AppendFault {
    None,
    #[cfg(all(test, feature = "test-live-agent"))]
    BeforeWrite,
    #[cfg(all(test, feature = "test-live-agent"))]
    BeforeFileSync,
    #[cfg(all(test, feature = "test-live-agent"))]
    BeforeDirectorySync,
}

fn append_inner(
    storage_root: &Path,
    record: &CommitJournalRecord,
    fault: AppendFault,
) -> Result<()> {
    record.validate()?;
    let Some((mut file, _created)) = open_journal(storage_root, true, true)? else {
        bail!("agent commit journal could not be created");
    };
    let bytes = bounded_contents(&mut file)?;
    let (_, mut valid_len, bad_tail) = parse_records(&bytes)?;
    if bad_tail {
        file.set_len(valid_len as u64)
            .context("truncate torn agent commit journal tail")?;
        file.sync_data()
            .context("sync repaired agent commit journal tail")?;
    }
    let payload = serde_json::to_vec(record).context("encode agent commit journal record")?;
    if payload.len() > MAX_RECORD_BYTES {
        bail!("agent commit journal record exceeds 32 KiB");
    }
    let header = format!(
        "LJ1 {} {}\n",
        payload.len(),
        hex::encode(Sha256::digest(&payload))
    );
    let total = header.len() + payload.len() + 1;
    if (valid_len as u64).saturating_add(total as u64) > MAX_JOURNAL_BYTES {
        drop(file);
        compact_completed(storage_root)
            .context("reclaim completed agent commit journal records")?;
        let Some((reopened, _)) = open_journal(storage_root, true, true)? else {
            bail!("agent commit journal disappeared during compaction");
        };
        file = reopened;
        let bytes = bounded_contents(&mut file)?;
        let (_, current_len, bad_tail) = parse_records(&bytes)?;
        if bad_tail {
            bail!("agent commit journal acquired an incomplete tail during compaction");
        }
        valid_len = current_len;
    }
    if (valid_len as u64).saturating_add(total as u64) > MAX_JOURNAL_BYTES {
        bail!(
            "agent commit journal is full of unresolved intents (10 MiB); try 'libra agent doctor --repair', or explicitly reset with '--reset-commit-link-journal' after inspecting the evidence"
        );
    }
    #[cfg(all(test, feature = "test-live-agent"))]
    if matches!(fault, AppendFault::BeforeWrite) {
        bail!("injected agent commit journal write failure");
    }
    #[cfg(not(all(test, feature = "test-live-agent")))]
    let _ = fault;
    file.seek(SeekFrom::End(0))?;
    file.write_all(header.as_bytes())
        .context("write agent commit journal header")?;
    file.write_all(&payload)
        .context("write agent commit journal intent")?;
    file.write_all(b"\n")
        .context("finish agent commit journal intent")?;
    #[cfg(all(test, feature = "test-live-agent"))]
    if matches!(fault, AppendFault::BeforeFileSync) {
        bail!("injected agent commit journal file sync failure");
    }
    file.sync_data()
        .context("sync agent commit journal intent")?;
    // Sync on every append. A retry after a failed directory sync cannot
    // reliably tell whether an existing entry reached durable storage.
    #[cfg(all(test, feature = "test-live-agent"))]
    if matches!(fault, AppendFault::BeforeDirectorySync) {
        bail!("injected agent commit journal directory sync failure");
    }
    sync_directory(storage_root)?;
    Ok(())
}

/// Remove an erased session's local identity evidence. The caller must first
/// commit the anti-resurrection tombstone, then hold the SQLite writer lock.
pub fn compact_erased(storage_root: &Path, session_id: &str) -> Result<()> {
    let Some((mut file, _)) = open_journal(storage_root, false, true)? else {
        return Ok(());
    };
    let bytes = bounded_contents(&mut file)?;
    let (mut records, _, _) = parse_records(&bytes)?;
    records.retain_mut(|record| {
        record.session_ids.retain(|id| id != session_id);
        !record.session_ids.is_empty()
    });
    drop(file);
    rewrite_records(storage_root, &records)
}

/// Reclaim records whose post-ref `Linked` stage proves the local DAO write
/// completed. Unresolved `Prepared` records remain available to doctor.
/// The caller holds the same SQLite writer lock as journal append and erase.
pub fn compact_completed(storage_root: &Path) -> Result<usize> {
    let Some((mut file, _)) = open_journal(storage_root, false, true)? else {
        return Ok(0);
    };
    let bytes = bounded_contents(&mut file)?;
    let (records, _, bad_tail) = parse_records(&bytes)?;
    if bad_tail {
        bail!("repair the torn agent commit journal tail before compacting completed records");
    }
    let completed: HashSet<Vec<u8>> = records
        .iter()
        .filter(|record| record.stage == CommitJournalStage::Linked)
        .map(|record| {
            let mut key = record.clone();
            key.stage = CommitJournalStage::Prepared;
            serde_json::to_vec(&key)
        })
        .collect::<Result<_, _>>()?;
    if completed.is_empty() {
        return Ok(0);
    }
    let mut retained = Vec::new();
    for record in records {
        let mut key = record.clone();
        key.stage = CommitJournalStage::Prepared;
        if !completed.contains(&serde_json::to_vec(&key)?) {
            retained.push(record);
        }
    }
    let removed = completed.len();
    drop(file);
    rewrite_records(storage_root, &retained)?;
    Ok(removed)
}

/// Doctor passes only records whose commit is reachable and whose every
/// session has a currently authorized, durable DAO link. This is the bounded
/// recovery path for a journal filled with unresolved prepared intents.
/// The caller holds the repository SQLite writer transaction.
pub fn compact_reconciled(
    storage_root: &Path,
    reconciled: &[CommitJournalRecord],
) -> Result<usize> {
    if reconciled.is_empty() {
        return Ok(0);
    }
    let Some((mut file, _)) = open_journal(storage_root, false, true)? else {
        return Ok(0);
    };
    let bytes = bounded_contents(&mut file)?;
    let (records, _, bad_tail) = parse_records(&bytes)?;
    if bad_tail {
        bail!("repair the torn agent commit journal tail before reclaiming recovered links");
    }
    let keys: HashSet<Vec<u8>> = reconciled
        .iter()
        .map(|record| {
            let mut key = record.clone();
            key.stage = CommitJournalStage::Prepared;
            serde_json::to_vec(&key)
        })
        .collect::<Result<_, _>>()?;
    let mut retained = Vec::new();
    let mut removed = 0;
    for record in records {
        let mut key = record.clone();
        key.stage = CommitJournalStage::Prepared;
        if keys.contains(&serde_json::to_vec(&key)?) {
            removed += 1;
        } else {
            retained.push(record);
        }
    }
    if removed > 0 {
        drop(file);
        rewrite_records(storage_root, &retained)?;
    }
    Ok(removed)
}

/// Explicit operator recovery for an unparseable or permanently full local
/// journal. This discards only local commit-link evidence, never checkpoint
/// content, commits, or catalog rows. The caller holds the SQLite writer lock
/// and must present a separate user-facing flag for this destructive choice.
pub fn reset_explicit(storage_root: &Path) -> Result<bool> {
    let path = journal_path(storage_root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("inspect agent commit journal before reset"),
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!(
            "agent commit journal is not a regular file; remove the unsafe entry manually after inspection"
        );
    }
    // A healthy, replayable journal is not disposable merely because the
    // operator supplied the reset flag. Reclaim completed pairs first; keep
    // unresolved intents unless the remaining file is near the hard cap.
    let issue = read_issue(storage_root);
    if issue.is_none() {
        if metadata.len() < MAX_JOURNAL_BYTES.saturating_sub(MAX_RECORD_BYTES as u64 + 128) {
            bail!(
                "agent commit journal is readable and has capacity; run 'libra agent doctor --repair' to replay it before considering an explicit reset"
            );
        }
        compact_completed(storage_root)?;
        let remaining = fs::metadata(&path)
            .context("inspect agent commit journal size after compaction")?
            .len();
        if remaining < MAX_JOURNAL_BYTES.saturating_sub(MAX_RECORD_BYTES as u64 + 128) {
            return Ok(false);
        }
    } else if !matches!(issue, Some("corrupt" | "oversized")) {
        bail!(
            "agent commit journal reset refused for {} evidence; restore safe permissions or use a compatible Libra version before retrying",
            issue.unwrap_or("unknown")
        );
    }
    #[cfg(unix)]
    let replacement = {
        use std::os::unix::fs::PermissionsExt;
        crate::utils::atomic_stream::StreamingAtomicFile::new_in_with_permissions(
            storage_root,
            true,
            fs::Permissions::from_mode(0o600),
        )
        .context("create empty agent commit journal replacement")?
    };
    #[cfg(not(unix))]
    let replacement = crate::utils::atomic_stream::StreamingAtomicFile::new_in(storage_root, true)
        .context("create empty agent commit journal replacement")?;
    replacement
        .persist(&path)
        .context("durably reset local agent commit journal")?;
    Ok(true)
}

fn rewrite_records(storage_root: &Path, records: &[CommitJournalRecord]) -> Result<()> {
    let path = journal_path(storage_root);
    #[cfg(unix)]
    let mut replacement = {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o600);
        crate::utils::atomic_stream::StreamingAtomicFile::new_in_with_permissions(
            storage_root,
            true,
            permissions,
        )
        .context("create agent commit journal replacement")?
    };
    #[cfg(not(unix))]
    let mut replacement =
        crate::utils::atomic_stream::StreamingAtomicFile::new_in(storage_root, true)
            .context("create agent commit journal replacement")?;
    for record in records {
        let payload = serde_json::to_vec(record)?;
        let header = format!(
            "LJ1 {} {}\n",
            payload.len(),
            hex::encode(Sha256::digest(&payload))
        );
        replacement.write_all(header.as_bytes())?;
        replacement.write_all(&payload)?;
        replacement.write_all(b"\n")?;
    }
    replacement
        .persist(&path)
        .context("durably replace agent commit journal after erasure")
}

#[cfg(all(test, feature = "test-live-agent"))]
mod tests {
    use super::*;

    fn record(session_id: &str, oid: char) -> CommitJournalRecord {
        CommitJournalRecord::prepared(
            CaptureScope {
                repo_id: "repository".into(),
                worktree_id: String::new(),
                workspace_id: None,
                workspace_fence: None,
            },
            "sha1",
            &oid.to_string().repeat(40),
            &[session_id.to_string()],
        )
    }

    #[test]
    #[ignore = "local-only Codex journal fault matrix; never run in CI"]
    fn agent_journal_write_sync_and_directory_sync_failures_preserve_valid_prefix() {
        for fault in [
            AppendFault::BeforeWrite,
            AppendFault::BeforeFileSync,
            AppendFault::BeforeDirectorySync,
        ] {
            let dir = tempfile::tempdir().expect("journal temp directory");
            append_inner(dir.path(), &record("codex_first", 'a'), AppendFault::None)
                .expect("write valid prefix");
            let original = read(dir.path()).expect("read valid prefix").0;
            let faulty_root = if matches!(fault, AppendFault::BeforeDirectorySync) {
                tempfile::tempdir().expect("new journal directory")
            } else {
                tempfile::tempdir().expect("unused journal directory")
            };
            let target = if matches!(fault, AppendFault::BeforeDirectorySync) {
                faulty_root.path()
            } else {
                dir.path()
            };
            assert!(append_inner(target, &record("codex_second", 'b'), fault).is_err());
            let retained = read(dir.path()).expect("read retained prefix").0;
            assert!(retained.len() >= original.len());
            assert_eq!(retained[0].commit_oid, original[0].commit_oid);
            append(target, &record("codex_retried", 'c')).expect("retry after injected failure");
            let (recovered, bad_tail) = read(target).expect("read recovered journal");
            assert!(!bad_tail);
            assert!(
                recovered.iter().any(|record| record.session_ids.len() == 1
                    && record.session_ids[0] == "codex_retried")
            );
        }
    }

    #[test]
    #[ignore = "local-only Codex journal corruption and erase matrix; never run in CI"]
    fn agent_journal_bad_tail_limit_and_erase_compaction() {
        let dir = tempfile::tempdir().expect("journal temp directory");
        append(dir.path(), &record("codex_erased", 'a')).expect("first intent");
        append(dir.path(), &record("codex_kept", 'b')).expect("second intent");
        let path = journal_path(dir.path());
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open tail")
            .write_all(b"LJ1 123 ")
            .expect("append damaged tail");
        let (prefix, bad_tail) = read(dir.path()).expect("read valid prefix");
        assert!(bad_tail);
        assert_eq!(prefix.len(), 2);
        append(dir.path(), &record("codex_third", 'c')).expect("repair tail on append");
        compact_erased(dir.path(), "codex_erased").expect("compact after tombstone");
        let (remaining, bad_tail) = read(dir.path()).expect("read compacted journal");
        assert!(!bad_tail);
        assert_eq!(remaining.len(), 2);
        assert!(
            remaining
                .iter()
                .all(|record| !record.session_ids.iter().any(|id| id == "codex_erased"))
        );
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open second tail")
            .write_all(b"LJ1 123 ")
            .expect("append second torn tail");
        assert!(repair_torn_tail(dir.path()).expect("explicitly repair torn tail"));
        assert_eq!(read(dir.path()).expect("read repaired prefix").0.len(), 2);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open journal for malformed tail")
            .write_all(b"garbage")
            .expect("append malformed tail");
        let malformed = fs::read(&path).expect("read malformed tail fixture");
        assert!(read(dir.path()).is_err());
        assert!(repair_torn_tail(dir.path()).is_err());
        assert_eq!(fs::read(&path).expect("preserve malformed tail"), malformed);
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open journal for size limit");
        file.set_len(MAX_JOURNAL_BYTES + 1)
            .expect("grow beyond cap");
        assert!(read(dir.path()).is_err());
        assert!(append(dir.path(), &record("codex_blocked", 'd')).is_err());
    }

    #[test]
    #[ignore = "local-only Codex journal corruption matrix; never run in CI"]
    fn agent_journal_middle_corruption_preserves_every_byte() {
        let dir = tempfile::tempdir().expect("journal temp directory");
        for (session, oid) in [("first", 'a'), ("middle", 'b'), ("last", 'c')] {
            append(dir.path(), &record(session, oid)).expect("append journal record");
        }
        let path = journal_path(dir.path());
        let mut bytes = fs::read(&path).expect("read journal bytes");
        let middle = bytes
            .windows(b"middle".len())
            .position(|window| window == b"middle")
            .expect("middle record payload");
        bytes[middle] = b'X';
        fs::write(&path, &bytes).expect("corrupt middle checksum");
        assert!(read(dir.path()).is_err());
        assert!(append(dir.path(), &record("fourth", 'd')).is_err());
        assert!(repair_torn_tail(dir.path()).is_err());
        assert!(compact_erased(dir.path(), "first").is_err());
        assert_eq!(fs::read(&path).expect("read intact corrupt file"), bytes);
    }

    #[test]
    #[ignore = "local-only explicit Codex journal reset; never run in CI"]
    fn agent_journal_explicit_reset_recovers_corrupt_or_oversized_regular_file() {
        let dir = tempfile::tempdir().expect("journal temp directory");
        let path = journal_path(dir.path());
        fs::write(&path, b"unparseable session identity").expect("seed corrupt journal");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .expect("secure corrupt journal fixture");
        }
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("grow corrupt journal");
        file.set_len(MAX_JOURNAL_BYTES + 1)
            .expect("grow beyond journal cap");
        assert!(read(dir.path()).is_err());
        assert!(reset_explicit(dir.path()).expect("reset explicit journal"));
        assert_eq!(fs::read(&path).expect("read reset journal"), b"");
        append(dir.path(), &record("recovered", 'a')).expect("append after reset");
        assert_eq!(read(dir.path()).expect("read new journal").0.len(), 1);
    }

    #[test]
    #[ignore = "local-only Codex journal reset refusal; never run in CI"]
    fn agent_journal_reset_refuses_future_schema_and_unsafe_permissions() {
        let dir = tempfile::tempdir().expect("journal temp directory");
        let path = journal_path(dir.path());
        let mut future = serde_json::to_value(record("future", 'a')).expect("encode fixture");
        future["version"] = serde_json::json!(2);
        let payload = serde_json::to_vec(&future).expect("encode future record");
        let frame = format!(
            "LJ1 {} {}\n",
            payload.len(),
            hex::encode(Sha256::digest(&payload))
        );
        let bytes = [frame.as_bytes(), &payload, b"\n"].concat();
        fs::write(&path, &bytes).expect("write future record");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .expect("secure future journal fixture");
        }
        assert_eq!(read_issue(dir.path()), Some("future_schema"));
        assert!(reset_explicit(dir.path()).is_err());
        assert_eq!(fs::read(&path).expect("preserve future record"), bytes);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
                .expect("widen journal permissions");
            assert_eq!(read_issue(dir.path()), Some("permissions"));
            assert!(reset_explicit(dir.path()).is_err());
            assert_eq!(fs::read(&path).expect("preserve unsafe journal"), bytes);
        }
    }

    #[test]
    #[ignore = "local-only Codex journal concurrent-reset fence; never run in CI"]
    fn agent_journal_reset_refuses_records_added_after_inspection() {
        let dir = tempfile::tempdir().expect("journal temp directory");
        append(dir.path(), &record("first", 'a')).expect("append initial intent");
        let initial = read(dir.path()).expect("read initial intents").0;
        append(dir.path(), &record("later", 'b')).expect("append concurrent intent");
        assert!(verify_no_new_records(dir.path(), &initial).is_err());
        compact_erased(dir.path(), "later").expect("remove later intent");
        assert!(verify_no_new_records(dir.path(), &initial).is_ok());
    }

    #[test]
    #[ignore = "local-only Codex journal capacity recovery; never run in CI"]
    fn agent_journal_reclaims_completed_pairs_before_capacity_failure() {
        let dir = tempfile::tempdir().expect("journal temp directory");
        let mut prepared = record("first", 'a');
        prepared.session_ids = (0..64)
            .map(|index| format!("s{index:03}_{}", "a".repeat(115)))
            .collect();
        let mut linked = prepared.clone();
        linked.stage = CommitJournalStage::Linked;
        let frame = |record: &CommitJournalRecord| {
            let payload = serde_json::to_vec(record).expect("encode frame");
            format!(
                "LJ1 {} {}\n",
                payload.len(),
                hex::encode(Sha256::digest(&payload))
            )
            .into_bytes()
            .into_iter()
            .chain(payload)
            .chain(*b"\n")
            .collect::<Vec<_>>()
        };
        let pair = [frame(&prepared), frame(&linked)].concat();
        let incoming_record = CommitJournalRecord::prepared(
            prepared.scope.clone(),
            "sha1",
            &"b".repeat(40),
            &prepared.session_ids,
        );
        let incoming = frame(&incoming_record);
        let mut bytes = Vec::new();
        while bytes.len() + pair.len() + incoming.len() <= MAX_JOURNAL_BYTES as usize {
            bytes.extend_from_slice(&pair);
        }
        let mut small_linked = record("small", 'c');
        small_linked.stage = CommitJournalStage::Linked;
        let small_pair = [frame(&record("small", 'c')), frame(&small_linked)].concat();
        while bytes.len() + small_pair.len() + incoming.len() <= MAX_JOURNAL_BYTES as usize {
            bytes.extend_from_slice(&small_pair);
        }
        assert!(bytes.len() + incoming.len() <= MAX_JOURNAL_BYTES as usize);
        bytes.extend_from_slice(&small_pair);
        assert!(bytes.len() <= MAX_JOURNAL_BYTES as usize);
        assert!(bytes.len() + incoming.len() > MAX_JOURNAL_BYTES as usize);
        fs::write(journal_path(dir.path()), bytes).expect("seed bounded journal");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(journal_path(dir.path()), fs::Permissions::from_mode(0o600))
                .expect("secure seeded journal");
        }
        append(dir.path(), &incoming_record).expect("reclaim completed pairs");
        let (records, bad_tail) = read(dir.path()).expect("read reclaimed journal");
        assert!(!bad_tail);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_ids, incoming_record.session_ids);
    }

    #[test]
    #[ignore = "local-only Codex journal forward-schema refusal; never run in CI"]
    fn agent_journal_valid_checksum_future_schema_is_never_truncated() {
        let dir = tempfile::tempdir().expect("journal temp directory");
        append(dir.path(), &record("codex_existing", 'a')).expect("existing intent");
        let path = journal_path(dir.path());
        let mut future =
            serde_json::to_value(record("codex_future", 'b')).expect("encode future record");
        future["version"] = serde_json::json!(2);
        let payload = serde_json::to_vec(&future).expect("encode future payload");
        let header = format!(
            "LJ1 {} {}\n",
            payload.len(),
            hex::encode(Sha256::digest(&payload))
        );
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open journal");
        file.write_all(header.as_bytes())
            .expect("write future header");
        file.write_all(&payload).expect("write future payload");
        file.write_all(b"\n").expect("finish future record");
        file.sync_data().expect("sync future record");
        let before = fs::read(&path).expect("read future journal bytes");
        assert!(read(dir.path()).is_err());
        assert!(append(dir.path(), &record("codex_new", 'c')).is_err());
        assert_eq!(fs::read(&path).expect("read preserved journal"), before);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "local-only Codex journal symlink refusal; never run in CI"]
    fn agent_journal_refuses_symlink_without_writing_target() {
        let dir = tempfile::tempdir().expect("journal temp directory");
        let target = dir.path().join("target");
        fs::write(&target, b"private").expect("write target");
        std::os::unix::fs::symlink(&target, journal_path(dir.path())).expect("create symlink");
        assert!(append(dir.path(), &record("codex_link", 'a')).is_err());
        assert!(read(dir.path()).is_err());
        assert_eq!(fs::read(&target).expect("read target"), b"private");
    }
}
