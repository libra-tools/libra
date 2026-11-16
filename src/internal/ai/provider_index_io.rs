//! IO owner for explicitly supplied, closed Codex index backups.
//!
//! Absence of sidecars does not prove a live database is closed. This API
//! requires the caller's closed-backup contract and never discovers a home DB.
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, bail, ensure};
use sea_orm::sqlx::{Connection, sqlite::SqliteConnectOptions};
use serde::{Deserialize, Serialize};

use super::{
    authorized_read::{self, RegisteredHelperOutput},
    observed_agents::{codex_index, transcript_source as source},
};

const MODE: &str = "codex-offline-index-copy-v1";
const COPY_LEAF: &str = "index.sqlite";
const SOURCE_CAP: u64 = 16 * 1024 * 1024;
pub(crate) const OUTPUT_CAP: u64 = 4 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Control {
    mode: String,
    scratch: PathBuf,
    scratch_identity: (u64, u64),
    remaining_ms: u64,
    source_len: u64,
    header: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recent_scope: Option<codex_index::RecentScopeQuery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    import_scope: Option<codex_index::ImportScopeQuery>,
}

#[derive(PartialEq, Eq)]
struct Stamp {
    identity: (u64, u64),
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
    #[cfg(windows)]
    changed: i64,
}

fn identity(file: &File) -> Result<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        Ok(crate::utils::windows_fs::identity(file)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        bail!("secure index identity is unavailable on this platform")
    }
}

fn stamp(file: &File) -> Result<Stamp> {
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "index backup must be a regular file");
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    #[cfg(unix)]
    ensure!(
        metadata.nlink() == 1,
        "index backup must be a standalone file, not a hard link"
    );
    #[cfg(windows)]
    let changed = {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::Storage::FileSystem::{
            FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
        };
        ensure!(
            crate::utils::windows_fs::information(file)?.nNumberOfLinks == 1,
            "index backup must be a standalone file, not a hard link"
        );
        let mut information = FILE_BASIC_INFO::default();
        // SAFETY: a live file handle and an initialized, correctly sized output.
        ensure!(
            unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileBasicInfo,
                    (&mut information as *mut FILE_BASIC_INFO).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            } != 0,
            "cannot inspect index backup change time"
        );
        information.ChangeTime
    };
    Ok(Stamp {
        identity: identity(file)?,
        len: metadata.len(),
        modified: metadata.modified()?,
        #[cfg(unix)]
        changed: (metadata.ctime(), metadata.ctime_nsec()),
        #[cfg(windows)]
        changed,
    })
}

fn header(file: &File) -> Result<[u8; 100]> {
    let mut header = [0; 100];
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(&mut header, 0)?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        // seek_read moves the shared cursor on Windows. Parent probes run
        // only before spawn or after successful reap; the worker rewinds
        // before its sequential copy read. Unix uses positional reads above.
        let mut read = 0;
        while read < header.len() {
            let count = file.seek_read(&mut header[read..], read as u64)?;
            ensure!(count > 0, "index header is truncated");
            read += count;
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        bail!("secure index header reads are unavailable on this platform");
    }
    Ok(header)
}

fn validate_header(header: &[u8], len: u64) -> Result<()> {
    ensure!(header.len() == 100, "index header is truncated");
    ensure!(
        &header[..16] == b"SQLite format 3\0",
        "invalid index header"
    );
    ensure!(
        matches!(header[18], 1 | 2) && header[18] == header[19],
        "inconsistent index journal format"
    );
    let encoded_page_size = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if encoded_page_size == 1 {
        65536
    } else {
        u64::from(encoded_page_size)
    };
    ensure!(
        (512..=65536).contains(&page_size) && page_size.is_power_of_two(),
        "invalid index page size"
    );
    ensure!(
        (page_size..=SOURCE_CAP).contains(&len) && len.is_multiple_of(page_size),
        "index backup is truncated or exceeds the read limit"
    );
    ensure!(
        header[24..28] == header[92..96],
        "index backup has an uncommitted header"
    );
    let pages = u32::from_be_bytes([header[28], header[29], header[30], header[31]]);
    ensure!(
        u64::from(pages) == len / page_size,
        "index page count changed"
    );
    Ok(())
}

fn without_sidecars(directory: &File, leaf: &std::ffi::OsStr, end: Instant) -> Result<()> {
    let sidecars = ["-wal", "-shm", "-journal"].map(|suffix| {
        let mut name = leaf.to_os_string();
        name.push(suffix);
        name
    });
    for (count, entry) in source::read_dir_pinned_provider_directory(directory)?.enumerate() {
        ensure!(
            count < 4096 && Instant::now() < end,
            "index directory budget exceeded"
        );
        let entry = entry?;
        ensure!(
            !sidecars.contains(&entry.file_name),
            "index sidecar is present; provide a closed consistent backup"
        );
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<File> {
    #[cfg(windows)]
    let directory = crate::utils::windows_fs::open_private_directory(path)?;
    #[cfg(not(windows))]
    let directory = source::open_absolute_directory_no_follow(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = directory.metadata()?;
        ensure!(
            metadata.mode() & 0o077 == 0 && metadata.uid() == unsafe { libc::geteuid() },
            "index scratch must be owner-only"
        );
    }
    Ok(directory)
}

/// Read only a caller-supplied, already closed and consistent backup. No
/// provider-home lookup or live DB/WAL copying is performed here. The API is
/// kept for compatibility and operator-local None-mode checks.
#[allow(
    dead_code,
    reason = "compatibility-only None-mode reader; import uses the scoped projection"
)]
pub(crate) async fn read_closed_backup(
    backup: &Path,
    end: Instant,
) -> Result<codex_index::IndexSnapshot> {
    let bytes = read_closed_backup_bytes(backup, end, None, None).await?;
    serde_json::from_slice(&bytes).context("index backup is unavailable or unsupported")
}

/// The caller provides both the explicit closed backup and the repository
/// scope; this never discovers a live provider database or reads rollouts.
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub(crate) enum ReconciliationReadFailure {
    #[error("reconciliation root changed or cannot be proved; reopen the worktree and retry")]
    ScopeRootUnproven,
    #[error("reconciliation helper response is inconsistent; retry with the current Libra build")]
    ResponseInconsistent,
    #[error("reconciliation exhausted its original time budget; retry with a smaller backup")]
    Budget,
}

impl ReconciliationReadFailure {
    pub(crate) fn reason_key(self) -> &'static str {
        match self {
            Self::ScopeRootUnproven => "scope_root_unproven",
            Self::ResponseInconsistent => "response_inconsistent",
            Self::Budget => "budget_exhausted",
        }
    }
}

pub(crate) async fn read_recent_closed_backup(
    backup: &Path,
    mut query: codex_index::RecentScopeQuery,
    end: Instant,
) -> Result<codex_index::RecentIndexOutcome> {
    ensure!(query.version == 1, "unsupported recent index query version");
    let root = normalized_reconciliation_cwd(&query.root)
        .context(ReconciliationReadFailure::ScopeRootUnproven)?;
    ensure!(
        root.to_str().is_some(),
        ReconciliationReadFailure::ScopeRootUnproven
    );
    ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
    let root_for_pin = root.clone();
    let (root_pin, root_identity) = tokio::task::spawn_blocking(move || {
        let pin = source::open_absolute_directory_no_follow(&root_for_pin)?;
        let id = identity(&pin)?;
        Ok::<_, anyhow::Error>((pin, id))
    })
    .await
    .context(ReconciliationReadFailure::ScopeRootUnproven)?
    .context(ReconciliationReadFailure::ScopeRootUnproven)?;
    query.root = root;
    query.root_identity = Some(root_identity);
    let bytes = read_closed_backup_bytes(backup, end, Some(query.clone()), None)
        .await
        .map_err(|error| {
            if Instant::now() >= end {
                error.context(ReconciliationReadFailure::Budget)
            } else {
                error
            }
        })?;
    let root_for_check = query.root.clone();
    tokio::task::spawn_blocking(move || {
        // Keep the original inode pinned until the reopen check has completed.
        let _root_pin = root_pin;
        ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
        let reopened = source::open_absolute_directory_no_follow(&root_for_check)
            .context(ReconciliationReadFailure::ScopeRootUnproven)?;
        let actual = identity(&reopened).context(ReconciliationReadFailure::ScopeRootUnproven)?;
        ensure!(
            actual == root_identity,
            ReconciliationReadFailure::ScopeRootUnproven
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context(ReconciliationReadFailure::ScopeRootUnproven)??;
    let outcome =
        serde_json::from_slice(&bytes).context(ReconciliationReadFailure::ResponseInconsistent)?;
    validate_recent_response(&outcome, &query)
        .context(ReconciliationReadFailure::ResponseInconsistent)?;
    Ok(outcome)
}

fn validate_recent_response(
    outcome: &codex_index::RecentIndexOutcome,
    query: &codex_index::RecentScopeQuery,
) -> Result<()> {
    let codex_index::RecentIndexOutcome::Available { snapshot } = outcome else {
        return Ok(());
    };
    let cutoff = query
        .now_ms
        .checked_sub(codex_index::RECENT_WINDOW_MS)
        .filter(|value| *value >= 0)
        .context("invalid recent window")?;
    let latest = query
        .now_ms
        .checked_add(codex_index::CLOCK_SKEW_MS)
        .context("invalid recent clock")?;
    let freshness = match snapshot.backup_max_updated_at_ms {
        None => codex_index::IndexFreshness::EmptySnapshot,
        Some(value) if value < cutoff => codex_index::IndexFreshness::OutsideWindow,
        Some(_) => codex_index::IndexFreshness::WithinWindow,
    };
    ensure!(
        snapshot.window_anchor_ms == query.now_ms
            && snapshot.cutoff_ms == cutoff
            && (25..=59).contains(&snapshot.max_migration)
            && snapshot.candidate_count == snapshot.threads.len()
            && snapshot.threads.len() <= 10_000
            && snapshot.scope_ambiguous_count
                == snapshot
                    .threads
                    .iter()
                    .filter(|thread| thread.scope == codex_index::RecentScopeStatus::AliasAmbiguous)
                    .count()
            && snapshot.freshness == freshness
            && snapshot
                .backup_max_updated_at_ms
                .is_none_or(|value| (0..=latest).contains(&value)),
        "recent index response is inconsistent"
    );
    ensure!(
        snapshot.scope_unknown_count
            == snapshot
                .threads
                .iter()
                .filter(|thread| thread.scope == codex_index::RecentScopeStatus::LayoutUnknown)
                .count(),
        "recent scope unknown count is inconsistent"
    );
    let mut seen = std::collections::HashSet::new();
    let mut previous: Option<(i64, &str)> = None;
    for thread in &snapshot.threads {
        ensure!(
            valid_response_id(&thread.id)
                && seen.insert(thread.id.as_str())
                && (cutoff..=latest).contains(&thread.updated_at_ms)
                && (0..=latest).contains(&thread.created_at_ms)
                && snapshot
                    .backup_max_updated_at_ms
                    .is_some_and(|maximum| maximum >= thread.updated_at_ms),
            "recent thread response is inconsistent"
        );
        if let Some((updated, id)) = previous {
            ensure!(
                updated > thread.updated_at_ms
                    || (updated == thread.updated_at_ms && id < thread.id.as_str()),
                "recent thread ordering is inconsistent"
            );
        }
        previous = Some((thread.updated_at_ms, &thread.id));
    }
    ensure!(
        snapshot.spawn_edges.len() <= 10_000
            && snapshot
                .spawn_edges
                .iter()
                .all(|edge| seen.contains(edge.child_thread_id.as_str())
                    && valid_response_id(&edge.parent_thread_id)),
        "recent edge response is inconsistent"
    );
    ensure!(
        freshness == codex_index::IndexFreshness::WithinWindow || snapshot.threads.is_empty(),
        "empty recent window response is inconsistent"
    );
    Ok(())
}

fn valid_response_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Keep the original directory inode alive through consent and catalog reads.
/// This is never a capability to open a provider transcript.
pub(crate) struct ImportRootPin {
    path: PathBuf,
    pin: Arc<File>,
    id: (u64, u64),
}

impl ImportRootPin {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn revalidate(&self, end: Instant) -> Result<()> {
        let root = self.path.clone();
        let held = self.pin.clone();
        let expected = self.id;
        tokio::task::spawn_blocking(move || {
            let _held = held;
            ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
            let reopened = source::open_absolute_directory_no_follow(&root)
                .context(ReconciliationReadFailure::ScopeRootUnproven)?;
            ensure!(
                identity(&reopened).context(ReconciliationReadFailure::ScopeRootUnproven)?
                    == expected,
                ReconciliationReadFailure::ScopeRootUnproven
            );
            ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context(ReconciliationReadFailure::ScopeRootUnproven)?
    }
}

pub(crate) struct ImportIndexRead {
    pub outcome: codex_index::ImportIndexOutcome,
    pub root: ImportRootPin,
}

pub(crate) async fn read_import_closed_backup(
    backup: &Path,
    mut query: codex_index::ImportScopeQuery,
    end: Instant,
) -> Result<ImportIndexRead> {
    ensure!(
        query.version == 1,
        ReconciliationReadFailure::ResponseInconsistent
    );
    ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
    let root = normalized_reconciliation_cwd(&query.root)
        .filter(|path| path.to_str().is_some())
        .context(ReconciliationReadFailure::ScopeRootUnproven)?;
    let path = root.clone();
    let pin = tokio::task::spawn_blocking(move || {
        let pin = source::open_absolute_directory_no_follow(&path)?;
        let id = identity(&pin)?;
        Ok::<_, anyhow::Error>((pin, id))
    })
    .await
    .context(ReconciliationReadFailure::ScopeRootUnproven)?
    .context(ReconciliationReadFailure::ScopeRootUnproven)?;
    query.root = root.clone();
    query.root_identity = Some(pin.1);
    let proof = ImportRootPin {
        path: root,
        pin: Arc::new(pin.0),
        id: pin.1,
    };
    let bytes = read_closed_backup_bytes(backup, end, None, Some(query.clone()))
        .await
        .map_err(|error| {
            if Instant::now() >= end {
                error.context(ReconciliationReadFailure::Budget)
            } else {
                error
            }
        })?;
    let outcome =
        serde_json::from_slice(&bytes).context(ReconciliationReadFailure::ResponseInconsistent)?;
    validate_import_response(&outcome).context(ReconciliationReadFailure::ResponseInconsistent)?;
    proof.revalidate(end).await?;
    Ok(ImportIndexRead {
        outcome,
        root: proof,
    })
}

fn validate_import_response(outcome: &codex_index::ImportIndexOutcome) -> Result<()> {
    use codex_index::{ImportIndexOutcome, ImportSourceState, RecentScopeStatus};
    let ImportIndexOutcome::Available { snapshot } = outcome else {
        return Ok(());
    };
    ensure!(
        (21..=59).contains(&snapshot.max_migration)
            && snapshot.candidate_count == snapshot.threads.len()
            && snapshot.threads.len() <= 10_000
            && snapshot.scope_ambiguous_count
                == snapshot
                    .threads
                    .iter()
                    .filter(|row| row.scope == RecentScopeStatus::AliasAmbiguous)
                    .count()
            && snapshot.scope_unknown_count
                == snapshot
                    .threads
                    .iter()
                    .filter(|row| row.scope == RecentScopeStatus::LayoutUnknown)
                    .count(),
        "import index response counts are inconsistent"
    );
    let mut previous: Option<&str> = None;
    let mut proven = std::collections::HashSet::new();
    for row in &snapshot.threads {
        ensure!(
            valid_response_id(&row.id)
                && previous.is_none_or(|id| id.as_bytes() < row.id.as_bytes()),
            "import index response identifiers are inconsistent"
        );
        previous = Some(&row.id);
        let valid = match (row.scope, row.source_state, row.rollout_path.as_deref()) {
            (RecentScopeStatus::Proven, ImportSourceState::Eligible, Some(path)) => {
                codex_index::valid_import_rollout_path(path, &row.id)
            }
            (RecentScopeStatus::Proven, ImportSourceState::InvalidPath, None) => true,
            (
                RecentScopeStatus::LayoutUnknown | RecentScopeStatus::AliasAmbiguous,
                ImportSourceState::ScopeUnproven,
                None,
            ) => true,
            _ => false,
        };
        ensure!(valid, "import index source state is inconsistent");
        if row.scope == RecentScopeStatus::Proven {
            proven.insert(row.id.as_str());
        }
    }
    ensure!(
        snapshot.spawn_edges.len() <= 10_000
            && snapshot
                .spawn_edges
                .iter()
                .all(|edge| proven.contains(edge.parent_thread_id.as_str())
                    && proven.contains(edge.child_thread_id.as_str())),
        "import index edge response is inconsistent"
    );
    Ok(())
}

async fn read_closed_backup_bytes(
    backup: &Path,
    end: Instant,
    recent_scope: Option<codex_index::RecentScopeQuery>,
    import_scope: Option<codex_index::ImportScopeQuery>,
) -> Result<Vec<u8>> {
    struct Scratch {
        // Close the Windows directory pin before TempDir tries to remove it.
        _directory: File,
        _directory_owner: tempfile::TempDir,
        #[cfg(windows)]
        _private_root: PrivateRoot,
    }
    #[cfg(windows)]
    struct PrivateRoot(PathBuf);
    #[cfg(windows)]
    impl Drop for PrivateRoot {
        fn drop(&mut self) {
            // Only our now-empty UUID directory; never recursively sweep temp.
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    ensure!(Instant::now() < end, "index deadline expired");
    let backup = source::normalize_macos_system_directory_alias(backup);
    let parent = backup.parent().context("index backup has no parent")?;
    let leaf = backup.file_name().context("index backup has no filename")?;
    let directory = source::open_absolute_directory_no_follow(parent)?;
    without_sidecars(&directory, leaf, end)?;
    let file = source::open_file_beneath_pinned_provider_directory(&directory, Path::new(leaf))?;
    let before = stamp(&file)?;
    let initial_header = header(&file)?;
    validate_header(&initial_header, before.len)?;

    #[cfg(all(test, feature = "test-live-agent"))]
    let scratch_base = tests::SCRATCH_BASE
        .try_with(Clone::clone)
        .unwrap_or_else(|_| std::env::temp_dir());
    #[cfg(not(all(test, feature = "test-live-agent")))]
    let scratch_base = std::env::temp_dir();

    #[cfg(unix)]
    let owner = {
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .prefix("libra-codex-index-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(&scratch_base)?
    };
    #[cfg(windows)]
    let private_root = {
        let root = source::normalize_macos_system_directory_alias(&scratch_base);
        let parent = source::open_absolute_directory_no_follow(&root)?;
        let name = format!("libra-codex-index-{}", uuid::Uuid::new_v4());
        let pin = crate::utils::windows_fs::create_directory_at(&parent, name.as_ref(), true)?;
        drop(pin);
        PrivateRoot(root.join(name))
    };
    #[cfg(windows)]
    let owner = tempfile::Builder::new()
        .prefix("copy-")
        .tempdir_in(&private_root.0)?;
    #[cfg(not(any(unix, windows)))]
    let owner = {
        bail!("secure index scratch is unavailable on this platform");
        #[allow(unreachable_code)]
        tempfile::tempdir()?
    };
    let scratch_path = source::normalize_macos_system_directory_alias(owner.path());
    let scratch_directory = private_directory(&scratch_path)?;
    let control = Control {
        mode: MODE.to_owned(),
        scratch: scratch_path,
        scratch_identity: identity(&scratch_directory)?,
        remaining_ms: end
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(60_000) as u64,
        source_len: before.len,
        header: initial_header.to_vec(),
        recent_scope,
        import_scope,
    };
    ensure!(control.remaining_ms > 0, "index deadline expired");
    let mut command =
        authorized_read::registered_helper_command(authorized_read::AUTHORIZED_READ_HELPER_ARG)
            .context("index reader needs the registered Libra helper")?;
    command
        .env_clear()
        .current_dir(Path::new("/"))
        .env(
            authorized_read::AUTHORIZED_READ_HELPER_CAP_ENV,
            SOURCE_CAP.to_string(),
        )
        .env(
            authorized_read::AUTHORIZED_READ_HELPER_MODE_ENV,
            serde_json::to_string(&control)?,
        );
    let scratch = Scratch {
        _directory: scratch_directory,
        _directory_owner: owner,
        #[cfg(windows)]
        _private_root: private_root,
    };
    let output = authorized_read::run_registered_descriptor_helper_retaining_until(
        command,
        file.try_clone()?,
        OUTPUT_CAP,
        end,
        Some(Box::new(scratch)),
    )
    .await;
    let bytes = match output {
        RegisteredHelperOutput::Output(bytes) => bytes,
        _ => bail!("index backup could not be read within its budget"),
    };
    ensure!(Instant::now() < end, "index read exceeded its deadline");
    ensure!(
        stamp(&file)? == before && header(&file)? == initial_header,
        "index backup changed during read"
    );
    let reopened =
        source::open_file_beneath_pinned_provider_directory(&directory, Path::new(leaf))?;
    ensure!(
        stamp(&reopened)? == before,
        "index backup was replaced during read"
    );
    without_sidecars(&directory, leaf, end)?;
    Ok(bytes)
}

// Windows repository discovery may return a canonical verbatim disk path.
// Normalize this one known disk spelling for reconciliation only; live source
// header acceptance and UNC/device refusal retain their existing contract.
fn normalized_reconciliation_cwd(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if !path.is_absolute() {
            return None;
        }
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next()
            && let Prefix::VerbatimDisk(drive) = prefix.kind()
        {
            let mut disk = PathBuf::from(format!("{}:\\", char::from(drive.to_ascii_uppercase())));
            for component in components {
                match component {
                    Component::RootDir | Component::CurDir => {}
                    Component::Normal(name) => disk.push(name),
                    _ => return None,
                }
            }
            return authorized_read::normalized_codex_cwd(&disk);
        }
    }
    authorized_read::normalized_codex_cwd(path)
}

/// A possible-alias key only: it never supplies filesystem or import authority.
fn reconciliation_alias_key(path: &Path) -> Option<PathBuf> {
    use unicode_normalization::UnicodeNormalization;
    #[cfg(target_os = "macos")]
    let data_alias = path
        .strip_prefix("/System/Volumes/Data")
        .ok()
        .map(|suffix| Path::new("/").join(suffix));
    #[cfg(target_os = "macos")]
    let path = data_alias.as_deref().unwrap_or(path);
    let normalized = normalized_reconciliation_cwd(path)?;
    Some(PathBuf::from(
        normalized
            .to_str()?
            .to_lowercase()
            .nfc()
            .collect::<String>(),
    ))
}

fn check_scope_descendants(
    root: &File,
    relative: &Path,
    memo: &mut std::collections::HashMap<PathBuf, Option<(u64, u64)>>,
    end: Instant,
) -> Result<()> {
    let mut directory = root.try_clone().context("retain reconciliation root")?;
    let mut prefix = PathBuf::new();
    for component in relative.components() {
        ensure!(Instant::now() < end, "scope deadline expired");
        let name = component.as_os_str();
        let metadata_name = name.to_str().is_some_and(|name| {
            name.eq_ignore_ascii_case(".git") || name.eq_ignore_ascii_case(".libra")
        });
        ensure!(
            !metadata_name,
            "repository metadata is not a session working directory"
        );
        prefix.push(name);
        if let Some(cached) = memo.get(&prefix) {
            let expected = cached.context("cached scope is unproven")?;
            let opened =
                source::open_directory_beneath_pinned_directory(&directory, Path::new(name))?;
            ensure!(
                identity(&opened)? == expected,
                "cached scope directory was replaced"
            );
            directory = opened;
            continue;
        }
        ensure!(memo.len() < 1024, "scope directory memo limit exceeded");
        let opened = (|| {
            let opened =
                source::open_directory_beneath_pinned_directory(&directory, Path::new(name))?;
            for (index, entry) in source::read_dir_pinned_provider_directory(&opened)?.enumerate() {
                ensure!(
                    index < 4096 && Instant::now() < end,
                    "scope directory budget exceeded"
                );
                let entry = entry?;
                let marker = entry.file_name.to_str().is_some_and(|name| {
                    name.eq_ignore_ascii_case(".git") || name.eq_ignore_ascii_case(".libra")
                });
                ensure!(!marker, "nested repository scope is unproven");
            }
            Ok::<_, anyhow::Error>(opened)
        })();
        match opened {
            Ok(opened) => {
                memo.insert(prefix.clone(), Some(identity(&opened)?));
                directory = opened;
            }
            Err(error) => {
                memo.insert(prefix.clone(), None);
                return Err(error);
            }
        }
    }
    Ok(())
}

struct ScopeProjection {
    root: PathBuf,
    alias_root: PathBuf,
    pin: File,
    memo: std::collections::HashMap<PathBuf, Option<(u64, u64)>>,
}
impl ScopeProjection {
    fn new(root: &Path, expected: Option<(u64, u64)>, end: Instant) -> Result<Self> {
        let root = normalized_reconciliation_cwd(root)
            .context(ReconciliationReadFailure::ScopeRootUnproven)?;
        ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
        let alias_root = reconciliation_alias_key(&root)
            .context(ReconciliationReadFailure::ScopeRootUnproven)?;
        let pin = source::open_absolute_directory_no_follow(&root)
            .context(ReconciliationReadFailure::ScopeRootUnproven)?;
        ensure!(
            Some(identity(&pin).context(ReconciliationReadFailure::ScopeRootUnproven)?) == expected,
            ReconciliationReadFailure::ScopeRootUnproven
        );
        Ok(Self {
            root,
            alias_root,
            pin,
            memo: std::collections::HashMap::new(),
        })
    }
    fn classify(
        &mut self,
        cwd: &str,
        end: Instant,
    ) -> Result<Option<codex_index::RecentScopeStatus>> {
        ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
        let normalized = normalized_reconciliation_cwd(Path::new(cwd));
        // Lexical checks precede provider-cwd filesystem access.
        let scope = match normalized
            .as_ref()
            .and_then(|cwd| cwd.strip_prefix(&self.root).ok())
        {
            Some(relative) => {
                if check_scope_descendants(&self.pin, relative, &mut self.memo, end).is_ok() {
                    codex_index::RecentScopeStatus::Proven
                } else {
                    codex_index::RecentScopeStatus::LayoutUnknown
                }
            }
            None if normalized.is_none() => codex_index::RecentScopeStatus::LayoutUnknown,
            None => {
                let possible_alias = normalized
                    .as_ref()
                    .and_then(|cwd| reconciliation_alias_key(cwd))
                    .is_some_and(|cwd| cwd.starts_with(&self.alias_root));
                if !possible_alias {
                    return Ok(None);
                }
                codex_index::RecentScopeStatus::AliasAmbiguous
            }
        };
        Ok(Some(scope))
    }
}

fn project_recent_scope(
    snapshot: codex_index::RecentRawSnapshot,
    query: &codex_index::RecentScopeQuery,
    end: Instant,
) -> Result<codex_index::RecentIndexSnapshot> {
    let mut projection = ScopeProjection::new(&query.root, query.root_identity, end)?;
    let mut threads = Vec::new();
    let mut ambiguous = 0usize;
    let mut unknown = 0usize;
    for thread in snapshot.threads {
        ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
        let Some(scope) = projection.classify(&thread.cwd, end)? else {
            continue;
        };
        match scope {
            codex_index::RecentScopeStatus::AliasAmbiguous => ambiguous += 1,
            codex_index::RecentScopeStatus::LayoutUnknown => unknown += 1,
            codex_index::RecentScopeStatus::Proven => {}
        }
        threads.push(codex_index::RecentIndexThread {
            id: thread.id,
            created_at_ms: thread.created_at_ms,
            updated_at_ms: thread.updated_at_ms,
            archived: thread.archived,
            scope,
        });
    }
    let ids: std::collections::HashSet<_> =
        threads.iter().map(|thread| thread.id.as_str()).collect();
    let spawn_edges = snapshot
        .spawn_edges
        .into_iter()
        .filter(|edge| ids.contains(edge.child_thread_id.as_str()))
        .map(|edge| codex_index::RecentSpawnEdge {
            parent_thread_id: edge.parent_thread_id,
            child_thread_id: edge.child_thread_id,
            status: codex_index::RecentEdgeStatus::from_provider(&edge.status),
        })
        .collect();
    ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
    Ok(codex_index::RecentIndexSnapshot {
        max_migration: snapshot.max_migration,
        window_anchor_ms: snapshot.window_anchor_ms,
        cutoff_ms: snapshot.cutoff_ms,
        backup_max_updated_at_ms: snapshot.backup_max_updated_at_ms,
        freshness: snapshot.freshness,
        candidate_count: threads.len(),
        scope_ambiguous_count: ambiguous,
        scope_unknown_count: unknown,
        threads,
        spawn_edges,
    })
}

fn project_import_scope(
    snapshot: codex_index::IndexSnapshot,
    query: &codex_index::ImportScopeQuery,
    end: Instant,
) -> Result<codex_index::ImportIndexSnapshot> {
    use codex_index::{ImportSourceState, RecentScopeStatus};
    let mut projection = ScopeProjection::new(&query.root, query.root_identity, end)?;
    let mut threads = Vec::new();
    for row in snapshot.threads {
        let Some(scope) = projection.classify(&row.cwd, end)? else {
            continue;
        };
        // Data-derived source paths are never stat'ed or opened by this owner.
        let path = PathBuf::from(row.rollout_path);
        let (source_state, rollout_path) = if scope != RecentScopeStatus::Proven {
            (ImportSourceState::ScopeUnproven, None)
        } else if codex_index::valid_import_rollout_path(&path, &row.id) {
            (ImportSourceState::Eligible, Some(path))
        } else {
            (ImportSourceState::InvalidPath, None)
        };
        threads.push(codex_index::ImportIndexThread {
            id: row.id,
            archived: row.archived,
            scope,
            source_state,
            rollout_path,
        });
    }
    threads.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    let proven: std::collections::HashSet<_> = threads
        .iter()
        .filter(|row| row.scope == RecentScopeStatus::Proven)
        .map(|row| row.id.as_str())
        .collect();
    let spawn_edges = snapshot
        .spawn_edges
        .into_iter()
        .filter(|edge| {
            proven.contains(edge.parent_thread_id.as_str())
                && proven.contains(edge.child_thread_id.as_str())
        })
        .map(|edge| codex_index::RecentSpawnEdge {
            parent_thread_id: edge.parent_thread_id,
            child_thread_id: edge.child_thread_id,
            status: codex_index::RecentEdgeStatus::from_provider(&edge.status),
        })
        .collect();
    ensure!(Instant::now() < end, ReconciliationReadFailure::Budget);
    Ok(codex_index::ImportIndexSnapshot {
        max_migration: snapshot.max_migration,
        candidate_count: threads.len(),
        scope_ambiguous_count: threads
            .iter()
            .filter(|row| row.scope == RecentScopeStatus::AliasAmbiguous)
            .count(),
        scope_unknown_count: threads
            .iter()
            .filter(|row| row.scope == RecentScopeStatus::LayoutUnknown)
            .count(),
        threads,
        spawn_edges,
    })
}

fn create_copy(directory: &File) -> Result<File> {
    #[cfg(unix)]
    {
        use std::{
            ffi::CString,
            os::fd::{AsRawFd, FromRawFd},
        };
        let name = CString::new(COPY_LEAF)?;
        // SAFETY: held directory and fixed NUL-terminated component; ownership
        // of a successful, fresh no-follow descriptor transfers immediately.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        ensure!(fd >= 0, "cannot create private index copy");
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    #[cfg(windows)]
    {
        Ok(crate::utils::windows_fs::create_private_file_at(
            directory,
            COPY_LEAF.as_ref(),
        )?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = directory;
        bail!("secure index copy creation is unavailable")
    }
}

async fn read_copy(
    path: &Path,
    end: Instant,
    recent_scope: Option<&codex_index::RecentScopeQuery>,
    import_scope: Option<&codex_index::ImportScopeQuery>,
) -> Result<Vec<u8>> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false)
        .immutable(true)
        .busy_timeout(Duration::ZERO)
        .pragma("query_only", "ON")
        .pragma("trusted_schema", "OFF")
        .pragma("ignore_check_constraints", "ON")
        .pragma("temp_store", "MEMORY");
    let mut connection = sea_orm::sqlx::SqliteConnection::connect_with(&options).await?;
    {
        let mut handle = connection.lock_handle().await?;
        unsafe extern "C" {
            fn sqlite3_limit(
                connection: *mut std::ffi::c_void,
                category: std::ffi::c_int,
                value: std::ffi::c_int,
            ) -> std::ffi::c_int;
        }
        let cap = i32::try_from(SOURCE_CAP).context("index engine limit exceeds this platform")?;
        // SAFETY: SQLx holds the connection lock, excluding its worker. This
        // stable SQLite C API receives its live opaque handle; category 0 is
        // SQLITE_LIMIT_LENGTH. Bound generated/CHECK expression intermediates
        // before integrity/schema queries, without narrowing valid source rows.
        let previous = unsafe { sqlite3_limit(handle.as_raw_handle().as_ptr().cast(), 0, cap) };
        ensure!(previous > 0, "cannot bound the index engine");
        handle.set_progress_handler(1000, move || Instant::now() < end);
    }
    let result = async {
        if let Some(query) = import_scope {
            let outcome = match codex_index::query_snapshot(&mut connection, end).await {
                Ok(snapshot) => match project_import_scope(snapshot, query, end) {
                    Ok(snapshot) => codex_index::ImportIndexOutcome::Available { snapshot },
                    Err(error)
                        if matches!(
                            error.downcast_ref::<ReconciliationReadFailure>(),
                            Some(ReconciliationReadFailure::ScopeRootUnproven)
                        ) =>
                    {
                        codex_index::ImportIndexOutcome::Unavailable {
                            reason: codex_index::IndexUnavailableReason::ScopeRootUnproven,
                        }
                    }
                    Err(error) => return Err(error),
                },
                Err(error) => match error.downcast_ref::<codex_index::IndexUnavailableReason>() {
                    Some(reason) => {
                        codex_index::ImportIndexOutcome::Unavailable { reason: *reason }
                    }
                    None => return Err(error),
                },
            };
            return Ok(serde_json::to_vec(&outcome)?);
        }
        match recent_scope {
            None => codex_index::query_snapshot(&mut connection, end)
                .await
                .and_then(|snapshot| Ok(serde_json::to_vec(&snapshot)?)),
            Some(query) => {
                let outcome = match codex_index::query_recent_snapshot(&mut connection, query, end)
                    .await
                {
                    Ok(snapshot) => match project_recent_scope(snapshot, query, end) {
                        Ok(snapshot) => codex_index::RecentIndexOutcome::Available { snapshot },
                        Err(error)
                            if matches!(
                                error.downcast_ref::<ReconciliationReadFailure>(),
                                Some(ReconciliationReadFailure::ScopeRootUnproven)
                            ) =>
                        {
                            codex_index::RecentIndexOutcome::Unavailable {
                                reason: codex_index::IndexUnavailableReason::ScopeRootUnproven,
                            }
                        }
                        Err(error) => return Err(error),
                    },
                    Err(error) => match error.downcast_ref::<codex_index::IndexUnavailableReason>()
                    {
                        Some(reason) => {
                            codex_index::RecentIndexOutcome::Unavailable { reason: *reason }
                        }
                        None => return Err(error),
                    },
                };
                Ok(serde_json::to_vec(&outcome)?)
            }
        }
    }
    .await;
    connection.close().await?;
    result
}

fn worker(control: Control) -> Result<Vec<u8>> {
    ensure!(
        control.recent_scope.is_none() || control.import_scope.is_none(),
        "ambiguous index query"
    );
    if let Some(query) = &control.import_scope {
        ensure!(
            query.version == 1 && query.root_identity.is_some(),
            "unsupported import index query"
        );
    }
    ensure!(
        (1..=60_000).contains(&control.remaining_ms),
        "invalid index deadline"
    );
    validate_header(&control.header, control.source_len)?;
    let end = Instant::now() + Duration::from_millis(control.remaining_ms);
    let directory = private_directory(&control.scratch)?;
    ensure!(
        identity(&directory)? == control.scratch_identity,
        "index scratch was replaced"
    );
    ensure!(
        source::read_dir_pinned_provider_directory(&directory)?
            .next()
            .is_none(),
        "index scratch is not empty"
    );
    let mut input = authorized_read::duplicate_helper_stdin_descriptor()
        .context("index input is unavailable")?;
    let before = stamp(&input)?;
    ensure!(
        before.len == control.source_len && header(&input)?.as_slice() == control.header,
        "index source changed before copy"
    );
    input.seek(SeekFrom::Start(0))?;
    let bytes = match authorized_read::read_strictly_bounded(&mut input, SOURCE_CAP) {
        authorized_read::StrictBoundedRead::Complete(bytes) => bytes,
        _ => bail!("index copy exceeds its budget"),
    };
    ensure!(
        stamp(&input)? == before
            && bytes.len() as u64 == before.len
            && header(&input)?.as_slice() == control.header,
        "index source changed during copy"
    );
    let mut copy = create_copy(&directory)?;
    copy.write_all(&bytes)?;
    drop(bytes);
    // This is an ephemeral, quiescent copy in the kernel page cache, not a
    // durable backup. Closing its writer is sufficient before readonly open.
    drop(copy);
    ensure!(Instant::now() < end, "index deadline expired before query");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let bytes = runtime.block_on(read_copy(
        &control.scratch.join(COPY_LEAF),
        end,
        control.recent_scope.as_ref(),
        control.import_scope.as_ref(),
    ))?;
    ensure!(Instant::now() < end, "index deadline expired after query");
    // The private copy is immutable. SQLite must not create WAL/journal files.
    let entries =
        source::read_dir_pinned_provider_directory(&directory)?.collect::<Result<Vec<_>>>()?;
    ensure!(
        entries.len() == 1 && entries[0].file_name == COPY_LEAF,
        "index query created unexpected sidecars"
    );
    ensure!(
        bytes.len() as u64 <= OUTPUT_CAP,
        "index response exceeds its budget"
    );
    Ok(bytes)
}

pub(crate) fn run_helper_if_requested(cap: u64, mode: &str) -> Option<i32> {
    // The caller already bounds the JSON control before normal source parsing.
    if mode.len() > 16 * 1024 || !mode.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(mode).ok()?;
    if value.get("mode").and_then(serde_json::Value::as_str) != Some(MODE) {
        return None;
    }
    let result = (|| {
        ensure!(cap == SOURCE_CAP, "invalid index read cap");
        worker(serde_json::from_str(mode)?)
    })();
    Some(match result {
        Ok(bytes) => {
            if std::io::stdout().lock().write_all(&bytes).is_ok() {
                0
            } else {
                1
            }
        }
        Err(_) => 2, // No source paths, SQL errors, or provider data on stderr.
    })
}

#[cfg(test)]
#[cfg(feature = "test-live-agent")]
mod tests {
    use sea_orm::sqlx::{SqliteConnection, sqlite::SqliteJournalMode};
    use sha2::{Digest, Sha256};

    use super::*;

    tokio::task_local! { pub(super) static SCRATCH_BASE: PathBuf; }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_preflight_rejects_lazy_text_before_fetch() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let root = tempfile::tempdir().unwrap();
        let positive = root.path().join("ordinary.sqlite");
        fixture(&positive, false).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&positive)
                .read_only(true)
                .immutable(true),
        )
        .await
        .unwrap();
        let result = codex_index::FULL_SCOPE_ROW_FETCHES
            .scope(
                calls.clone(),
                codex_index::query_snapshot(
                    &mut connection,
                    Instant::now() + Duration::from_secs(2),
                ),
            )
            .await;
        assert!(result.is_ok());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "ordinary query must exercise both string projections"
        );
        connection.close().await.unwrap();

        let backup = root.path().join("lazy.sqlite");
        fixture(&backup, false).await;
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                .await
                .unwrap();
        sea_orm::sqlx::query("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<700) INSERT INTO threads SELECT printf('extra-%d',x),'/raw','/work/repo',0,'0.162.1' FROM n")
            .execute(&mut connection).await.unwrap();
        sea_orm::sqlx::query("ALTER TABLE threads RENAME COLUMN cwd TO old_cwd")
            .execute(&mut connection)
            .await
            .unwrap();
        // DEFAULT cannot bind parameters. The only interpolated byte is the
        // fixed ASCII x, repeated by a fixture constant, never provider data.
        let alter = format!(
            "ALTER TABLE threads ADD COLUMN cwd TEXT NOT NULL DEFAULT '{}'",
            "x".repeat(2048)
        );
        sea_orm::sqlx::query(sea_orm::sqlx::AssertSqlSafe(alter))
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();
        assert!(std::fs::metadata(&backup).unwrap().len() < 1024 * 1024);
        let before = std::fs::read(&backup).unwrap();
        calls.store(0, Ordering::SeqCst);
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&backup)
                .read_only(true)
                .immutable(true),
        )
        .await
        .unwrap();
        let result = codex_index::FULL_SCOPE_ROW_FETCHES
            .scope(
                calls.clone(),
                codex_index::query_snapshot(
                    &mut connection,
                    Instant::now() + Duration::from_secs(2),
                ),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "oversized logical text must be refused before SQLx allocates string rows"
        );
        connection.close().await.unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), before);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_edge_text_shares_thread_budget_before_fetch() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let root = tempfile::tempdir().unwrap();
        let backup = root.path().join("combined.sqlite");
        fixture(&backup, false).await;
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                .await
                .unwrap();
        for sql in [
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<300) INSERT INTO threads SELECT printf('extra-%d',x),'/raw','/work/repo',0,'0.162.1' FROM n",
            "ALTER TABLE threads RENAME COLUMN cwd TO old_cwd",
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<6000) INSERT INTO thread_spawn_edges SELECT 'parent',printf('edge-%d',x),'completed' FROM n",
            "ALTER TABLE thread_spawn_edges RENAME COLUMN status TO old_status",
        ] {
            sea_orm::sqlx::query(sql)
                .execute(&mut connection)
                .await
                .unwrap();
        }
        for (ddl, bytes) in [
            (
                "ALTER TABLE threads ADD COLUMN cwd TEXT NOT NULL DEFAULT",
                2048,
            ),
            (
                "ALTER TABLE thread_spawn_edges ADD COLUMN status TEXT NOT NULL DEFAULT",
                128,
            ),
        ] {
            // Fixed DDL prefixes and ASCII x fixture bytes only; DEFAULT has no bind syntax.
            let sql = format!("{ddl} '{}'", "x".repeat(bytes));
            sea_orm::sqlx::query(sea_orm::sqlx::AssertSqlSafe(sql))
                .execute(&mut connection)
                .await
                .unwrap();
        }
        connection.close().await.unwrap();
        assert!(std::fs::metadata(&backup).unwrap().len() < 1024 * 1024);
        let before = std::fs::read(&backup).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&backup)
                .read_only(true)
                .immutable(true),
        )
        .await
        .unwrap();
        let result = codex_index::FULL_SCOPE_ROW_FETCHES
            .scope(
                calls.clone(),
                codex_index::query_snapshot(
                    &mut connection,
                    Instant::now() + Duration::from_secs(2),
                ),
            )
            .await;
        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<codex_index::IndexUnavailableReason>(),
            Some(&codex_index::IndexUnavailableReason::TextCap)
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "thread projection is bounded; oversized combined edge text must never be fetched"
        );
        connection.close().await.unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), before);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_duplicate_ids_are_rejected_in_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let backup = root.path().join("duplicates.sqlite");
        fixture(&backup, false).await;
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                .await
                .unwrap();
        for sql in [
            "ALTER TABLE threads RENAME TO original_threads",
            "CREATE TABLE threads AS SELECT * FROM original_threads",
            "INSERT INTO threads SELECT * FROM original_threads WHERE id='parent'",
        ] {
            sea_orm::sqlx::query(sql)
                .execute(&mut connection)
                .await
                .unwrap();
        }
        connection.close().await.unwrap();
        let before = std::fs::read(&backup).unwrap();
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&backup)
                .read_only(true)
                .immutable(true),
        )
        .await
        .unwrap();
        let error =
            codex_index::query_snapshot(&mut connection, Instant::now() + Duration::from_secs(2))
                .await
                .unwrap_err();
        assert_eq!(
            error.downcast_ref::<codex_index::IndexUnavailableReason>(),
            Some(&codex_index::IndexUnavailableReason::RowInvalid)
        );
        connection.close().await.unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), before);
    }

    async fn read_import_for_test(
        backup: &Path,
        repo: &Path,
        scratch: &Path,
    ) -> Result<ImportIndexRead> {
        SCRATCH_BASE
            .scope(
                scratch.to_path_buf(),
                authorized_read::with_test_helper_program(
                    helper(),
                    read_import_closed_backup(
                        backup,
                        codex_index::ImportScopeQuery {
                            version: 1,
                            root: repo.to_path_buf(),
                            root_identity: None,
                        },
                        Instant::now() + Duration::from_secs(5),
                    ),
                ),
            )
            .await
    }

    async fn full_fixture(backup: &Path, repo: &Path) {
        fixture(backup, false).await;
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(backup))
                .await
                .unwrap();
        sea_orm::sqlx::query(
            "UPDATE threads SET cwd=?,rollout_path=CASE id WHEN 'parent' THEN ? ELSE ? END",
        )
        .bind(repo.to_str().unwrap())
        .bind(repo.join("rollout-control-parent.jsonl").to_str().unwrap())
        .bind(repo.join("rollout-control-child.jsonl").to_str().unwrap())
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_bad_paths_are_proven_but_never_returned() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let backup = temp.path().join("index.sqlite");
        full_fixture(&backup, &repo).await;
        let scratch = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for leaf in [
            "auth.json",
            "config.toml",
            "history.jsonl",
            "state_5.sqlite",
        ] {
            let path = temp.path().join(leaf);
            std::fs::write(&path, b"CX19-DO-NOT-READ-SENTINEL").unwrap();
            paths.push(path);
        }
        paths.extend([
            PathBuf::from("auth.json"),
            repo.join("..").join("rollout-x-parent.jsonl"),
            repo.join("rollout-x-wrong-sid.jsonl"),
        ]);
        for path in paths {
            let mut connection =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                    .await
                    .unwrap();
            sea_orm::sqlx::query("UPDATE threads SET rollout_path=? WHERE id='parent'")
                .bind(path.to_str().unwrap())
                .execute(&mut connection)
                .await
                .unwrap();
            connection.close().await.unwrap();
            let read = read_import_for_test(&backup, &repo, scratch.path())
                .await
                .unwrap();
            let codex_index::ImportIndexOutcome::Available { snapshot } = read.outcome else {
                panic!("closed full-history fixture unavailable");
            };
            assert_eq!(snapshot.max_migration, 21);
            assert_eq!(snapshot.candidate_count, 2);
            let parent = snapshot
                .threads
                .iter()
                .find(|row| row.id == "parent")
                .unwrap();
            assert_eq!(parent.scope, codex_index::RecentScopeStatus::Proven);
            assert_eq!(
                parent.source_state,
                codex_index::ImportSourceState::InvalidPath
            );
            assert!(parent.rollout_path.is_none());
            assert_eq!(snapshot.threads[0].id, "child");
            assert_eq!(
                snapshot.threads[0].source_state,
                codex_index::ImportSourceState::Eligible
            );
            assert_eq!(snapshot.scope_unknown_count, 0);
        }
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_edges_require_both_proven_endpoints() {
        for kind in ["foreign", "nested", "alias", "proven"] {
            let temp = tempfile::tempdir().unwrap();
            let repo = temp.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            let backup = temp.path().join("index.sqlite");
            full_fixture(&backup, &repo).await;
            let parent = match kind {
                "foreign" => temp.path().join("outside"),
                "nested" => {
                    let dir = repo.join("nested");
                    std::fs::create_dir(&dir).unwrap();
                    std::fs::write(dir.join(".libra"), b"nested").unwrap();
                    dir
                }
                "alias" => repo.with_file_name("REPO"),
                _ => repo.clone(),
            };
            let mut connection =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                    .await
                    .unwrap();
            sea_orm::sqlx::query("UPDATE threads SET cwd=? WHERE id='parent'")
                .bind(parent.to_str().unwrap())
                .execute(&mut connection)
                .await
                .unwrap();
            for sql in [
                "UPDATE thread_spawn_edges SET status='PRIVATE-EDGE-STATUS'",
                "INSERT INTO thread_spawn_edges VALUES('parent','ghost','completed')",
            ] {
                sea_orm::sqlx::query(sql)
                    .execute(&mut connection)
                    .await
                    .unwrap();
            }
            connection.close().await.unwrap();
            let scratch = tempfile::tempdir().unwrap();
            let read = read_import_for_test(&backup, &repo, scratch.path())
                .await
                .unwrap();
            let wire = serde_json::to_string(&read.outcome).unwrap();
            assert!(!wire.contains("PRIVATE-EDGE-STATUS"));
            let codex_index::ImportIndexOutcome::Available { snapshot } = read.outcome else {
                panic!("full scope unavailable");
            };
            assert_eq!(snapshot.spawn_edges.len(), usize::from(kind == "proven"));
            if kind == "proven" {
                assert_eq!(
                    snapshot.spawn_edges[0].status,
                    codex_index::RecentEdgeStatus::Other
                );
            }
            assert_eq!(
                snapshot.candidate_count,
                if kind == "foreign" { 1 } else { 2 }
            );
        }
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_sort_is_bytewise_under_provider_nocase() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let backup = temp.path().join("index.sqlite");
        fixture(&backup, false).await;
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                .await
                .unwrap();
        for sql in [
            "ALTER TABLE threads RENAME TO original_threads",
            "CREATE TABLE threads(id TEXT COLLATE NOCASE,rollout_path TEXT,cwd TEXT,archived INTEGER,cli_version TEXT)",
        ] {
            sea_orm::sqlx::query(sql)
                .execute(&mut connection)
                .await
                .unwrap();
        }
        for id in ["a", "B"] {
            sea_orm::sqlx::query("INSERT INTO threads VALUES(?,?,?,0,'')")
                .bind(id)
                .bind(repo.join(format!("rollout-x-{id}.jsonl")).to_str().unwrap())
                .bind(repo.to_str().unwrap())
                .execute(&mut connection)
                .await
                .unwrap();
        }
        connection.close().await.unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let read = read_import_for_test(&backup, &repo, scratch.path())
            .await
            .unwrap();
        let codex_index::ImportIndexOutcome::Available { snapshot } = read.outcome else {
            panic!("full scope unavailable");
        };
        assert_eq!(
            snapshot
                .threads
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["B", "a"]
        );
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_parent_rejects_forged_relations_and_counts() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let backup = temp.path().join("index.sqlite");
        full_fixture(&backup, &repo).await;
        let scratch = tempfile::tempdir().unwrap();
        let read = read_import_for_test(&backup, &repo, scratch.path())
            .await
            .unwrap();
        validate_import_response(&read.outcome).unwrap();
        let baseline = serde_json::to_value(&read.outcome).unwrap();
        let mut bad = Vec::new();
        for (field, value) in [
            ("candidate_count", serde_json::json!(3)),
            ("scope_unknown_count", serde_json::json!(1)),
            ("scope_ambiguous_count", serde_json::json!(1)),
            ("max_migration", serde_json::json!(20)),
        ] {
            let mut wire = baseline.clone();
            wire["snapshot"][field] = value;
            bad.push(wire);
        }
        let mut wire = baseline.clone();
        wire["snapshot"]["threads"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        bad.push(wire);
        let mut wire = baseline.clone();
        wire["snapshot"]["threads"][1]["id"] = serde_json::json!("child");
        bad.push(wire);
        let mut wire = baseline.clone();
        wire["snapshot"]["threads"][0]["rollout_path"] = serde_json::Value::Null;
        bad.push(wire);
        let mut wire = baseline.clone();
        wire["snapshot"]["threads"][0]["source_state"] = serde_json::json!("scope_unproven");
        wire["snapshot"]["threads"][0]["rollout_path"] = serde_json::Value::Null;
        bad.push(wire);
        let mut wire = baseline.clone();
        wire["snapshot"]["threads"][0]["scope"] = serde_json::json!("layout_unknown");
        wire["snapshot"]["scope_unknown_count"] = serde_json::json!(1);
        wire["snapshot"]["threads"][0]["source_state"] = serde_json::json!("invalid_path");
        wire["snapshot"]["threads"][0]["rollout_path"] = serde_json::Value::Null;
        bad.push(wire);
        for path in [
            "auth.json".to_string(),
            repo.join("auth.json").to_str().unwrap().to_string(),
            repo.join("..")
                .join("rollout-x-child.jsonl")
                .to_str()
                .unwrap()
                .to_string(),
            repo.join("rollout-x-wrong-id.jsonl")
                .to_str()
                .unwrap()
                .to_string(),
        ] {
            let mut wire = baseline.clone();
            wire["snapshot"]["threads"][0]["rollout_path"] = serde_json::json!(path);
            bad.push(wire);
        }
        let mut wire = baseline.clone();
        wire["snapshot"]["spawn_edges"][0]["parent_thread_id"] = serde_json::json!("outside");
        bad.push(wire);
        for wire in bad {
            let outcome = serde_json::from_value(wire).unwrap();
            assert!(validate_import_response(&outcome).is_err());
        }
        let mut old = serde_json::json!({"mode":MODE,"scratch":"/scratch","scratch_identity":[1,2],"remaining_ms":1000,"source_len":4096,"header":[1,2,3]});
        let decoded: Control = serde_json::from_value(old.clone()).unwrap();
        assert!(decoded.import_scope.is_none());
        assert_eq!(serde_json::to_value(decoded).unwrap(), old);
        old["import_scope"] = serde_json::json!({"version":2,"root":"/repo","root_identity":[1,2]});
        let control: Control = serde_json::from_value(old.clone()).unwrap();
        assert!(
            worker(control)
                .unwrap_err()
                .to_string()
                .contains("unsupported import")
        );
        old["import_scope"]["version"] = serde_json::json!(1);
        old["recent_scope"] = serde_json::json!({"version":1,"root":"/repo","now_ms":NOW_MS});
        assert!(
            worker(serde_json::from_value(old).unwrap())
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_escaped_response_cap_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let backup = temp.path().join("index.sqlite");
        full_fixture(&backup, &repo).await;
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                .await
                .unwrap();
        let controls = char::from(1u8).to_string().repeat(2500);
        for n in 0..350 {
            let id = format!("row-{n}");
            let path = repo.join(format!("rollout-{controls}-{id}.jsonl"));
            sea_orm::sqlx::query("INSERT INTO threads VALUES(?,?,?,0,'')")
                .bind(id)
                .bind(path.to_str().unwrap())
                .bind(repo.to_str().unwrap())
                .execute(&mut connection)
                .await
                .unwrap();
        }
        connection.close().await.unwrap();
        let before = std::fs::read(&backup).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        assert!(
            read_import_for_test(&backup, &repo, scratch.path())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_sidecars_deadline_and_pinned_root_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let backup = temp.path().join("index.sqlite");
        full_fixture(&backup, &repo).await;
        let scratch = tempfile::tempdir().unwrap();
        let original = std::fs::read(&backup).unwrap();
        let read = read_import_for_test(&backup, &repo, scratch.path())
            .await
            .unwrap();
        assert!(matches!(
            read.outcome,
            codex_index::ImportIndexOutcome::Available { .. }
        ));
        read.root
            .revalidate(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        assert!(read.root.revalidate(Instant::now()).await.is_err());
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = temp.path().join(format!("index.sqlite{suffix}"));
            std::fs::write(&sidecar, b"functional sidecar").unwrap();
            assert!(
                read_import_for_test(&backup, &repo, scratch.path())
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(&sidecar).unwrap(), b"functional sidecar");
            std::fs::remove_file(sidecar).unwrap();
        }
        let expired = read_import_closed_backup(
            &backup,
            codex_index::ImportScopeQuery {
                version: 1,
                root: repo.clone(),
                root_identity: None,
            },
            Instant::now(),
        )
        .await;
        assert!(expired.is_err());
        std::fs::rename(&repo, temp.path().join("saved-repo")).unwrap();
        std::fs::create_dir(&repo).unwrap();
        let error = read
            .root
            .revalidate(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ReconciliationReadFailure>(),
            Some(ReconciliationReadFailure::ScopeRootUnproven)
        ));
        assert_eq!(std::fs::read(&backup).unwrap(), original);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_full_scope_inflight_source_root_and_deadline_are_rechecked() {
        for attack in ["root", "backup", "deadline"] {
            let temp = tempfile::tempdir().unwrap();
            let repo = temp.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            let backup = temp.path().join("index.sqlite");
            full_fixture(&backup, &repo).await;
            let ready = temp.path().join("helper-ready");
            let program = temp.path().join("slow-import-helper");
            let baseline_scratch = tempfile::tempdir().unwrap();
            let baseline = read_import_for_test(&backup, &repo, baseline_scratch.path())
                .await
                .unwrap();
            assert!(matches!(
                baseline.outcome,
                codex_index::ImportIndexOutcome::Available { .. }
            ));
            let frame = serde_json::to_string(&baseline.outcome).unwrap();
            python_helper(
                &program,
                &format!(
                    "import os,time\nfrom pathlib import Path\nPath({:?}).write_text(str(os.getpid())+'\\n')\ntime.sleep({})\nprint({:?})",
                    ready.to_str().unwrap(),
                    if attack == "deadline" { "30" } else { "0.4" },
                    frame,
                ),
            );
            let scratch = tempfile::tempdir().unwrap();
            let end = Instant::now()
                + Duration::from_millis(if attack == "deadline" { 1200 } else { 3000 });
            let read = SCRATCH_BASE.scope(
                scratch.path().to_path_buf(),
                authorized_read::with_test_helper_program(
                    program,
                    read_import_closed_backup(
                        &backup,
                        codex_index::ImportScopeQuery {
                            version: 1,
                            root: repo.clone(),
                            root_identity: None,
                        },
                        end,
                    ),
                ),
            );
            let mutate = async {
                let pid = loop {
                    if let Some(pid) = std::fs::read_to_string(&ready)
                        .ok()
                        .and_then(|value| value.strip_suffix('\n')?.parse::<i32>().ok())
                    {
                        break pid;
                    }
                    assert!(Instant::now() < end, "helper never started: {attack}");
                    tokio::time::sleep(Duration::from_millis(5)).await;
                };
                if attack == "root" {
                    std::fs::rename(&repo, temp.path().join("old-repo")).unwrap();
                    std::fs::create_dir(&repo).unwrap();
                } else if attack == "backup" {
                    let mut source = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&backup)
                        .unwrap();
                    source.write_all(b"changed source").unwrap();
                }
                pid
            };
            let (result, pid) = tokio::join!(read, mutate);
            let error = result.err().unwrap();
            match attack {
                "root" => assert!(matches!(
                    error.downcast_ref::<ReconciliationReadFailure>(),
                    Some(ReconciliationReadFailure::ScopeRootUnproven)
                )),
                "backup" => assert!(error.to_string().contains("changed"), "{error:#}"),
                "deadline" => assert!(matches!(
                    error.downcast_ref::<ReconciliationReadFailure>(),
                    Some(ReconciliationReadFailure::Budget)
                )),
                _ => unreachable!(),
            }
            // Cancellation returns at the host deadline. The existing reaper
            // retains the scratch until wait() proves helper termination.
            let reaped_by = Instant::now() + Duration::from_secs(3);
            while std::fs::read_dir(scratch.path()).unwrap().next().is_some() {
                assert!(
                    Instant::now() < reaped_by,
                    "helper cleanup did not finish: {attack}"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            // SAFETY: signal zero only checks existence of our completed child.
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "helper still running: {attack}"
            );
            assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_index_full_scope_rollout_local_disk_prefixes_are_explicit() {
        for path in [
            r"C:\repo\rollout-old-sid.jsonl",
            r"\\?\C:\repo\rollout-old-sid.jsonl",
        ] {
            assert!(codex_index::valid_import_rollout_path(
                Path::new(path),
                "sid"
            ));
        }
        for path in [
            r"\\host\share\rollout-old-sid.jsonl",
            r"\\?\UNC\host\share\rollout-old-sid.jsonl",
            r"\\.\C:\rollout-old-sid.jsonl",
            r"C:rollout-old-sid.jsonl",
            r"C:\repo\..\rollout-old-sid.jsonl",
            r"C:\repo\auth.json",
        ] {
            assert!(
                !codex_index::valid_import_rollout_path(Path::new(path), "sid"),
                "{path}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_reconcile_index_verbatim_disk_is_a_known_lexical_alias_only() {
        assert_eq!(
            normalized_reconciliation_cwd(Path::new(r"\\?\C:\repo\sub")),
            normalized_reconciliation_cwd(Path::new(r"C:\repo\sub"))
        );
        for path in [
            r"\\server\share\repo",
            r"\\?\UNC\server\share\repo",
            r"C:relative",
            r"\\?\C:\repo\..\other",
        ] {
            assert!(
                normalized_reconciliation_cwd(Path::new(path)).is_none(),
                "{path}"
            );
        }
    }

    const NOW_MS: i64 = 1_792_396_800_000;

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_reconcile_index_check_expressions_are_not_executed_by_integrity_probe() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let path = root.path().join("backup.sqlite");
        recent_fixture(&path, &repo).await;
        let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        sea_orm::sqlx::query("PRAGMA ignore_check_constraints=ON")
            .execute(&mut db)
            .await
            .unwrap();
        sea_orm::sqlx::query("CREATE TABLE check_probe(id INTEGER, CHECK(max(printf('%02000000d',1),printf('%02000000d',2),printf('%02000000d',3),printf('%02000000d',4)) IS NULL))").execute(&mut db).await.unwrap();
        sea_orm::sqlx::query("INSERT INTO check_probe VALUES(1)")
            .execute(&mut db)
            .await
            .unwrap();
        sea_orm::sqlx::query("PRAGMA ignore_check_constraints=OFF")
            .execute(&mut db)
            .await
            .unwrap();
        let check: String = sea_orm::sqlx::query_scalar("PRAGMA quick_check(1)")
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert!(check.contains("CHECK constraint failed"), "{check}");
        db.close().await.unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(before.len() < 1024 * 1024);
        let scratch = tempfile::tempdir().unwrap();
        // The connection intentionally skips CHECK bytecode. Relevant schema,
        // migration, type, scope and row validation still run in the read txn.
        let result = read_recent(&path, &repo, scratch.path()).await.unwrap();
        assert_eq!(result.candidate_count, 2);
        assert_eq!(result.scope_unknown_count, 0);
        assert_eq!(read(&path, scratch.path()).await.unwrap().threads.len(), 2);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_reconcile_index_unprojected_generated_schema_is_rejected_before_integrity() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let path = root.path().join("backup.sqlite");
        recent_fixture(&path, &repo).await;
        mutate_recent(&path,"CREATE TABLE unrelated_generated(id INTEGER, payload TEXT GENERATED ALWAYS AS(printf('%02000000d',id)) VIRTUAL NOT NULL)").await;
        mutate_recent(&path, "INSERT INTO unrelated_generated(id) VALUES(1)").await;
        let scratch = tempfile::tempdir().unwrap();
        assert!(read(&path, scratch.path()).await.is_err());
        assert_eq!(
            read_recent_outcome(&path, &repo, scratch.path())
                .await
                .unwrap(),
            codex_index::RecentIndexOutcome::Unavailable {
                reason: codex_index::IndexUnavailableReason::GeneratedSchema
            }
        );
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_reconcile_index_generated_projection_cannot_amplify_a_small_backup() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let path = root.path().join("backup.sqlite");
        recent_fixture(&path, &repo).await;
        mutate_recent(&path, "ALTER TABLE threads RENAME COLUMN cwd TO stored_cwd").await;
        mutate_recent(&path,"ALTER TABLE threads ADD COLUMN cwd TEXT GENERATED ALWAYS AS(printf('%020000000d',1)) VIRTUAL").await;
        assert!(std::fs::metadata(&path).unwrap().len() < 1024 * 1024);
        let mut db = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&path)
                .read_only(true)
                .immutable(true),
        )
        .await
        .unwrap();
        let error = codex_index::query_snapshot(&mut db, Instant::now() + Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "index schema is unavailable");
        db.close().await.unwrap();
        let scratch = tempfile::tempdir().unwrap();
        assert!(read(&path, scratch.path()).await.is_err());
        assert_eq!(
            read_recent_outcome(&path, &repo, scratch.path())
                .await
                .unwrap(),
            codex_index::RecentIndexOutcome::Unavailable {
                reason: codex_index::IndexUnavailableReason::SchemaUnsupported,
            }
        );
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_reconcile_invalid_rows_return_fixed_reason() {
        for statement in [
            "UPDATE threads SET id='invalid session id' WHERE id='child'",
            "UPDATE threads SET cwd='' WHERE id='child'",
            "UPDATE threads SET archived=7 WHERE id='child'",
            "UPDATE threads SET id=x'ff' WHERE id='child'",
        ] {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            let backup = root.path().join("backup.sqlite");
            recent_fixture(&backup, &repo).await;
            mutate_recent(&backup, statement).await;
            let scratch = tempfile::tempdir().unwrap();
            let outcome = read_recent_outcome(&backup, &repo, scratch.path())
                .await
                .unwrap();
            assert!(
                matches!(
                    outcome,
                    codex_index::RecentIndexOutcome::Unavailable {
                        reason: codex_index::IndexUnavailableReason::RowInvalid,
                    }
                ),
                "{statement}: {outcome:?}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_reconcile_sqlite_lazy_defaults_are_bounded_before_row_projection() {
        for (bytes, rows, expected) in [
            (2048usize, 700, codex_index::IndexUnavailableReason::TextCap),
            (
                64 * 1024,
                500,
                codex_index::IndexUnavailableReason::RowInvalid,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            let backup = root.path().join("backup.sqlite");
            recent_fixture(&backup, &repo).await;
            // Existing physical records get the new large literal lazily.
            mutate_recent(&backup, "ALTER TABLE threads RENAME COLUMN cwd TO old_cwd").await;
            let mut db =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
                    .await
                    .unwrap();
            sea_orm::sqlx::query("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<?) INSERT INTO threads SELECT printf('extra-%d',x),'/raw',old_cwd,0,'0.162.1',created_at_ms,updated_at_ms FROM n CROSS JOIN threads WHERE id='parent'")
                .bind(rows).execute(&mut db).await.unwrap();
            let alter = format!(
                "ALTER TABLE threads ADD COLUMN cwd TEXT NOT NULL DEFAULT '{}'",
                "x".repeat(bytes)
            );
            // SQLite DEFAULT cannot bind parameters. This literal contains only
            // the fixed ASCII x byte repeated by the two fixture constants.
            sea_orm::sqlx::query(sea_orm::sqlx::AssertSqlSafe(alter))
                .execute(&mut db)
                .await
                .unwrap();
            db.close().await.unwrap();
            assert!(std::fs::metadata(&backup).unwrap().len() < 1024 * 1024);
            let scratch = tempfile::tempdir().unwrap();
            let outcome = read_recent_outcome(&backup, &repo, scratch.path())
                .await
                .unwrap();
            assert!(
                matches!(outcome, codex_index::RecentIndexOutcome::Unavailable {reason} if reason==expected),
                "{bytes}/{rows}: {outcome:?}"
            );
        }
        // All receipt checksums must remain 48 bytes, including versions
        // whose digest value is not one of the pinned required migrations.
        // Reject lazy oversized values without copying them in full per row.
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let backup = root.path().join("checksums.sqlite");
        recent_fixture(&backup, &repo).await;
        let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
            .await
            .unwrap();
        sea_orm::sqlx::query("ALTER TABLE _sqlx_migrations RENAME COLUMN checksum TO old_checksum")
            .execute(&mut db)
            .await
            .unwrap();
        for version in 1..=59 {
            sea_orm::sqlx::query("INSERT OR IGNORE INTO _sqlx_migrations(version,success,old_checksum) VALUES(?,1,X'00')").bind(version).execute(&mut db).await.unwrap();
        }
        let alter = format!(
            "ALTER TABLE _sqlx_migrations ADD COLUMN checksum BLOB NOT NULL DEFAULT X'{}'",
            "ab".repeat(512 * 1024)
        );
        // Constant hex fixture bytes only; SQLite DDL cannot bind a DEFAULT.
        sea_orm::sqlx::query(sea_orm::sqlx::AssertSqlSafe(alter))
            .execute(&mut db)
            .await
            .unwrap();
        sea_orm::sqlx::query(
            "UPDATE _sqlx_migrations SET checksum=old_checksum WHERE version IN (1,5,21,25)",
        )
        .execute(&mut db)
        .await
        .unwrap();
        db.close().await.unwrap();
        assert!(std::fs::metadata(&backup).unwrap().len() < 2 * 1024 * 1024);
        let scratch = tempfile::tempdir().unwrap();
        assert!(read(&backup, scratch.path()).await.is_err());
        assert_eq!(
            read_recent_outcome(&backup, &repo, scratch.path())
                .await
                .unwrap(),
            codex_index::RecentIndexOutcome::Unavailable {
                reason: codex_index::IndexUnavailableReason::MigrationUnsupported,
            }
        );
        let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&backup))
            .await
            .unwrap();
        sea_orm::sqlx::query(
            "UPDATE _sqlx_migrations SET checksum=zeroblob(48) WHERE version NOT IN (1,5,21,25)",
        )
        .execute(&mut db)
        .await
        .unwrap();
        db.close().await.unwrap();
        assert_eq!(
            read(&backup, scratch.path()).await.unwrap().max_migration,
            59
        );
        assert!(matches!(
            read_recent_outcome(&backup, &repo, scratch.path())
                .await
                .unwrap(),
            codex_index::RecentIndexOutcome::Available { .. }
        ));
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_reconcile_index_error_display_and_provider_edge_status_are_fixed() {
        use codex_index::IndexUnavailableReason as Why;
        for (reason, expected) in [
            (Why::SchemaUnsupported, "index schema is unsupported"),
            (
                Why::MigrationUnsupported,
                "index migration history is unsupported",
            ),
            (Why::IntegrityFailed, "index backup integrity check failed"),
            (Why::ScopeRootUnproven, "index scope root cannot be proved"),
            (
                Why::TimestampMigration,
                "index millisecond migration is unavailable",
            ),
            (
                Why::TimestampSchema,
                "index millisecond schema is unavailable",
            ),
            (
                Why::TimestampInvalid,
                "index millisecond timestamps are invalid",
            ),
            (
                Why::FutureSkew,
                "index timestamps exceed the clock skew allowance",
            ),
            (Why::RowCap, "recent index thread limit exceeded"),
            (Why::RowInvalid, "recent index row validation failed"),
            (Why::TextCap, "recent index text limit exceeded"),
            (Why::EdgeCap, "recent index edge limit exceeded"),
            (Why::EdgeTextCap, "recent index edge text limit exceeded"),
            (
                Why::GeneratedSchema,
                "index generated schema is unsupported",
            ),
        ] {
            assert_eq!(reason.to_string(), expected);
            assert_eq!(serde_json::to_value(reason).unwrap(), reason.reason_key());
        }
        for (failure, key, display) in [
            (
                ReconciliationReadFailure::ScopeRootUnproven,
                "scope_root_unproven",
                "reconciliation root changed or cannot be proved; reopen the worktree and retry",
            ),
            (
                ReconciliationReadFailure::ResponseInconsistent,
                "response_inconsistent",
                "reconciliation helper response is inconsistent; retry with the current Libra build",
            ),
            (
                ReconciliationReadFailure::Budget,
                "budget_exhausted",
                "reconciliation exhausted its original time budget; retry with a smaller backup",
            ),
        ] {
            assert_eq!(failure.reason_key(), key);
            assert_eq!(failure.to_string(), display);
        }
        for value in [
            "\u{1b}[31mPRIVATE_EDGE_STATUS\n",
            "provider future status",
            "💭",
            "",
        ] {
            assert_eq!(
                codex_index::RecentEdgeStatus::from_provider(value),
                codex_index::RecentEdgeStatus::Other
            );
        }
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_reconcile_index_edge_text_never_crosses_recent_wire() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let path = root.path().join("backup.sqlite");
        recent_fixture(&path, &repo).await;
        let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        let unsafe_status = "\u{1b}[31mPRIVATE_EDGE_STATUS\n";
        sea_orm::sqlx::query("UPDATE thread_spawn_edges SET status=?")
            .bind(unsafe_status)
            .execute(&mut db)
            .await
            .unwrap();
        db.close().await.unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let recent = read_recent(&path, &repo, scratch.path()).await.unwrap();
        assert_eq!(
            recent.spawn_edges[0].status,
            codex_index::RecentEdgeStatus::Other
        );
        let wire = serde_json::to_string(&recent).unwrap();
        assert!(!wire.contains("PRIVATE_EDGE_STATUS"));
        assert!(!wire.contains("\\u001b"));
        // The old internal full-snapshot contract remains byte-data compatible.
        assert_eq!(
            read(&path, scratch.path()).await.unwrap().spawn_edges[0].status,
            unsafe_status
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_reconcile_index_hundreds_of_directories_work_with_low_fd_limit() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let path = root.path().join("backup.sqlite");
        recent_fixture(&path, &repo).await;
        let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        let mut txn = db.begin().await.unwrap();
        for index in 0..333 {
            let cwd = repo.join(format!("sub-{index}"));
            std::fs::create_dir(&cwd).unwrap();
            sea_orm::sqlx::query(
                "INSERT INTO threads VALUES(?,'/private/never-read',?,0,'0.162.1',?,?)",
            )
            .bind(format!("many-{index}"))
            .bind(cwd.to_str().unwrap())
            .bind(NOW_MS)
            .bind(NOW_MS)
            .execute(&mut *txn)
            .await
            .unwrap();
        }
        txn.commit().await.unwrap();
        db.close().await.unwrap();
        let executable = helper().to_str().unwrap().replace('\'', "'\"'\"'");
        let program = root.path().join("low-fd-helper");
        std::fs::write(
            &program,
            format!("#!/bin/sh\nulimit -n 256\nexec '{executable}' \"$@\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let query = codex_index::RecentScopeQuery {
            version: 1,
            now_ms: NOW_MS,
            root: repo,
            root_identity: None,
        };
        let result = SCRATCH_BASE
            .scope(
                scratch.path().to_path_buf(),
                authorized_read::with_test_helper_program(
                    program,
                    read_recent_closed_backup(
                        &path,
                        query,
                        Instant::now() + Duration::from_secs(5),
                    ),
                ),
            )
            .await
            .unwrap();
        let codex_index::RecentIndexOutcome::Available { snapshot } = result else {
            panic!("expected available");
        };
        assert_eq!(snapshot.candidate_count, 335);
        assert_eq!(snapshot.scope_ambiguous_count, 0);
        assert_eq!(snapshot.scope_unknown_count, 0);
        assert!(
            snapshot
                .threads
                .iter()
                .all(|thread| thread.scope == codex_index::RecentScopeStatus::Proven)
        );
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_unavailable_reasons_are_typed_and_safe() {
        use codex_index::{IndexUnavailableReason as Why, RecentIndexOutcome as Outcome};
        for (sql, expected) in [
            (
                "ALTER TABLE threads RENAME COLUMN cwd TO missing_cwd",
                Why::SchemaUnsupported,
            ),
            (
                "UPDATE _sqlx_migrations SET checksum=zeroblob(48) WHERE version=1",
                Why::MigrationUnsupported,
            ),
            (
                "UPDATE _sqlx_migrations SET checksum=zeroblob(49) WHERE version=5",
                Why::MigrationUnsupported,
            ),
            (
                "UPDATE _sqlx_migrations SET checksum=zeroblob(48) WHERE version=25",
                Why::TimestampMigration,
            ),
            (
                "ALTER TABLE threads RENAME COLUMN updated_at_ms TO unknown_ms",
                Why::TimestampSchema,
            ),
            (
                "UPDATE threads SET created_at_ms=NULL",
                Why::TimestampInvalid,
            ),
            (
                "UPDATE threads SET updated_at_ms=1792397100001",
                Why::FutureSkew,
            ),
            (
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO threads SELECT printf('new-%d',x),'/raw-private-rollout','/foreign',0,'0.162.1',1792396800000,1792396800000 FROM n",
                Why::RowCap,
            ),
            (
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<400) INSERT INTO threads SELECT printf('new-%d',x),'/raw-private-rollout',printf('%04000d',x),0,'0.162.1',1792396800000,1792396800000 FROM n",
                Why::TextCap,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            let db = root.path().join("backup.sqlite");
            recent_fixture(&db, &repo).await;
            mutate_recent(&db, sql).await;
            let scratch = tempfile::tempdir().unwrap();
            let outcome = read_recent_outcome(&db, &repo, scratch.path())
                .await
                .unwrap();
            assert_eq!(outcome, Outcome::Unavailable { reason: expected });
            let wire = serde_json::to_string(&outcome).unwrap();
            assert!(!wire.contains("rollout"));
            assert!(!wire.contains(repo.to_str().unwrap()));
            assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_integrity_failure_is_typed() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let db = root.path().join("backup.sqlite");
        recent_fixture(&db, &repo).await;
        // Make an otherwise readable SQLite file fail quick_check on reopen.
        for sql in [
            "CREATE TABLE integrity_fixture(value INTEGER)",
            "INSERT INTO integrity_fixture VALUES (NULL)",
            "PRAGMA writable_schema=ON",
            "UPDATE sqlite_schema SET sql='CREATE TABLE integrity_fixture(value INTEGER NOT NULL)' WHERE name='integrity_fixture'",
        ] {
            let mut connection =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&db))
                    .await
                    .unwrap();
            if sql.starts_with("UPDATE sqlite_schema") {
                sea_orm::sqlx::query("PRAGMA writable_schema=ON")
                    .execute(&mut connection)
                    .await
                    .unwrap();
            }
            sea_orm::sqlx::query(sql)
                .execute(&mut connection)
                .await
                .unwrap();
            connection.close().await.unwrap();
        }
        let before = std::fs::read(&db).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        assert_eq!(
            read_recent_outcome(&db, &repo, scratch.path())
                .await
                .unwrap(),
            codex_index::RecentIndexOutcome::Unavailable {
                reason: codex_index::IndexUnavailableReason::IntegrityFailed
            }
        );
        assert_eq!(std::fs::read(&db).unwrap(), before);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_millisecond_declarations_and_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let db = root.path().join("backup.sqlite");
        recent_fixture(&db, &repo).await;
        let scratch = tempfile::tempdir().unwrap();
        let cutoff = NOW_MS - codex_index::RECENT_WINDOW_MS;
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&db))
                .await
                .unwrap();
        sea_orm::sqlx::query("UPDATE threads SET created_at_ms=?,updated_at_ms=? WHERE id='child'")
            .bind(cutoff)
            .bind(cutoff)
            .execute(&mut connection)
            .await
            .unwrap();
        sea_orm::sqlx::query(
            "UPDATE threads SET created_at_ms=?,updated_at_ms=? WHERE id='parent'",
        )
        .bind(NOW_MS + 300000)
        .bind(NOW_MS + 300000)
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        let result = read_recent(&db, &repo, scratch.path()).await.unwrap();
        assert_eq!(result.candidate_count, 2);
        assert_eq!(result.threads[0].id, "parent");
        assert_eq!(result.threads[1].updated_at_ms, cutoff);
        // A TEXT column is not made trustworthy merely by integer-looking data.
        mutate_recent(
            &db,
            "ALTER TABLE threads RENAME COLUMN updated_at_ms TO old_ms",
        )
        .await;
        mutate_recent(&db, "ALTER TABLE threads ADD COLUMN updated_at_ms TEXT").await;
        mutate_recent(&db, "UPDATE threads SET updated_at_ms=old_ms").await;
        let result = read_recent_outcome(&db, &repo, scratch.path())
            .await
            .unwrap();
        assert_eq!(
            result,
            codex_index::RecentIndexOutcome::Unavailable {
                reason: codex_index::IndexUnavailableReason::TimestampSchema
            }
        );
        mutate_recent(&db, "ALTER TABLE threads DROP COLUMN updated_at_ms").await;
        mutate_recent(&db, "ALTER TABLE threads ADD COLUMN updated_at_ms INTEGER GENERATED ALWAYS AS(old_ms) VIRTUAL").await;
        let result = read_recent_outcome(&db, &repo, scratch.path())
            .await
            .unwrap();
        assert_eq!(
            result,
            codex_index::RecentIndexOutcome::Unavailable {
                reason: codex_index::IndexUnavailableReason::GeneratedSchema
            }
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_index_recent_response_invariants_are_revalidated() {
        let query = codex_index::RecentScopeQuery {
            version: 1,
            now_ms: NOW_MS,
            root: PathBuf::from("/repo"),
            root_identity: None,
        };
        let good = serde_json::json!({
            "status":"available", "snapshot": {
                "max_migration":25, "window_anchor_ms":NOW_MS,
                "cutoff_ms":NOW_MS-codex_index::RECENT_WINDOW_MS,
                "backup_max_updated_at_ms":NOW_MS, "freshness":"within_window",
                "candidate_count":1, "scope_ambiguous_count":0, "scope_unknown_count":0,
                "threads":[{"id":"parent","created_at_ms":NOW_MS,"updated_at_ms":NOW_MS,"archived":false,"scope":"proven"}],
                "spawn_edges":[]
            }
        });
        validate_recent_response(&serde_json::from_value(good.clone()).unwrap(), &query).unwrap();
        for (field, bad) in [
            ("window_anchor_ms", serde_json::json!(NOW_MS - 1)),
            ("cutoff_ms", serde_json::json!(0)),
            ("candidate_count", serde_json::json!(0)),
            ("scope_ambiguous_count", serde_json::json!(1)),
            ("backup_max_updated_at_ms", serde_json::json!(NOW_MS - 1)),
            ("freshness", serde_json::json!("empty_snapshot")),
            ("max_migration", serde_json::json!(60)),
        ] {
            let mut value = good.clone();
            value["snapshot"][field] = bad;
            assert!(
                validate_recent_response(&serde_json::from_value(value).unwrap(), &query).is_err(),
                "{field}"
            );
        }
        let mut bad = good;
        bad["snapshot"]["spawn_edges"] = serde_json::json!([{"parent_thread_id":"parent","child_thread_id":"foreign","status":"completed"}]);
        assert!(validate_recent_response(&serde_json::from_value(bad).unwrap(), &query).is_err());
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_scope_memo_avoids_repeated_directory_scans() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        for index in 0..3000 {
            std::fs::write(sub.join(format!("entry-{index}")), b"").unwrap();
        }
        let root_path = source::normalize_macos_system_directory_alias(&repo);
        let pin = source::open_absolute_directory_no_follow(&root_path).unwrap();
        let mut memo = std::collections::HashMap::new();
        let end = Instant::now() + Duration::from_secs(2);
        for _ in 0..2000 {
            check_scope_descendants(&pin, Path::new("sub"), &mut memo, end).unwrap();
        }
        assert_eq!(memo.len(), 1);
        // A shared prefix is still retained by exactly the first inspection.
        std::fs::create_dir(sub.join("next")).unwrap();
        check_scope_descendants(&pin, Path::new("sub/next"), &mut memo, end).unwrap();
        assert_eq!(memo.len(), 2);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_case_variant_markers_and_metadata_cwd_are_unknown() {
        for (marker, cwd_suffix) in [
            (".GIT", "sub"),
            (".Libra", "sub"),
            (".git", "sub/deeper"),
            (".libra", ".libra"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            std::fs::create_dir_all(repo.join("sub/deeper")).unwrap();
            let path = repo.join(cwd_suffix);
            if cwd_suffix == ".libra" {
                std::fs::create_dir(&path).unwrap();
            } else {
                std::fs::write(repo.join("sub").join(marker), b"nested").unwrap();
            }
            let db = root.path().join("backup.sqlite");
            recent_fixture(&db, &path).await;
            let scratch = tempfile::tempdir().unwrap();
            let result = read_recent(&db, &repo, scratch.path()).await.unwrap();
            assert_eq!(result.candidate_count, 2);
            assert_eq!(result.scope_ambiguous_count, 0, "{marker}/{cwd_suffix}");
            assert_eq!(result.scope_unknown_count, 2, "{marker}/{cwd_suffix}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_root_replacement_cannot_supply_scope_authority() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let db = root.path().join("backup.sqlite");
        recent_fixture(&db, &repo).await;
        let program = root.path().join("slow-helper");
        let ready = root.path().join("helper-ready");
        python_helper(
            &program,
            &format!(
                "import time\nfrom pathlib import Path\nPath({:?}).write_text('ready')\ntime.sleep(0.4)\nprint('{{\"status\":\"unavailable\",\"reason\":\"timestamp_invalid\"}}')",
                ready.to_str().unwrap()
            ),
        );
        let scratch = tempfile::tempdir().unwrap();
        let query = codex_index::RecentScopeQuery {
            version: 1,
            now_ms: NOW_MS,
            root: repo.clone(),
            root_identity: None,
        };
        let read = SCRATCH_BASE.scope(
            scratch.path().to_path_buf(),
            authorized_read::with_test_helper_program(
                program,
                read_recent_closed_backup(&db, query, Instant::now() + Duration::from_secs(3)),
            ),
        );
        let swap = async {
            let until = Instant::now() + Duration::from_secs(2);
            while !ready.exists() {
                assert!(Instant::now() < until, "helper did not start");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            std::fs::rename(&repo, root.path().join("old-repo")).unwrap();
            std::fs::create_dir(&repo).unwrap();
        };
        let (result, ()) = tokio::join!(read, swap);
        assert!(result.unwrap_err().to_string().contains("root changed"));
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        for attack in ["gone", "link"] {
            let isolated = tempfile::tempdir().unwrap();
            let repo = isolated.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            let backup = isolated.path().join("backup.sqlite");
            recent_fixture(&backup, &repo).await;
            let saved = isolated.path().join("saved-repo");
            std::fs::rename(&repo, &saved).unwrap();
            if attack == "link" {
                std::os::unix::fs::symlink(&saved, &repo).unwrap();
            }
            let scratch = tempfile::tempdir().unwrap();
            assert!(
                read_recent(&backup, &repo, scratch.path()).await.is_err(),
                "{attack}"
            );
            assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        }
    }
    const TIMESTAMP_DIGEST: &str = "d6fb51d79941b93c3278fe2a8a0c6fa882407b7b716e32d72329f0b7b60c181a42f49e56cd552315efb0402a111ede12";

    async fn recent_fixture(path: &Path, repo: &Path) {
        fixture(path, false).await;
        let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
            .await
            .unwrap();
        for sql in [
            "ALTER TABLE threads ADD COLUMN created_at_ms INTEGER",
            "ALTER TABLE threads ADD COLUMN updated_at_ms INTEGER",
        ] {
            sea_orm::sqlx::query(sql).execute(&mut db).await.unwrap();
        }
        sea_orm::sqlx::query("INSERT INTO _sqlx_migrations VALUES(25,1,?)")
            .bind(hex::decode(TIMESTAMP_DIGEST).unwrap())
            .execute(&mut db)
            .await
            .unwrap();
        sea_orm::sqlx::query("UPDATE threads SET cwd=?,created_at_ms=?,updated_at_ms=?")
            .bind(repo.to_str().unwrap())
            .bind(NOW_MS - 2000)
            .bind(NOW_MS - 1000)
            .execute(&mut db)
            .await
            .unwrap();
        db.close().await.unwrap();
    }

    async fn mutate_recent(path: &Path, sql: &'static str) {
        let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
            .await
            .unwrap();
        sea_orm::sqlx::query(sql).execute(&mut db).await.unwrap();
        db.close().await.unwrap();
    }

    async fn read_recent_outcome(
        path: &Path,
        repo: &Path,
        scratch: &Path,
    ) -> Result<codex_index::RecentIndexOutcome> {
        SCRATCH_BASE
            .scope(
                scratch.to_path_buf(),
                authorized_read::with_test_helper_program(
                    helper(),
                    read_recent_closed_backup(
                        path,
                        codex_index::RecentScopeQuery {
                            version: 1,
                            now_ms: NOW_MS,
                            root: repo.to_path_buf(),
                            root_identity: None,
                        },
                        Instant::now() + Duration::from_secs(5),
                    ),
                ),
            )
            .await
    }

    async fn read_recent(
        path: &Path,
        repo: &Path,
        scratch: &Path,
    ) -> Result<codex_index::RecentIndexSnapshot> {
        match read_recent_outcome(path, repo, scratch).await? {
            codex_index::RecentIndexOutcome::Available { snapshot } => Ok(snapshot),
            codex_index::RecentIndexOutcome::Unavailable { reason } => Err(reason.into()),
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_index_recent_control_preserves_old_wire_and_rejects_unknown_fields() {
        let old = serde_json::json!({
            "mode": MODE, "scratch": "/scratch", "scratch_identity": [1,2],
            "remaining_ms": 1000, "source_len": 4096, "header": [1,2,3]
        });
        let parsed: Control = serde_json::from_value(old.clone()).unwrap();
        assert!(parsed.recent_scope.is_none());
        assert_eq!(serde_json::to_value(parsed).unwrap(), old);
        let parsed: Control = serde_json::from_value(old.clone()).unwrap();
        let wire = serde_json::to_string(&parsed).unwrap();
        assert_eq!(
            wire,
            r#"{"mode":"codex-offline-index-copy-v1","scratch":"/scratch","scratch_identity":[1,2],"remaining_ms":1000,"source_len":4096,"header":[1,2,3]}"#
        );
        let mut invalid = old.clone();
        invalid["unregistered"] = true.into();
        assert!(serde_json::from_value::<Control>(invalid).is_err());
        let mut recent = old;
        recent["recent_scope"] = serde_json::json!({"version":1,"now_ms":NOW_MS,"root":"/repo"});
        let parsed: Control = serde_json::from_value(recent.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), recent);
        recent["recent_scope"]["purpose"] = "unknown".into();
        assert!(serde_json::from_value::<Control>(recent).is_err());
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_timestamp_migration_does_not_change_inspect_all() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let path = root.path().join("backup.sqlite");
        fixture(&path, false).await;
        let scratch = tempfile::tempdir().unwrap();
        let old = read(&path, scratch.path()).await.unwrap();
        assert_eq!(old.max_migration, 21);
        assert!(read_recent(&path, &repo, scratch.path()).await.is_err());
        assert_eq!(read(&path, scratch.path()).await.unwrap(), old);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        let current_path = root.path().join("current.sqlite");
        recent_fixture(&current_path, &repo).await;
        let current = read(&current_path, scratch.path()).await.unwrap();
        assert_eq!(current.max_migration, 25);
        assert_eq!(current.threads.len(), 2);
        assert_eq!(current.spawn_edges.len(), 1);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_scope_is_readonly_and_omits_rollout_paths() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let path = root.path().join("backup.sqlite");
        recent_fixture(&path, &repo).await;
        let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        sea_orm::sqlx::query("UPDATE threads SET cwd=? WHERE id='child'")
            .bind(sub.to_str().unwrap())
            .execute(&mut db)
            .await
            .unwrap();
        // A lexically foreign path is excluded without touching that path.
        sea_orm::sqlx::query(
            "INSERT INTO threads VALUES('foreign','/NEVER_READ_PRIVATE_SOURCE',?,0,'0.162.1',?,?)",
        )
        .bind(root.path().join("foreign-unreadable").to_str().unwrap())
        .bind(NOW_MS)
        .bind(NOW_MS)
        .execute(&mut db)
        .await
        .unwrap();
        db.close().await.unwrap();
        let before = std::fs::read(&path).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let result = read_recent(&path, &repo, scratch.path()).await.unwrap();
        assert_eq!(result.max_migration, 25);
        assert_eq!(result.candidate_count, 2);
        assert_eq!(result.scope_ambiguous_count, 0);
        assert_eq!(result.freshness, codex_index::IndexFreshness::WithinWindow);
        assert_eq!(result.backup_max_updated_at_ms, Some(NOW_MS));
        assert!(
            result
                .threads
                .iter()
                .all(|thread| thread.scope == codex_index::RecentScopeStatus::Proven)
        );
        assert!(result.threads.iter().any(|thread| thread.archived));
        assert_eq!(result.spawn_edges.len(), 1);
        let output = serde_json::to_string(&result).unwrap();
        assert!(!output.contains("rollout"));
        assert!(!output.contains("cwd"));
        assert!(!output.contains(repo.to_str().unwrap()));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_invalid_old_timestamps_cannot_hide_before_filter() {
        for mutation in [
            "UPDATE threads SET updated_at_ms=NULL WHERE id='child'",
            "UPDATE threads SET created_at_ms=NULL,updated_at_ms=1 WHERE id='child'",
            "UPDATE threads SET created_at_ms='bad-text',updated_at_ms=1 WHERE id='child'",
            "UPDATE threads SET created_at_ms=1.5,updated_at_ms=1 WHERE id='child'",
            "UPDATE threads SET updated_at_ms='bad-text' WHERE id='child'",
            "UPDATE threads SET updated_at_ms=1.5 WHERE id='child'",
            "UPDATE threads SET created_at_ms=-1,updated_at_ms=1 WHERE id='child'",
            "UPDATE threads SET updated_at_ms=1792397100001 WHERE id='child'",
            "UPDATE threads SET created_at_ms=9223372036854775807,updated_at_ms=1 WHERE id='child'",
            "UPDATE _sqlx_migrations SET checksum=zeroblob(48) WHERE version=25",
            "DELETE FROM _sqlx_migrations WHERE version=25",
            "ALTER TABLE threads RENAME COLUMN updated_at_ms TO old_updated_at",
        ] {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            let path = root.path().join("backup.sqlite");
            recent_fixture(&path, &repo).await;
            mutate_recent(&path, mutation).await;
            let before = std::fs::read(&path).unwrap();
            let scratch = tempfile::tempdir().unwrap();
            assert!(
                read_recent(&path, &repo, scratch.path()).await.is_err(),
                "{mutation}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_empty_and_stale_snapshots_are_distinct() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let path = root.path().join("backup.sqlite");
        recent_fixture(&path, &repo).await;
        mutate_recent(&path, "UPDATE threads SET created_at_ms=1,updated_at_ms=1").await;
        let scratch = tempfile::tempdir().unwrap();
        let stale = read_recent(&path, &repo, scratch.path()).await.unwrap();
        assert_eq!(stale.freshness, codex_index::IndexFreshness::OutsideWindow);
        assert_eq!(stale.backup_max_updated_at_ms, Some(1));
        assert_eq!(stale.candidate_count, 0);
        mutate_recent(&path, "DELETE FROM threads").await;
        let empty = read_recent(&path, &repo, scratch.path()).await.unwrap();
        assert_eq!(empty.freshness, codex_index::IndexFreshness::EmptySnapshot);
        assert_eq!(empty.backup_max_updated_at_ms, None);
        assert_eq!(empty.candidate_count, 0);
        // Orphaned old spawn rows cannot invent recent child candidates.
        assert!(empty.spawn_edges.is_empty());
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_window_precedes_thread_and_edge_limits() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let path = root.path().join("backup.sqlite");
        recent_fixture(&path, &repo).await;
        mutate_recent(&path, "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO threads SELECT printf('old-%d',x),'/old/rollout','/old/repo',0,'0.162.1',1,1 FROM n").await;
        mutate_recent(&path, "INSERT INTO thread_spawn_edges SELECT 'old-parent',id,'completed' FROM threads WHERE id LIKE 'old-%'").await;
        let scratch = tempfile::tempdir().unwrap();
        let result = read_recent(&path, &repo, scratch.path()).await.unwrap();
        assert_eq!(result.candidate_count, 2);
        assert_eq!(result.spawn_edges.len(), 1);
        mutate_recent(&path, "UPDATE threads SET updated_at_ms=1792396800000").await;
        assert!(read_recent(&path, &repo, scratch.path()).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_recent_nested_link_gone_and_case_ambiguous_are_unknown() {
        use std::os::unix::fs::symlink;
        for kind in ["nested", "link", "gone", "case", "unicode"]
            .into_iter()
            .chain(cfg!(target_os = "macos").then_some("data"))
        {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join(if kind == "unicode" {
                "caf\u{e9}"
            } else {
                "repo"
            });
            let sub = repo.join("sub");
            std::fs::create_dir_all(&sub).unwrap();
            let path = root.path().join("backup.sqlite");
            recent_fixture(&path, &sub).await;
            let supplied = match kind {
                "case" => repo.with_file_name("REPO").join("sub"),
                "unicode" => repo.with_file_name("cafe\u{301}").join("sub"),
                "data" => Path::new("/System/Volumes/Data").join(sub.strip_prefix("/").unwrap()),
                _ => sub.clone(),
            };
            let mut db =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
                    .await
                    .unwrap();
            sea_orm::sqlx::query("UPDATE threads SET cwd=?")
                .bind(supplied.to_str().unwrap())
                .execute(&mut db)
                .await
                .unwrap();
            db.close().await.unwrap();
            match kind {
                "nested" => {
                    std::fs::write(sub.join(".git"), b"later nested repo").unwrap();
                }
                "link" => {
                    std::fs::remove_dir(&sub).unwrap();
                    let foreign = root.path().join("foreign");
                    std::fs::create_dir(&foreign).unwrap();
                    symlink(&foreign, &sub).unwrap();
                }
                "gone" => {
                    std::fs::remove_dir(&sub).unwrap();
                }
                "case" | "unicode" | "data" => {}
                _ => unreachable!(),
            }
            let scratch = tempfile::tempdir().unwrap();
            let result = read_recent(&path, &repo, scratch.path()).await.unwrap();
            assert_eq!(result.candidate_count, 2, "{kind}");
            assert_eq!(
                result.scope_ambiguous_count,
                if matches!(kind, "case" | "unicode" | "data") {
                    2
                } else {
                    0
                },
                "{kind}"
            );
            assert_eq!(
                result.scope_unknown_count,
                if matches!(kind, "case" | "unicode" | "data") {
                    0
                } else {
                    2
                },
                "{kind}"
            );
            assert!(
                result
                    .threads
                    .iter()
                    .all(|thread| thread.scope != codex_index::RecentScopeStatus::Proven),
                "{kind}"
            );
            assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        }
    }

    fn helper() -> PathBuf {
        let path = crate::utils::test::real_libra_helper_binary();
        assert!(
            path.is_file(),
            "build current-source Libra before local index tests"
        );
        path
    }

    async fn fixture(path: &Path, wal: bool) {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(if wal {
                SqliteJournalMode::Wal
            } else {
                SqliteJournalMode::Delete
            });
        let mut db = SqliteConnection::connect_with(&options).await.unwrap();
        for sql in [
            "CREATE TABLE _sqlx_migrations (version INTEGER PRIMARY KEY, success BOOLEAN NOT NULL, checksum BLOB NOT NULL)",
            "CREATE TABLE threads(id TEXT PRIMARY KEY,rollout_path TEXT NOT NULL,cwd TEXT NOT NULL,archived INTEGER NOT NULL,cli_version TEXT NOT NULL)",
            "CREATE TABLE thread_spawn_edges(parent_thread_id TEXT NOT NULL,child_thread_id TEXT PRIMARY KEY,status TEXT NOT NULL)",
            "INSERT INTO threads VALUES ('parent','/data/rollout-parent.jsonl','/work/repo',0,'0.162.1'),('child','/data/rollout-child.jsonl','/work/repo',1,'0.162.1')",
            "INSERT INTO thread_spawn_edges VALUES ('parent','child','completed')",
        ] {
            sea_orm::sqlx::query(sql).execute(&mut db).await.unwrap();
        }
        for (version, digest) in codex_index::required_migrations_for_local_test() {
            sea_orm::sqlx::query("INSERT INTO _sqlx_migrations VALUES (?,1,?)")
                .bind(version)
                .bind(hex::decode(digest).unwrap())
                .execute(&mut db)
                .await
                .unwrap();
        }
        if wal {
            sea_orm::sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&mut db)
                .await
                .unwrap();
        }
        db.close().await.unwrap();
    }

    async fn read(path: &Path, scratch: &Path) -> Result<codex_index::IndexSnapshot> {
        SCRATCH_BASE
            .scope(
                scratch.to_path_buf(),
                authorized_read::with_test_helper_program(
                    helper(),
                    read_closed_backup(path, Instant::now() + Duration::from_secs(5)),
                ),
            )
            .await
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_closed_delete_and_wal_backups_are_readonly() {
        for wal in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let db = root.path().join("backup.sqlite");
            fixture(&db, wal).await;
            let scratch = tempfile::tempdir().unwrap();
            let bytes = std::fs::read(&db).unwrap();
            assert_eq!(bytes[18], if wal { 2 } else { 1 });
            let digest = Sha256::digest(&bytes);
            let before = stamp(&File::open(&db).unwrap()).unwrap();
            let result = read(&db, scratch.path()).await.unwrap();
            assert_eq!(result.max_migration, 21);
            assert_eq!(result.threads.len(), 2);
            assert_eq!(result.spawn_edges[0].parent_thread_id, "parent");
            assert_eq!(result.spawn_edges[0].child_thread_id, "child");
            assert!(
                result
                    .threads
                    .iter()
                    .any(|thread| thread.id == "child" && thread.archived)
            );
            assert!(before == stamp(&File::open(&db).unwrap()).unwrap());
            assert_eq!(digest, Sha256::digest(std::fs::read(&db).unwrap()));
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
            assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_sidecars_and_truncated_backup_are_unavailable() {
        for suffix in ["-wal", "-shm", "-journal"] {
            let root = tempfile::tempdir().unwrap();
            let db = root.path().join("backup.sqlite");
            fixture(&db, false).await;
            let sidecar = root.path().join(format!("backup.sqlite{suffix}"));
            std::fs::write(&sidecar, b"invalid sidecar").unwrap();
            let result = read(&db, root.path()).await.unwrap_err();
            assert!(result.to_string().contains("sidecar"));
            assert_eq!(std::fs::read(&sidecar).unwrap(), b"invalid sidecar");
        }
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("backup.sqlite");
        fixture(&db, false).await;
        let file = std::fs::OpenOptions::new().write(true).open(&db).unwrap();
        file.set_len(file.metadata().unwrap().len() - 1).unwrap();
        assert!(read(&db, root.path()).await.is_err());
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_hard_link_and_virtual_schema_are_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("backup.sqlite");
        fixture(&db, false).await;
        let linked = root.path().join("hard-linked.sqlite");
        std::fs::hard_link(&db, &linked).unwrap();
        assert!(
            read(&linked, root.path())
                .await
                .unwrap_err()
                .to_string()
                .contains("hard link")
        );
        std::fs::remove_file(linked).unwrap();
        let mut connection =
            SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&db))
                .await
                .unwrap();
        sea_orm::sqlx::query("DROP TABLE threads")
            .execute(&mut connection)
            .await
            .unwrap();
        sea_orm::sqlx::query(
            "CREATE VIRTUAL TABLE threads USING fts5(id, rollout_path, cwd, archived, cli_version)",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&db)
                .read_only(true)
                .immutable(true),
        )
        .await
        .unwrap();
        let integrity: String = sea_orm::sqlx::query_scalar("PRAGMA quick_check(1)")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
        let error =
            codex_index::query_snapshot(&mut connection, Instant::now() + Duration::from_secs(2))
                .await
                .unwrap_err();
        assert_eq!(error.to_string(), "index schema is unavailable");
        connection.close().await.unwrap();
        // A table-like virtual schema cannot turn missing ordinary tables into
        // a successful zero-thread snapshot, even if quick_check succeeds.
        let scratch = tempfile::tempdir().unwrap();
        assert!(read(&db, scratch.path()).await.is_err());
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_header_probe_preserves_shared_descriptor_offset() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("backup.sqlite");
        fixture(&db, false).await;
        let mut file = File::open(&db).unwrap();
        let mut child_descriptor = file.try_clone().unwrap();
        child_descriptor.seek(SeekFrom::Start(777)).unwrap();
        let bytes = header(&file).unwrap();
        assert_eq!(&bytes[..16], b"SQLite format 3\0");
        assert_eq!(file.stream_position().unwrap(), 777);
        assert_eq!(child_descriptor.stream_position().unwrap(), 777);
    }

    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_future_failed_checksum_and_row_limit_are_unavailable() {
        for mutation in [
            "INSERT INTO _sqlx_migrations VALUES (60,1,zeroblob(48))",
            "UPDATE _sqlx_migrations SET success=0 WHERE version=5",
            "UPDATE _sqlx_migrations SET checksum=zeroblob(48) WHERE version=21",
            "DELETE FROM _sqlx_migrations WHERE version=1",
            "ALTER TABLE threads RENAME COLUMN cwd TO missing_cwd",
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO threads SELECT printf('row-%d',x),'/data/rollout','/work/repo',0,'0.162.1' FROM n",
        ] {
            let root = tempfile::tempdir().unwrap();
            let db = root.path().join("backup.sqlite");
            fixture(&db, false).await;
            let mut connection =
                SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&db))
                    .await
                    .unwrap();
            sea_orm::sqlx::query(mutation)
                .execute(&mut connection)
                .await
                .unwrap();
            connection.close().await.unwrap();
            let before = std::fs::read(&db).unwrap();
            let scratch = tempfile::tempdir().unwrap();
            assert!(
                read(&db, scratch.path()).await.is_err(),
                "must be unavailable: {mutation}"
            );
            assert_eq!(std::fs::read(&db).unwrap(), before);
            assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_links_and_hot_writer_are_unavailable() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("backup.sqlite");
        fixture(&db, false).await;
        let linked = root.path().join("link.sqlite");
        symlink(&db, &linked).unwrap();
        assert!(read(&linked, root.path()).await.is_err());
        let linked_root = root.path().join("linked-root");
        symlink(root.path(), &linked_root).unwrap();
        assert!(
            read(&linked_root.join("backup.sqlite"), root.path())
                .await
                .is_err()
        );
        let mut writer = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&db))
            .await
            .unwrap();
        sea_orm::sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut writer)
            .await
            .unwrap();
        sea_orm::sqlx::query("UPDATE threads SET cwd='/writing'")
            .execute(&mut writer)
            .await
            .unwrap();
        assert!(
            read(&db, root.path())
                .await
                .unwrap_err()
                .to_string()
                .contains("sidecar")
        );
        sea_orm::sqlx::query("ROLLBACK")
            .execute(&mut writer)
            .await
            .unwrap();
        writer.close().await.unwrap();
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_index_repeated_sidecar_check_sees_new_entries() {
        let root = tempfile::tempdir().unwrap();
        let path = source::normalize_macos_system_directory_alias(root.path());
        let directory = source::open_absolute_directory_no_follow(&path).unwrap();
        let end = Instant::now() + Duration::from_secs(1);
        without_sidecars(&directory, "backup.sqlite".as_ref(), end).unwrap();
        std::fs::write(path.join("backup.sqlite-wal"), b"late writer").unwrap();
        assert!(without_sidecars(&directory, "backup.sqlite".as_ref(), end).is_err());
    }

    #[cfg(unix)]
    fn python_helper(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/usr/bin/python3\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_concurrent_change_rejects_even_successful_empty_response() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("backup.sqlite");
        fixture(&db, false).await;
        let program = root.path().join("bounded-test-helper");
        python_helper(
            &program,
            "import time\ntime.sleep(0.4)\nprint('{\"max_migration\":21,\"threads\":[],\"spawn_edges\":[]}')",
        );
        let scratch = tempfile::tempdir().unwrap();
        let read = SCRATCH_BASE.scope(
            scratch.path().to_path_buf(),
            authorized_read::with_test_helper_program(
                program,
                read_closed_backup(&db, Instant::now() + Duration::from_secs(3)),
            ),
        );
        let write = async {
            tokio::time::sleep(Duration::from_millis(70)).await;
            std::fs::OpenOptions::new()
                .append(true)
                .open(&db)
                .unwrap()
                .write_all(b"concurrent writer")
                .unwrap();
        };
        let (result, ()) = tokio::join!(read, write);
        assert!(result.unwrap_err().to_string().contains("changed"));
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_index_deadline_reaps_helper_before_scratch_cleanup() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("backup.sqlite");
        fixture(&db, false).await;
        let program = root.path().join("bounded-test-helper");
        python_helper(
            &program,
            &format!(
                "import os,json,time\nc=json.loads(os.environ[{}])\nf=os.open(c['scratch']+'/index.sqlite',os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600)\nos.write(f,(str(os.getpid())+'\\n').encode())\nos.close(f)\ntime.sleep(30)",
                serde_json::to_string(authorized_read::AUTHORIZED_READ_HELPER_MODE_ENV).unwrap(),
            ),
        );
        let scratch = tempfile::tempdir().unwrap();
        let end = Instant::now() + Duration::from_millis(1200);
        let read = SCRATCH_BASE.scope(
            scratch.path().to_path_buf(),
            authorized_read::with_test_helper_program(program, read_closed_backup(&db, end)),
        );
        let observe = async {
            loop {
                for dir in std::fs::read_dir(scratch.path()).unwrap() {
                    let path = dir.unwrap().path();
                    if let Ok(bytes) = std::fs::read(path.join(COPY_LEAF))
                        && let Some(pid_bytes) = bytes.strip_suffix(b"\n")
                    {
                        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o077, 0);
                        assert_eq!(
                            path.join(COPY_LEAF)
                                .metadata()
                                .unwrap()
                                .permissions()
                                .mode()
                                & 0o077,
                            0
                        );
                        return std::str::from_utf8(pid_bytes)
                            .unwrap()
                            .parse::<i32>()
                            .unwrap();
                    }
                }
                assert!(
                    Instant::now() < end,
                    "helper never created its functional copy"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let (result, pid) = tokio::join!(read, observe);
        assert!(result.is_err());
        let reap_end = Instant::now() + Duration::from_secs(3);
        while std::fs::read_dir(scratch.path()).unwrap().next().is_some() {
            assert!(
                Instant::now() < reap_end,
                "private scratch survived helper reaping"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // SAFETY: signal zero only probes this test helper's PID; no signal is sent.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "scratch removed before child was reaped"
        );
    }
}
