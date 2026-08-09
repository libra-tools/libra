//! Session storage: save and load sessions from disk.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{
    file_history::FileHistoryStore,
    jsonl::{SessionEvent, SessionJsonlStore},
    migration::LegacySessionMigrator,
    state::SessionState,
};

const SESSION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const STALE_SESSION_LOCK_AGE: Duration = Duration::from_secs(30);
const THREAD_ID_METADATA_KEYS: &[&str] = &["thread_id", "threadId", "canonical_thread_id"];

/// Manages session persistence on disk.
///
/// Sessions are stored as append-only JSONL streams in a sessions directory.
pub struct SessionStore {
    sessions_dir: PathBuf,
    /// `true` when this store was constructed via
    /// [`Self::from_storage_path_with_subdir`] — the only constructor that
    /// permits [`Self::adopt_legacy_subdir_session_if_needed`] to migrate
    /// from the pre-partition `sessions/<id>/` layout.
    ///
    /// We track intent explicitly rather than inferring from the path
    /// shape because a storage root that is itself named `sessions` would
    /// otherwise pass a path-basename guard and cause spurious migrations
    /// for non-partitioned stores.
    partitioned: bool,
}

#[derive(Debug)]
pub struct SessionFileLock {
    lock_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadataBackfillReport {
    pub scanned: usize,
    pub updates: Vec<SessionMetadataBackfillUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadataBackfillUpdate {
    pub session_id: String,
    pub working_dir: String,
    pub legacy_session_id: String,
    pub canonical_thread_id: Option<String>,
    pub provider_thread_id: Option<String>,
}

impl Drop for SessionFileLock {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.lock_path)
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.lock_path.display(),
                error = %err,
                "failed to release session lock"
            );
        }
    }
}

impl SessionStore {
    /// Create a store rooted at `{working_dir}/.libra/sessions/`.
    pub fn new(working_dir: &Path) -> Self {
        Self {
            sessions_dir: working_dir.join(".libra").join("sessions"),
            partitioned: false,
        }
    }

    /// Create a store rooted at `{storage_path}/sessions/`.
    pub fn from_storage_path(storage_path: &Path) -> Self {
        Self {
            sessions_dir: storage_path.join("sessions"),
            partitioned: false,
        }
    }

    /// Create a store rooted at `{storage_path}/sessions/{subdir}/`. Used
    /// by CEX-EntireIO Phase 3 (`docs/development/commands/_general.md` §11.5) to
    /// keep `libra code` and `libra agent` session locks in disjoint
    /// subtrees so their `.lock` files cannot collide. Pre-existing
    /// callers stay on [`Self::from_storage_path`] which preserves the
    /// historical layout.
    pub fn from_storage_path_with_subdir(storage_path: &Path, subdir: &str) -> Self {
        Self {
            sessions_dir: storage_path.join("sessions").join(subdir),
            partitioned: true,
        }
    }

    /// Move a single legacy session entry from `{sessions_dir}/../{id}` into
    /// `{sessions_dir}/{id}` if the legacy entry exists and the new entry
    /// does not. Returns `Ok(true)` if a migration happened, `Ok(false)` if
    /// nothing to do.
    ///
    /// Designed for the CEX-EntireIO Phase 3 partition step (entire.md
    /// §11.2): when an agent capture SessionStore is constructed under a
    /// `agent/` subdir, we may still find old session directories at the
    /// pre-partition path. Moving them in place keeps in-flight hook
    /// lifecycles intact rather than starting a fresh session and losing
    /// continuity.
    ///
    /// Round-1 review fixes:
    /// - Adoption is gated on the explicit `partitioned` flag rather than
    ///   on a path-basename guess; a storage root that itself happens to
    ///   be named `sessions/` will not falsely qualify.
    /// - When a non-stale legacy lock exists, the migration is skipped
    ///   entirely — that lock signals a writer that may still be
    ///   appending to the legacy directory, and renaming the directory
    ///   from under it would split the event stream. The next adoption
    ///   attempt picks the migration up after the writer drops or the
    ///   lock ages out.
    /// - The legacy lock file is *not* renamed into the new tree. The
    ///   subsequent `lock_session` call acquires a fresh lock at the new
    ///   path; carrying over a possibly-stale lock would cause that
    ///   fresh acquisition to wait the full timeout on a held lock.
    pub fn adopt_legacy_subdir_session_if_needed(&self, id: &str) -> io::Result<bool> {
        if !self.partitioned {
            return Ok(false);
        }
        let Some(parent) = self.sessions_dir.parent() else {
            return Ok(false);
        };
        let legacy_root = parent.join(id);
        let new_root = self.sessions_dir.join(id);
        if !legacy_root.exists() || new_root.exists() {
            return Ok(false);
        }
        // If a legacy lock file is present and not stale, treat that as
        // signal of an in-flight writer on the old path. Skip and try
        // again next time — once the writer drops the lock (or the lock
        // ages out per `STALE_SESSION_LOCK_AGE`), adoption will pick up
        // automatically.
        let legacy_lock = parent.join(format!("{id}.lock"));
        if legacy_lock.exists() && !self.is_stale_lock_at(&legacy_lock) {
            return Ok(false);
        }
        fs::create_dir_all(&self.sessions_dir)?;
        fs::rename(&legacy_root, &new_root)?;
        // The legacy lock file is intentionally removed (best-effort)
        // rather than renamed: a subsequent `lock_session` call will
        // create a fresh lock under the new subdir, and any orphaned
        // lock at the old path would otherwise cause that acquisition
        // to wait the full `SESSION_LOCK_TIMEOUT` before declaring it
        // stale.
        if legacy_lock.exists() {
            let _ = fs::remove_file(&legacy_lock);
        }
        Ok(true)
    }

    /// Static variant of the instance-level `is_stale_lock` used by the
    /// adoption helper, since at that point we want to inspect a lock at
    /// the legacy path (outside `sessions_dir`).
    fn is_stale_lock_at(&self, lock_path: &Path) -> bool {
        let Ok(metadata) = fs::metadata(lock_path) else {
            return false;
        };
        let Ok(modified_at) = metadata.modified() else {
            return false;
        };
        let Ok(elapsed) = modified_at.elapsed() else {
            return false;
        };
        elapsed >= STALE_SESSION_LOCK_AGE
    }

    /// Create the sessions directory if it doesn't exist.
    fn ensure_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.sessions_dir)
    }

    /// Return the on-disk root directory for a session.
    pub fn session_root(&self, id: &str) -> PathBuf {
        self.sessions_dir.join(id)
    }

    /// Return the file-history store for a session.
    pub fn file_history(&self, id: &str) -> FileHistoryStore {
        FileHistoryStore::new(self.session_root(id))
    }

    /// Save a session to disk.
    pub fn save(&self, session: &SessionState) -> io::Result<()> {
        if !is_safe_session_path_id(&session.id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "refusing to save session with unsafe id '{}': session ids must be a single relative path component",
                    session.id
                ),
            ));
        }
        self.ensure_dir()?;
        SessionJsonlStore::new(self.session_root(&session.id))
            .append(&SessionEvent::snapshot(session.clone()))?;
        if let Err(error) = self.record_thread_session_index(session) {
            // Drop the one-time backfill marker so the next lookup rebuilds
            // instead of permanently omitting this session after a partial save.
            // Propagate cleanup failure: a stale marker with an unindexed
            // snapshot would hide the session from thread lookups forever.
            if let Err(cleanup_error) = fs::remove_file(self.thread_index_backfill_marker_path())
                && cleanup_error.kind() != io::ErrorKind::NotFound
            {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "failed to record thread session index for '{}': {error}; additionally failed to invalidate the thread-index backfill marker so lookups can rebuild ({cleanup_error})",
                        session.id
                    ),
                ));
            }
            return Err(error);
        }
        Ok(())
    }

    /// Session ids known to belong to `thread_id` (exact id and/or index).
    ///
    /// The index is maintained on [`Self::save`]. When the one-time migration
    /// marker is absent, [`Self::rebuild_thread_session_index`] runs first so
    /// pre-index sessions remain visible without a per-request historical scan
    /// afterward.
    pub fn session_ids_for_thread(&self, thread_id: &str) -> io::Result<Vec<String>> {
        self.rebuild_thread_session_index()?;
        let mut ids = BTreeSet::new();
        if !is_safe_thread_index_id(thread_id) {
            return Ok(Vec::new());
        }
        ids.insert(thread_id.to_string());
        let index_path = self.thread_index_path(thread_id);
        if index_path.exists() {
            for line in fs::read_to_string(&index_path)?.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    ids.insert(line.to_string());
                }
            }
        }
        Ok(ids
            .into_iter()
            .filter(|id| self.session_root(id).exists() || self.session_path(id).exists())
            .collect())
    }

    /// Rebuild `.thread_index` from every readable session (one-time).
    ///
    /// Called from Code startup and from the first graph overlay lookup when
    /// the migration marker is absent. Subsequent calls are no-ops. Fails
    /// closed if any session cannot be loaded or indexed so a partial
    /// migration cannot hide reconciliation overlays.
    pub fn rebuild_thread_session_index(&self) -> io::Result<()> {
        let marker = self.thread_index_backfill_marker_path();
        if marker.exists() {
            return Ok(());
        }
        if !self.sessions_dir.exists() {
            fs::create_dir_all(self.thread_index_dir())?;
            fs::write(&marker, b"1\n")?;
            return Ok(());
        }

        fs::create_dir_all(self.thread_index_dir())?;
        let mut failures = Vec::new();
        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if id.starts_with('.') {
                continue;
            }
            let session_id = if path.is_dir() {
                id.to_string()
            } else if path.extension().is_some_and(|ext| ext == "json") {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or(id)
                    .to_string()
            } else {
                continue;
            };
            match self.load(&session_id) {
                Ok(session) => {
                    if let Err(error) = self.record_thread_session_index(&session) {
                        failures.push(format!(
                            "{session_id}: failed to record thread index ({error})"
                        ));
                    }
                }
                Err(error) => {
                    failures.push(format!("{session_id}: failed to load session ({error})"));
                }
            }
        }
        if !failures.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "thread session index rebuild is incomplete; refusing to mark it complete while {} session(s) cannot be indexed: {}",
                    failures.len(),
                    failures.join("; ")
                ),
            ));
        }
        fs::write(&marker, b"1\n")?;
        Ok(())
    }

    fn thread_index_backfill_marker_path(&self) -> PathBuf {
        self.thread_index_dir().join(".backfilled")
    }

    fn thread_index_dir(&self) -> PathBuf {
        self.sessions_dir.join(".thread_index")
    }

    fn thread_index_path(&self, thread_id: &str) -> PathBuf {
        // INVARIANT: callers only pass `is_safe_thread_index_id` values.
        self.thread_index_dir().join(thread_id)
    }

    fn record_thread_session_index(&self, session: &SessionState) -> io::Result<()> {
        let mut thread_ids = BTreeSet::new();
        for key in THREAD_ID_METADATA_KEYS {
            if let Some(value) = session
                .metadata
                .get(*key)
                .and_then(serde_json::Value::as_str)
                .filter(|value| is_safe_thread_index_id(value))
            {
                thread_ids.insert(value.to_string());
            }
        }
        if thread_ids.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(self.thread_index_dir())?;
        for thread_id in thread_ids {
            let path = self.thread_index_path(&thread_id);
            let existing = if path.exists() {
                fs::read_to_string(&path)?
            } else {
                String::new()
            };
            if existing.lines().any(|line| line.trim() == session.id) {
                continue;
            }
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            writeln!(file, "{}", session.id)?;
        }
        Ok(())
    }

    /// Load a session by ID.
    pub fn load(&self, id: &str) -> io::Result<SessionState> {
        self.migrate_legacy_session_if_needed(id)?;
        let jsonl = SessionJsonlStore::new(self.session_root(id));
        if let Some(session) = jsonl.load_state()? {
            return Ok(session);
        }

        let path = self.session_path(id);
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let events_path = jsonl.events_path();
                if events_path.exists() {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "session event log '{}' contains no replayable session state",
                            events_path.display()
                        ),
                    ))
                } else {
                    Err(err)
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Load the most recently updated session.
    pub fn load_latest(&self) -> io::Result<Option<SessionState>> {
        let sessions = self.list()?;
        if sessions.is_empty() {
            return Ok(None);
        }

        // Find the most recently modified session file
        let mut latest: Option<(SessionState, SystemTime)> = None;

        for info in sessions {
            match self.load(&info.id) {
                Ok(session) => {
                    let modified = self.session_modified_time(&info.id);

                    if latest
                        .as_ref()
                        .is_none_or(|(_, best_time)| modified > *best_time)
                    {
                        latest = Some((session, modified));
                    }
                }
                Err(e) => {
                    tracing::warn!(session_id = %info.id, error = %e, "skipping corrupt session file");
                }
            }
        }

        Ok(latest.map(|(session, _)| session))
    }

    /// Load the most recently updated session for a specific working directory.
    pub fn load_latest_for_working_dir(
        &self,
        working_dir: &str,
    ) -> io::Result<Option<SessionState>> {
        let sessions = self.list()?;
        if sessions.is_empty() {
            return Ok(None);
        }

        let mut latest: Option<(SessionState, SystemTime)> = None;

        for info in sessions {
            match self.load(&info.id) {
                Ok(session) if session.working_dir == working_dir => {
                    let modified = self.session_modified_time(&info.id);

                    if latest
                        .as_ref()
                        .is_none_or(|(_, best_time)| modified > *best_time)
                    {
                        latest = Some((session, modified));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(session_id = %info.id, error = %e, "skipping corrupt session file");
                }
            }
        }

        Ok(latest.map(|(session, _)| session))
    }

    /// Load the most recently updated session for a canonical Libra thread_id.
    pub fn load_for_thread_id(
        &self,
        thread_id: &str,
        working_dir: &str,
    ) -> io::Result<Option<SessionState>> {
        let sessions = self.list()?;
        if sessions.is_empty() {
            return Ok(None);
        }

        let mut latest: Option<(SessionState, SystemTime)> = None;
        for info in sessions {
            match self.load(&info.id) {
                Ok(session)
                    if session.working_dir == working_dir
                        && session_matches_thread_id(&session, thread_id) =>
                {
                    let modified = self.session_modified_time(&info.id);

                    if latest
                        .as_ref()
                        .is_none_or(|(_, best_time)| modified > *best_time)
                    {
                        latest = Some((session, modified));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(session_id = %info.id, error = %e, "skipping corrupt session file");
                }
            }
        }

        Ok(latest.map(|(session, _)| session))
    }

    /// Preview non-destructive legacy metadata backfill for existing sessions.
    pub fn preview_legacy_metadata_backfill(&self) -> io::Result<SessionMetadataBackfillReport> {
        self.legacy_metadata_backfill_report(false)
    }

    /// Apply non-destructive legacy metadata backfill for existing sessions.
    pub fn apply_legacy_metadata_backfill(&self) -> io::Result<SessionMetadataBackfillReport> {
        self.legacy_metadata_backfill_report(true)
    }

    /// List all saved sessions (basic info only).
    pub fn list(&self) -> io::Result<Vec<SessionInfo>> {
        if !self.sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut ids = BTreeSet::new();
        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(id) = path.file_name().and_then(|name| name.to_str()) {
                    if id.starts_with('.') {
                        continue;
                    }
                    ids.insert(id.to_string());
                }
            } else if path.extension().is_some_and(|ext| ext == "json")
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                ids.insert(stem.to_string());
            }
        }

        let mut sessions = Vec::new();
        for id in ids {
            match self.load(&id) {
                Ok(session) => {
                    sessions.push(SessionInfo {
                        id: session.id,
                        created_at: session.created_at.to_string(),
                        summary: session.summary,
                        message_count: session.messages.len(),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %id,
                        error = %e,
                        "skipping malformed session"
                    );
                }
            }
        }

        Ok(sessions)
    }

    /// Delete a session by ID.
    pub fn delete(&self, id: &str) -> io::Result<()> {
        let mut removed = false;
        let legacy_path = self.session_path(id);
        match fs::remove_file(&legacy_path) {
            Ok(()) => removed = true,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(io::Error::new(
                    err.kind(),
                    format!(
                        "failed to remove legacy session file '{}': {err}",
                        legacy_path.display()
                    ),
                ));
            }
        }

        let root = self.session_root(id);
        match fs::remove_dir_all(&root) {
            Ok(()) => removed = true,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(io::Error::new(
                    err.kind(),
                    format!(
                        "failed to remove session directory '{}': {err}",
                        root.display()
                    ),
                ));
            }
        }

        if removed {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("session '{id}' was not found"),
            ))
        }
    }

    /// Acquire an exclusive lock for one session ID.
    ///
    /// The lock is implemented via a lock file in the sessions directory and
    /// is automatically released when the returned guard is dropped.
    pub fn lock_session(&self, id: &str) -> io::Result<SessionFileLock> {
        self.ensure_dir()?;
        let lock_path = self.session_lock_path(id);
        let start = Instant::now();

        loop {
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    let lock_content = format!(
                        "pid={}\ncreated_at_ns={}\n",
                        std::process::id(),
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                    );
                    file.write_all(lock_content.as_bytes())?;
                    return Ok(SessionFileLock { lock_path });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    if self.is_stale_lock(&lock_path) {
                        match fs::remove_file(&lock_path) {
                            Ok(()) => continue,
                            Err(remove_err) if remove_err.kind() == io::ErrorKind::NotFound => {
                                continue;
                            }
                            Err(remove_err) => {
                                return Err(io::Error::new(
                                    remove_err.kind(),
                                    format!(
                                        "failed to clear stale session lock '{}': {remove_err}",
                                        lock_path.display()
                                    ),
                                ));
                            }
                        }
                    }

                    if start.elapsed() >= SESSION_LOCK_TIMEOUT {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!(
                                "timed out waiting for session lock '{}'",
                                lock_path.display()
                            ),
                        ));
                    }
                    thread::sleep(SESSION_LOCK_POLL_INTERVAL);
                }
                Err(err) => {
                    return Err(io::Error::new(
                        err.kind(),
                        format!(
                            "failed to open session lock '{}': {err}",
                            lock_path.display()
                        ),
                    ));
                }
            }
        }
    }

    /// Move a malformed session file out of the way so ingestion can continue.
    pub fn archive_corrupt_session(&self, id: &str) -> io::Result<Option<PathBuf>> {
        let legacy_source = self.session_path(id);
        let session_root = self.session_root(id);
        let (source, archived) = if legacy_source.exists() {
            (
                legacy_source,
                self.sessions_dir.join(format!(
                    "{id}.corrupt.{}.json",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                )),
            )
        } else {
            (
                session_root,
                self.sessions_dir.join(format!(
                    "{id}.corrupt.{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                )),
            )
        };

        match fs::rename(&source, &archived) {
            Ok(()) => Ok(Some(archived)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(io::Error::new(
                err.kind(),
                format!(
                    "failed to archive corrupt session file '{}' -> '{}': {err}",
                    source.display(),
                    archived.display()
                ),
            )),
        }
    }

    fn legacy_metadata_backfill_report(
        &self,
        apply: bool,
    ) -> io::Result<SessionMetadataBackfillReport> {
        let sessions = self.list()?;
        let mut updates = Vec::new();

        for info in &sessions {
            let mut session = match self.load(&info.id) {
                Ok(session) => session,
                Err(error) => {
                    tracing::warn!(session_id = %info.id, error = %error, "skipping corrupt session file");
                    continue;
                }
            };
            if session.metadata.contains_key("legacy_session_id") {
                continue;
            }

            let canonical_thread_id = first_metadata_string(&session, THREAD_ID_METADATA_KEYS)
                .or_else(|| uuid_session_id(&session.id));
            let provider_thread_id =
                first_metadata_string(&session, &["provider_thread_id", "providerThreadId"]);
            let update = SessionMetadataBackfillUpdate {
                session_id: session.id.clone(),
                working_dir: session.working_dir.clone(),
                legacy_session_id: session.id.clone(),
                canonical_thread_id,
                provider_thread_id,
            };

            if apply {
                session.metadata.insert(
                    "legacy_session_id".to_string(),
                    serde_json::json!(update.legacy_session_id),
                );
                if let Some(thread_id) = update.canonical_thread_id.as_ref() {
                    session
                        .metadata
                        .entry("thread_id".to_string())
                        .or_insert_with(|| serde_json::json!(thread_id));
                }
                if let Some(provider_thread_id) = update.provider_thread_id.as_ref() {
                    session
                        .metadata
                        .entry("provider_thread_id".to_string())
                        .or_insert_with(|| serde_json::json!(provider_thread_id));
                }
                self.save(&session)?;
            }

            updates.push(update);
        }

        Ok(SessionMetadataBackfillReport {
            scanned: sessions.len(),
            updates,
        })
    }

    fn session_path(&self, id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{id}.json"))
    }

    fn migrate_legacy_session_if_needed(&self, id: &str) -> io::Result<bool> {
        LegacySessionMigrator::new(self.sessions_dir.clone())
            .migrate_if_needed(id, &self.session_root(id))
    }

    fn session_modified_time(&self, id: &str) -> SystemTime {
        let events_path = SessionJsonlStore::new(self.session_root(id)).events_path();
        fs::metadata(&events_path)
            .and_then(|metadata| metadata.modified())
            .or_else(|_| {
                fs::metadata(self.session_path(id)).and_then(|metadata| metadata.modified())
            })
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }

    fn session_lock_path(&self, id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{id}.lock"))
    }

    fn is_stale_lock(&self, lock_path: &Path) -> bool {
        let Ok(metadata) = fs::metadata(lock_path) else {
            return false;
        };
        let Ok(modified_at) = metadata.modified() else {
            return false;
        };
        let Ok(elapsed) = modified_at.elapsed() else {
            return false;
        };
        elapsed >= STALE_SESSION_LOCK_AGE
    }
}

fn session_matches_thread_id(session: &SessionState, thread_id: &str) -> bool {
    if session.id == thread_id {
        return true;
    }

    THREAD_ID_METADATA_KEYS.iter().any(|key| {
        session
            .metadata
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == thread_id)
    })
}

fn first_metadata_string(session: &SessionState, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        session
            .metadata
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    })
}

fn uuid_session_id(session_id: &str) -> Option<String> {
    uuid::Uuid::parse_str(session_id)
        .ok()
        .map(|id| id.to_string())
}

fn is_safe_thread_index_id(thread_id: &str) -> bool {
    // Only UUID thread ids are indexed on disk so metadata cannot escape the
    // `.thread_index` directory via `..`, separators, or absolute paths.
    uuid::Uuid::parse_str(thread_id).is_ok()
        && !thread_id.contains('/')
        && !thread_id.contains('\\')
        && !thread_id.contains('\0')
}

fn is_safe_session_path_id(session_id: &str) -> bool {
    // Session ids are used as directory/file names under sessions/. Reject
    // path escape and absolute components.
    if session_id.is_empty()
        || session_id == "."
        || session_id == ".."
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains('\0')
    {
        return false;
    }
    let path = Path::new(session_id);
    !path.is_absolute() && path.components().count() == 1
}

/// Brief info about a saved session.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub created_at: String,
    pub summary: String,
    pub message_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_rejects_unsafe_session_ids() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::from_storage_path(tmp.path());
        for unsafe_id in [
            "../escape",
            "/tmp/abs",
            "a/b",
            "a\\b",
            "a\0b",
            ".",
            "..",
            "",
        ] {
            let mut session = SessionState::new("/repo");
            session.id = unsafe_id.to_string();
            let error = store
                .save(&session)
                .expect_err("unsafe id must be rejected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(
                !tmp.path().join("sessions").join(unsafe_id).exists(),
                "unsafe id must not create a session path: {unsafe_id}"
            );
        }
    }

    #[test]
    fn test_save_and_load() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let mut session = SessionState::new("/tmp/test");
        session.summary = "Test session".to_string();
        session.add_user_message("hello");

        store.save(&session).unwrap();

        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.summary, "Test session");
        assert_eq!(loaded.message_count(), 1);
    }

    #[test]
    fn test_list_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let s1 = SessionState::new("/tmp/a");
        let s2 = SessionState::new("/tmp/b");
        store.save(&s1).unwrap();
        store.save(&s2).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_list_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let list = store.list().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_delete_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let session = SessionState::new("/tmp/test");
        store.save(&session).unwrap();
        assert!(store.load(&session.id).is_ok());

        store.delete(&session.id).unwrap();
        assert!(store.load(&session.id).is_err());
    }

    #[test]
    fn test_from_storage_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::from_storage_path(tmp.path());

        let session = SessionState::new("/tmp/test");
        store.save(&session).unwrap();
        assert!(
            tmp.path()
                .join("sessions")
                .join(&session.id)
                .join("events.jsonl")
                .exists()
        );
    }

    /// CEX-EntireIO §11.2 Phase 3.4: a SessionStore created with a subdir
    /// must rehome a clean legacy session that landed in the
    /// pre-partition `sessions/<id>/` directory before the agent capture
    /// hook runtime started using the `agent/` subdir. We mock the
    /// pre-partition state by writing the session through the legacy
    /// store, then asserting the subdir-aware store moves the directory
    /// into place. The lock-file behaviour is asserted by sibling tests
    /// (`adopt_legacy_subdir_session_skips_when_legacy_lock_is_fresh` and
    /// `lock_session_after_adoption_completes_quickly`).
    #[test]
    fn adopt_legacy_subdir_session_moves_legacy_entry() {
        let tmp = tempfile::TempDir::new().unwrap();

        let legacy_store = SessionStore::from_storage_path(tmp.path());
        let session = SessionState::new("/tmp/agent-fixture");
        legacy_store.save(&session).unwrap();
        let id = session.id.clone();
        let legacy_dir = tmp.path().join("sessions").join(&id);
        assert!(legacy_dir.join("events.jsonl").exists());

        let store = SessionStore::from_storage_path_with_subdir(tmp.path(), "agent");
        let migrated = store.adopt_legacy_subdir_session_if_needed(&id).unwrap();
        assert!(migrated, "first call should migrate");

        let new_dir = tmp.path().join("sessions").join("agent").join(&id);
        assert!(new_dir.join("events.jsonl").exists(), "session moved");
        assert!(!legacy_dir.exists(), "legacy directory removed");

        // Idempotency: running the adoption a second time is a no-op.
        let again = store.adopt_legacy_subdir_session_if_needed(&id).unwrap();
        assert!(!again, "second call is a no-op");
    }

    /// Symmetric guard: when there is no legacy entry to migrate, the
    /// adoption call must be a no-op rather than producing spurious errors
    /// or empty directories. This protects fresh installs that never had a
    /// pre-partition layout.
    #[test]
    fn adopt_legacy_subdir_session_is_noop_without_legacy_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::from_storage_path_with_subdir(tmp.path(), "agent");
        let result = store
            .adopt_legacy_subdir_session_if_needed("nonexistent-id")
            .unwrap();
        assert!(!result);
    }

    /// Round-1 review concern (gating): a non-partitioned store
    /// (`from_storage_path` / `new`) must reject adoption even when a
    /// legacy-shaped entry happens to exist below it. This catches the
    /// pathological case where a storage root is itself named `sessions`
    /// and would otherwise pass a path-basename guard.
    #[test]
    fn adopt_legacy_subdir_session_rejects_non_partitioned_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::from_storage_path(tmp.path());
        // Plant a directory in the parent that *would* migrate if the
        // partitioned flag were absent.
        fs::create_dir_all(tmp.path().join("dummy-id")).unwrap();
        let result = store
            .adopt_legacy_subdir_session_if_needed("dummy-id")
            .unwrap();
        assert!(!result, "non-partitioned store must not adopt");
    }

    /// Round-1 review concern (lock-stream race): when the legacy lock
    /// file is fresh (not stale by `STALE_SESSION_LOCK_AGE`), the
    /// adoption MUST refuse to migrate, because a live writer might
    /// still be appending to the legacy directory. Skipping protects
    /// the event stream from being split across two parents.
    #[test]
    fn adopt_legacy_subdir_session_skips_when_legacy_lock_is_fresh() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Plant the legacy session directory and a fresh lock file.
        let legacy_dir = tmp.path().join("sessions").join("agent-fixture");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("events.jsonl"), b"").unwrap();
        let legacy_lock = tmp.path().join("sessions").join("agent-fixture.lock");
        fs::write(&legacy_lock, b"").unwrap();

        let store = SessionStore::from_storage_path_with_subdir(tmp.path(), "agent");
        let migrated = store
            .adopt_legacy_subdir_session_if_needed("agent-fixture")
            .unwrap();
        assert!(!migrated, "fresh legacy lock blocks migration");
        assert!(legacy_dir.exists(), "legacy entry untouched");
        assert!(
            legacy_lock.exists(),
            "legacy lock left in place for the writer"
        );
    }

    /// Round-1 review concern (lock-acquire flow): after a successful
    /// adoption with no legacy lock present, the call site immediately
    /// tries to acquire a lock under the new subdir. This must succeed
    /// quickly — the legacy lock file must not have been carried over
    /// to the new path, otherwise `lock_session` would wait the full
    /// timeout on a stale entry. We exercise the no-legacy-lock branch
    /// here (the dominant case for an upgrade after a clean shutdown);
    /// the fresh-lock branch is covered by
    /// `adopt_legacy_subdir_session_skips_when_legacy_lock_is_fresh`.
    #[test]
    fn lock_session_after_adoption_completes_quickly() {
        let tmp = tempfile::TempDir::new().unwrap();

        let legacy_dir = tmp.path().join("sessions").join("agent-fixture");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("events.jsonl"), b"").unwrap();

        let store = SessionStore::from_storage_path_with_subdir(tmp.path(), "agent");
        let migrated = store
            .adopt_legacy_subdir_session_if_needed("agent-fixture")
            .unwrap();
        assert!(migrated, "no legacy lock present means migration proceeds");

        // No lock should exist under the new path yet — this is the
        // critical guarantee that `lock_session` will make a fresh one
        // rather than waiting on a carried-over file.
        let new_lock = tmp
            .path()
            .join("sessions")
            .join("agent")
            .join("agent-fixture.lock");
        assert!(!new_lock.exists(), "no inherited lock at new location");

        let started = std::time::Instant::now();
        let lock = store.lock_session("agent-fixture").unwrap();
        // The acquisition should be effectively instant (< the timeout).
        assert!(
            started.elapsed() < SESSION_LOCK_TIMEOUT,
            "lock_session blocked on a stale carry-over"
        );
        assert!(new_lock.exists(), "fresh lock created under the new subdir");
        drop(lock);
    }

    #[test]
    fn test_load_latest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let s1 = SessionState::new("/tmp/first");
        store.save(&s1).unwrap();

        // Brief sleep to ensure different modified times
        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut s2 = SessionState::new("/tmp/second");
        s2.summary = "latest".to_string();
        store.save(&s2).unwrap();

        let latest = store.load_latest().unwrap().unwrap();
        assert_eq!(latest.summary, "latest");
    }

    #[test]
    fn test_load_latest_for_working_dir_filters_shared_storage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::from_storage_path(tmp.path());

        let mut worktree_a_old = SessionState::new("/repo/.worktrees/a");
        worktree_a_old.summary = "a-old".to_string();
        store.save(&worktree_a_old).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut worktree_b = SessionState::new("/repo/.worktrees/b");
        worktree_b.summary = "b-latest".to_string();
        store.save(&worktree_b).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut worktree_a_new = SessionState::new("/repo/.worktrees/a");
        worktree_a_new.summary = "a-latest".to_string();
        store.save(&worktree_a_new).unwrap();

        let latest_for_a = store
            .load_latest_for_working_dir("/repo/.worktrees/a")
            .unwrap()
            .unwrap();
        assert_eq!(latest_for_a.summary, "a-latest");

        let latest_for_b = store
            .load_latest_for_working_dir("/repo/.worktrees/b")
            .unwrap()
            .unwrap();
        assert_eq!(latest_for_b.summary, "b-latest");
    }

    #[test]
    fn test_load_for_thread_id_uses_canonical_metadata_and_working_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::from_storage_path(tmp.path());
        let thread_id = "11111111-1111-4111-8111-111111111111";

        let mut wrong_worktree = SessionState::new("/repo/other");
        wrong_worktree.summary = "wrong".to_string();
        wrong_worktree
            .metadata
            .insert("thread_id".to_string(), serde_json::json!(thread_id));
        store.save(&wrong_worktree).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut canonical = SessionState::new("/repo/main");
        canonical.summary = "canonical".to_string();
        canonical
            .metadata
            .insert("thread_id".to_string(), serde_json::json!(thread_id));
        store.save(&canonical).unwrap();

        let loaded = store
            .load_for_thread_id(thread_id, "/repo/main")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.summary, "canonical");
    }

    #[test]
    fn test_legacy_metadata_backfill_preview_and_apply_are_non_destructive() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::from_storage_path(tmp.path());
        let thread_id = "11111111-1111-4111-8111-111111111111";

        let mut session = SessionState::new("/repo/main");
        let session_id = session.id.clone();
        session
            .metadata
            .insert("thread_id".to_string(), serde_json::json!(thread_id));
        store.save(&session).unwrap();

        let preview = store.preview_legacy_metadata_backfill().unwrap();
        assert_eq!(preview.scanned, 1);
        assert_eq!(preview.updates.len(), 1);
        assert_eq!(preview.updates[0].legacy_session_id, session_id);
        assert_eq!(
            preview.updates[0].canonical_thread_id.as_deref(),
            Some(thread_id)
        );
        assert!(
            !store
                .load(&session_id)
                .unwrap()
                .metadata
                .contains_key("legacy_session_id")
        );

        let applied = store.apply_legacy_metadata_backfill().unwrap();
        assert_eq!(applied.updates.len(), 1);
        let loaded = store.load(&session_id).unwrap();
        assert_eq!(
            loaded
                .metadata
                .get("legacy_session_id")
                .and_then(serde_json::Value::as_str),
            Some(session_id.as_str())
        );

        let second = store.preview_legacy_metadata_backfill().unwrap();
        assert!(second.updates.is_empty());
    }

    #[test]
    fn test_save_load_roundtrip_with_messages_and_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let mut session = SessionState::new("/project/dir");
        session.summary = "Implemented auth feature".to_string();
        session.context_mode = Some("dev".to_string());
        session.add_user_message("add JWT authentication");
        session.add_assistant_message("I'll implement JWT auth using jsonwebtoken crate");
        session.add_user_message("looks good, proceed");
        session.add_assistant_message("Done. Created auth module with login/verify endpoints.");
        session
            .metadata
            .insert("provider".to_string(), serde_json::json!("anthropic"));
        session
            .metadata
            .insert("model".to_string(), serde_json::json!("claude-3-5-sonnet"));

        store.save(&session).unwrap();

        let loaded = store.load(&session.id).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.working_dir, "/project/dir");
        assert_eq!(loaded.summary, "Implemented auth feature");
        assert_eq!(loaded.context_mode.as_deref(), Some("dev"));
        assert_eq!(loaded.message_count(), 4);
        assert_eq!(loaded.messages[0].role, "user");
        assert_eq!(loaded.messages[0].content, "add JWT authentication");
        assert_eq!(loaded.messages[3].role, "assistant");
        assert_eq!(loaded.metadata.get("provider").unwrap(), "anthropic");
        assert_eq!(loaded.metadata.get("model").unwrap(), "claude-3-5-sonnet");
    }

    #[test]
    fn test_load_latest_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        assert!(store.load_latest().unwrap().is_none());
    }
}
