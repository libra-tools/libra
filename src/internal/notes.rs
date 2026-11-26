//! Notes operations: add, list, show, and remove notes attached to commits.
//!
//! Notes are stored as blobs in the object store with mappings persisted in the
//! SQLite `notes` table. Each row maps a (`notes_ref`, `object`) pair to a blob
//! hash. The default notes ref is `refs/notes/commits`.

use git_internal::{errors::GitError, hash::ObjectHash, internal::object::ObjectTrait};
use sea_orm::{ConnectionTrait, DbErr, Statement};

use crate::{internal::db::get_db_conn_instance, utils::util};

/// Default notes ref namespace.
pub const DEFAULT_NOTES_REF: &str = "refs/notes/commits";

/// Merge bookkeeping lives beside the worktree gitdir, like Git's
/// `NOTES_MERGE_*` files.
pub const NOTES_MERGE_WORKTREE: &str = "NOTES_MERGE_WORKTREE";
pub const NOTES_MERGE_STATE: &str = "NOTES_MERGE_STATE";
pub const NOTES_MERGE_LOCK: &str = "NOTES_MERGE_LOCK";
pub const NOTES_MERGE_COMMITTING: &str = "NOTES_MERGE_COMMITTING";

/// One conflict entry: the target-side value before the merge and the source
/// value the merge wanted to apply (both blob ids).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotesMergeConflictEntry {
    pub object: String,
    pub before: String,
    pub source: String,
}

/// One entry that is copied because the target ref has no note for the object.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotesMergeCopyEntry {
    pub object: String,
    pub source: String,
}

/// Versioned `NOTES_MERGE_STATE` payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotesMergeState {
    pub version: u32,
    pub target_ref: String,
    pub source_ref: String,
    pub to_copy: Vec<NotesMergeCopyEntry>,
    pub conflicts: Vec<NotesMergeConflictEntry>,
}

impl NotesMergeState {
    pub const VERSION: u32 = 1;

    pub fn to_bytes(&self) -> Result<Vec<u8>, NotesError> {
        serde_json::to_vec_pretty(self).map_err(|error| NotesError::MergeStateIo {
            path: NOTES_MERGE_STATE.to_string(),
            detail: error.to_string(),
        })
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NotesError> {
        serde_json::from_slice(bytes).map_err(|error| NotesError::MergeStateIo {
            path: NOTES_MERGE_STATE.to_string(),
            detail: error.to_string(),
        })
    }
}

/// Exclusive `NOTES_MERGE_LOCK`; removed on drop, including error paths.
pub struct NotesMergeLock {
    path: std::path::PathBuf,
}

impl NotesMergeLock {
    pub fn acquire() -> Result<Self, NotesError> {
        let path = merge_state_dir()?.join(NOTES_MERGE_LOCK);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => Ok(Self { path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(NotesError::MergeLocked(path.display().to_string()))
            }
            Err(error) => Err(NotesError::MergeStateIo {
                path: path.display().to_string(),
                detail: error.to_string(),
            }),
        }
    }
}

impl Drop for NotesMergeLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn merge_state_dir() -> Result<std::path::PathBuf, NotesError> {
    util::request_worktree_gitdir().map_err(|error| NotesError::MergeStateIo {
        path: "<worktree gitdir>".to_string(),
        detail: error.to_string(),
    })
}

/// Any leftover merge file blocks a new merge (Git's "in progress" contract).
pub fn ensure_no_merge_in_progress() -> Result<(), NotesError> {
    let dir = merge_state_dir()?;
    for name in [
        NOTES_MERGE_STATE,
        NOTES_MERGE_WORKTREE,
        NOTES_MERGE_COMMITTING,
    ] {
        if dir.join(name).exists() {
            return Err(NotesError::MergeInProgress);
        }
    }
    Ok(())
}

/// Materialize a manual conflict: the conflict worktree (exclusive), one
/// `<oid>` file per conflict, then the atomic versioned state file.
fn write_manual_conflict_state(
    target_ref: &str,
    source_ref: &str,
    to_copy: &[(String, String)],
    conflicts: &[(String, String, String)],
) -> Result<(), NotesError> {
    let dir = merge_state_dir()?;
    let worktree = dir.join(NOTES_MERGE_WORKTREE);
    std::fs::create_dir(&worktree).map_err(|error| NotesError::MergeStateIo {
        path: worktree.display().to_string(),
        detail: error.to_string(),
    })?;
    for (object, before, source) in conflicts {
        let body = conflict_marker_body(target_ref, before, source_ref, source)?;
        let path = worktree.join(object);
        std::fs::write(&path, body).map_err(|error| NotesError::MergeStateIo {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
    }
    let state = NotesMergeState {
        version: NotesMergeState::VERSION,
        target_ref: target_ref.to_string(),
        source_ref: source_ref.to_string(),
        to_copy: to_copy
            .iter()
            .map(|(object, source)| NotesMergeCopyEntry {
                object: object.clone(),
                source: source.clone(),
            })
            .collect(),
        conflicts: conflicts
            .iter()
            .map(|(object, before, source)| NotesMergeConflictEntry {
                object: object.clone(),
                before: before.clone(),
                source: source.clone(),
            })
            .collect(),
    };
    let path = dir.join(NOTES_MERGE_STATE);
    crate::utils::atomic_write::write_atomic(&path, &state.to_bytes()?, true).map_err(|error| {
        NotesError::MergeStateIo {
            path: path.display().to_string(),
            detail: error.to_string(),
        }
    })
}

fn conflict_marker_body(
    target_ref: &str,
    before_blob: &str,
    source_ref: &str,
    source_blob: &str,
) -> Result<String, NotesError> {
    let ensure_newline = |text: String| {
        if text.ends_with('\n') {
            text
        } else {
            format!("{text}\n")
        }
    };
    let before = ensure_newline(load_blob_text(before_blob)?);
    let source = ensure_newline(load_blob_text(source_blob)?);
    Ok(format!(
        "<<<<<<< {target_ref}\n{before}=======\n{source}>>>>>>> {source_ref}\n"
    ))
}

/// `notes merge --abort` / `--quit`: discard the merge state without touching
/// the `notes` table. Any subset of the three paths is removed.
pub async fn abort_merge() -> Result<(), NotesError> {
    let _lock = NotesMergeLock::acquire()?;
    let dir = merge_state_dir()?;
    let paths = [
        dir.join(NOTES_MERGE_STATE),
        dir.join(NOTES_MERGE_WORKTREE),
        dir.join(NOTES_MERGE_COMMITTING),
    ];
    if !paths.iter().any(|path| path.exists()) {
        return Err(NotesError::MergeInProgress);
    }
    for path in &paths {
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(path);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

/// Outcome of `notes merge --commit`.
#[derive(Debug, Clone)]
pub struct CommitMergeResult {
    pub notes_ref: String,
    pub source_ref: String,
    pub applied: usize,
    pub skipped: usize,
}

/// Apply a materialized manual merge (`notes merge --commit`).
///
/// All row checks happen before any write inside one transaction, so a
/// concurrent change leaves the table untouched. Every merge form holds the
/// worktree-scoped `NOTES_MERGE_LOCK`, and the commit-in-progress marker turns a
/// crash window into a fail-closed state (retry semantics: `PM-38`).
pub async fn commit_merge() -> Result<CommitMergeResult, NotesError> {
    let _lock = NotesMergeLock::acquire()?;
    let dir = merge_state_dir()?;
    let state_path = dir.join(NOTES_MERGE_STATE);
    let bytes = match std::fs::read(&state_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(NotesError::MergeInProgress);
        }
        Err(error) => {
            return Err(NotesError::MergeStateIo {
                path: state_path.display().to_string(),
                detail: error.to_string(),
            });
        }
    };
    let state = NotesMergeState::from_bytes(&bytes)
        .map_err(|error| NotesError::MergeStateCorrupt(error.to_string()))?;
    let worktree = dir.join(NOTES_MERGE_WORKTREE);

    // Resolved value per conflict: the worktree file, or the target-side value
    // when the file was deleted (Git keeps `ours`).
    let mut targets: Vec<(String, String, String)> = Vec::new(); // (object, before, target)
    for conflict in &state.conflicts {
        let path = worktree.join(&conflict.object);
        let target = match std::fs::read(&path) {
            Ok(bytes) => {
                let text = String::from_utf8(bytes).map_err(|error| NotesError::MergeStateIo {
                    path: path.display().to_string(),
                    detail: error.to_string(),
                })?;
                store_blob_text(&text)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => conflict.before.clone(),
            Err(error) => {
                return Err(NotesError::MergeStateIo {
                    path: path.display().to_string(),
                    detail: error.to_string(),
                });
            }
        };
        targets.push((conflict.object.clone(), conflict.before.clone(), target));
    }

    let marker = dir.join(NOTES_MERGE_COMMITTING);
    match crate::storage::runtime::detect_current_repo_backend()
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
    {
        crate::storage::StorageBackendKind::Redb => {
            let store = crate::storage::runtime::open_current_git_store_read_write()
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;

            if marker.exists() {
                for (object, _before, target) in &targets {
                    let current = store
                        .get_note(&state.target_ref, object)
                        .await
                        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
                        .map(|r| r.blob);
                    if current.as_deref() != Some(target.as_str()) {
                        return Err(NotesError::MergeRaced {
                            object: object.clone(),
                        });
                    }
                }
                for copy in &state.to_copy {
                    let current = store
                        .get_note(&state.target_ref, &copy.object)
                        .await
                        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
                        .map(|r| r.blob);
                    if current.as_deref() != Some(copy.source.as_str()) {
                        return Err(NotesError::MergeRaced {
                            object: copy.object.clone(),
                        });
                    }
                }
                let skipped = targets.len() + state.to_copy.len();
                let _ = std::fs::remove_file(&state_path);
                let _ = std::fs::remove_dir_all(&worktree);
                let _ = std::fs::remove_file(&marker);
                return Ok(CommitMergeResult {
                    notes_ref: state.target_ref,
                    source_ref: state.source_ref,
                    applied: 0,
                    skipped,
                });
            }

            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
            {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(NotesError::MergeInProgress);
                }
                Err(error) => {
                    return Err(NotesError::MergeStateIo {
                        path: marker.display().to_string(),
                        detail: error.to_string(),
                    });
                }
            }

            let mut ops: Vec<crate::storage::NoteCasOp> = Vec::new();
            let mut skipped = 0usize;
            let mut verify_err: Option<NotesError> = None;
            for (object, before, target) in &targets {
                let current = match store.get_note(&state.target_ref, object).await {
                    Ok(rec) => rec.map(|r| r.blob),
                    Err(err) => {
                        verify_err = Some(NotesError::from(DbErr::Custom(err.to_string())));
                        break;
                    }
                };
                if current.as_deref() == Some(target.as_str()) {
                    skipped += 1;
                } else if current.as_deref() == Some(before.as_str()) {
                    if before == target {
                        skipped += 1;
                    } else {
                        ops.push(crate::storage::NoteCasOp::UpdateIfMatches {
                            object: object.clone(),
                            expected_blob: before.clone(),
                            new_blob: target.clone(),
                        });
                    }
                } else {
                    verify_err = Some(NotesError::MergeRaced {
                        object: object.clone(),
                    });
                    break;
                }
            }
            if verify_err.is_none() {
                for copy in &state.to_copy {
                    let current = match store.get_note(&state.target_ref, &copy.object).await {
                        Ok(rec) => rec.map(|r| r.blob),
                        Err(err) => {
                            verify_err = Some(NotesError::from(DbErr::Custom(err.to_string())));
                            break;
                        }
                    };
                    match current {
                        None => {
                            ops.push(crate::storage::NoteCasOp::InsertIfAbsent {
                                object: copy.object.clone(),
                                blob: copy.source.clone(),
                            });
                        }
                        Some(cur) if cur == copy.source => {
                            skipped += 1;
                        }
                        Some(_) => {
                            verify_err = Some(NotesError::MergeRaced {
                                object: copy.object.clone(),
                            });
                            break;
                        }
                    }
                }
            }
            if let Some(error) = verify_err {
                let _ = std::fs::remove_file(&marker);
                return Err(error);
            }

            let applied = if ops.is_empty() {
                0
            } else {
                match store
                    .apply_notes_cas_batch(&state.target_ref, &ops)
                    .await
                    .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
                {
                    crate::storage::NoteBatchOutcome::Applied {
                        inserted, updated, ..
                    } => inserted + updated,
                    crate::storage::NoteBatchOutcome::CasConflict { object, .. } => {
                        let _ = std::fs::remove_file(&marker);
                        return Err(NotesError::MergeRaced { object });
                    }
                }
            };

            // Cleanup order from ADR-PM-02: state, worktree, marker.
            let _ = std::fs::remove_file(&state_path);
            let _ = std::fs::remove_dir_all(&worktree);
            let _ = std::fs::remove_file(&marker);

            Ok(CommitMergeResult {
                notes_ref: state.target_ref,
                source_ref: state.source_ref,
                applied,
                skipped,
            })
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db = get_db_conn_instance().await;

            // A pre-existing marker means the previous commit crashed before or after
            // its transaction. We cannot distinguish those cases, so retry CONFIRMS
            // only (never replays): every row must already hold its target value, in
            // which case the three state paths are cleaned up; otherwise fail closed
            // and keep everything so the user can `--abort` and merge again.
            if marker.exists() {
                for (object, before, target) in &targets {
                    let current = note_blob_with_conn(&db, &state.target_ref, object).await?;
                    let _ = before;
                    if current.as_deref() != Some(target.as_str()) {
                        return Err(NotesError::MergeRaced {
                            object: object.clone(),
                        });
                    }
                }
                for copy in &state.to_copy {
                    let current = note_blob_with_conn(&db, &state.target_ref, &copy.object).await?;
                    if current.as_deref() != Some(copy.source.as_str()) {
                        return Err(NotesError::MergeRaced {
                            object: copy.object.clone(),
                        });
                    }
                }
                let skipped = targets.len() + state.to_copy.len();
                let _ = std::fs::remove_file(&state_path);
                let _ = std::fs::remove_dir_all(&worktree);
                let _ = std::fs::remove_file(&marker);
                return Ok(CommitMergeResult {
                    notes_ref: state.target_ref,
                    source_ref: state.source_ref,
                    applied: 0,
                    skipped,
                });
            }

            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
            {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(NotesError::MergeInProgress);
                }
                Err(error) => {
                    return Err(NotesError::MergeStateIo {
                        path: marker.display().to_string(),
                        detail: error.to_string(),
                    });
                }
            }

            let txn = crate::internal::db::begin_write_transaction(&db).await?;
            let verify = async {
                for (object, before, target) in &targets {
                    let current = note_blob_with_conn(&txn, &state.target_ref, object).await?;
                    if current.as_deref() != Some(before.as_str())
                        && current.as_deref() != Some(target.as_str())
                    {
                        return Err(NotesError::MergeRaced {
                            object: object.clone(),
                        });
                    }
                }
                for copy in &state.to_copy {
                    let current =
                        note_blob_with_conn(&txn, &state.target_ref, &copy.object).await?;
                    if let Some(current) = current
                        && current != copy.source
                    {
                        return Err(NotesError::MergeRaced {
                            object: copy.object.clone(),
                        });
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = verify {
                txn.rollback().await?;
                let _ = std::fs::remove_file(&marker);
                return Err(error);
            }

            let mut applied = 0usize;
            let mut skipped = 0usize;
            for (object, before, target) in &targets {
                if before == target {
                    skipped += 1;
                    continue;
                }
                txn.execute_raw(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Sqlite,
                    "UPDATE notes SET blob = ? WHERE notes_ref = ? AND object = ? AND blob = ?",
                    [
                        target.clone().into(),
                        state.target_ref.clone().into(),
                        object.clone().into(),
                        before.clone().into(),
                    ],
                ))
                .await?;
                applied += 1;
            }
            for copy in &state.to_copy {
                let current = note_blob_with_conn(&txn, &state.target_ref, &copy.object).await?;
                if current.is_some() {
                    skipped += 1;
                    continue;
                }
                txn.execute_raw(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Sqlite,
                    "INSERT INTO notes (notes_ref, object, blob) VALUES (?, ?, ?)",
                    [
                        state.target_ref.clone().into(),
                        copy.object.clone().into(),
                        copy.source.clone().into(),
                    ],
                ))
                .await?;
                applied += 1;
            }
            txn.commit().await?;

            // Cleanup order from ADR-PM-02: state, worktree, marker.
            let _ = std::fs::remove_file(&state_path);
            let _ = std::fs::remove_dir_all(&worktree);
            let _ = std::fs::remove_file(&marker);

            Ok(CommitMergeResult {
                notes_ref: state.target_ref,
                source_ref: state.source_ref,
                applied,
                skipped,
            })
        }
    }
}

async fn note_blob_with_conn<C>(
    conn: &C,
    notes_ref: &str,
    object: &str,
) -> Result<Option<String>, NotesError>
where
    C: sea_orm::ConnectionTrait,
{
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT blob FROM notes WHERE notes_ref = ? AND object = ? LIMIT 1",
            [notes_ref.into(), object.into()],
        ))
        .await?;
    Ok(row.and_then(|row| row.try_get::<String>("", "blob").ok()))
}

/// Normalize a user-supplied notes ref. Short names (e.g. `review`) are
/// expanded to `refs/notes/<name>` for Git compatibility; refs that already
/// start with `refs/notes/` pass through unchanged. Any other fully-qualified
/// ref (e.g. `refs/heads/main`) is rejected.
pub fn normalize_notes_ref(raw: &str) -> Result<String, NotesError> {
    if raw.starts_with("refs/notes/") {
        return Ok(raw.to_string());
    }
    // Short name: no path separator — expand to refs/notes/<name>.
    if !raw.contains('/') {
        return Ok(format!("refs/notes/{raw}"));
    }
    Err(NotesError::InvalidNotesRef(raw.to_string()))
}

/// Validates that a notes ref starts with `refs/notes/`.
pub fn validate_notes_ref(notes_ref: &str) -> Result<(), NotesError> {
    if notes_ref.starts_with("refs/notes/") {
        Ok(())
    } else {
        Err(NotesError::InvalidNotesRef(notes_ref.to_string()))
    }
}

/// The result of adding a note.
#[derive(Debug, Clone)]
pub struct AddNoteResult {
    pub notes_ref: String,
    pub object: String,
    pub note_hash: String,
}

/// A note entry returned from listing.
///
/// When listing by a specific object that has no note, `note_hash` is `None`.
#[derive(Debug, Clone)]
pub struct NoteEntry {
    pub note_hash: Option<String>,
    pub annotated_object: String,
}

/// Errors that can occur during note operations.
#[derive(Debug, thiserror::Error)]
pub enum NotesError {
    #[error("notes ref must start with 'refs/notes/': {0}")]
    InvalidNotesRef(String),

    #[error("note already exists for object '{object}' in {notes_ref}")]
    AlreadyExists { notes_ref: String, object: String },

    #[error("no note found for object '{object}' in {notes_ref}")]
    NotFound { notes_ref: String, object: String },

    #[error("invalid object reference '{0}': {1}")]
    InvalidObject(String, String),

    #[error("HEAD does not point to a commit")]
    HeadUnborn,

    #[error("failed to query notes: {0}")]
    QueryFailed(#[from] DbErr),

    #[error("failed to resolve object: {0}")]
    ResolveFailed(String),

    #[error("failed to store blob: {0}")]
    StoreBlobFailed(#[source] std::io::Error),

    #[error("notes merge conflict for {} object(s) in {notes_ref}: {}", objects.len(), objects.join(", "))]
    MergeConflict {
        notes_ref: String,
        objects: Vec<String>,
    },

    #[error(
        "unsupported notes merge strategy '{0}' (expected manual, ours, theirs, union, or cat_sort_uniq)"
    )]
    UnsupportedStrategy(String),

    #[error("note for '{object}' changed concurrently during merge; re-run the merge")]
    MergeRaced { object: String },
    #[error(
        "a notes merge is already in progress; resolve it or run 'libra notes merge --abort' first"
    )]
    MergeInProgress,

    #[error(
        "another notes merge holds the lock at {0}; confirm no notes merge is running and delete that file"
    )]
    MergeLocked(String),

    #[error("failed to access notes merge state at {path}: {detail}")]
    MergeStateIo { path: String, detail: String },
    #[error("the notes merge state is corrupt: {0}")]
    MergeStateCorrupt(String),
}

impl From<std::io::Error> for NotesError {
    fn from(error: std::io::Error) -> Self {
        NotesError::StoreBlobFailed(error)
    }
}

/// Conflict-resolution strategy for `notes merge`. `Manual` (Git's default)
/// aborts on a conflicting note since Libra has no NOTES_MERGE worktree; the
/// others resolve automatically per object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteMergeStrategy {
    /// Abort the merge if any object has a differing note on both sides.
    Manual,
    /// Keep the current note on conflict.
    Ours,
    /// Take the other ref's note on conflict.
    Theirs,
    /// Concatenate both note contents.
    Union,
    /// Concatenate, sort, and de-duplicate the combined lines.
    CatSortUniq,
}

impl NoteMergeStrategy {
    /// Parse a `--strategy` value; `None` defaults to [`NoteMergeStrategy::Manual`].
    pub fn parse(value: Option<&str>) -> Result<Self, NotesError> {
        match value {
            None | Some("manual") => Ok(Self::Manual),
            Some("ours") => Ok(Self::Ours),
            Some("theirs") => Ok(Self::Theirs),
            Some("union") => Ok(Self::Union),
            Some("cat_sort_uniq") => Ok(Self::CatSortUniq),
            Some(other) => Err(NotesError::UnsupportedStrategy(other.to_string())),
        }
    }
}

/// Outcome of [`merge`].
#[derive(Debug, Clone)]
pub struct MergeNotesResult {
    pub notes_ref: String,
    pub other_ref: String,
    /// Notes copied (object new to the current ref) or conflict-resolved.
    pub merged: usize,
    /// Objects whose note was already identical (nothing to do).
    pub skipped: usize,
    /// Objects whose conflict was resolved by the chosen strategy.
    pub resolved_conflicts: Vec<String>,
}

/// Resolve an optional object string to a commit [`ObjectHash`].
///
/// When `object` is `None`, resolves HEAD. When `Some(s)`, delegates to
/// [`util::get_commit_base`].
pub async fn resolve_object(object: Option<&str>) -> Result<ObjectHash, NotesError> {
    match object {
        Some(s) if !s.is_empty() => resolve_ref(s).await,
        _ => resolve_head().await,
    }
}

async fn resolve_head() -> Result<ObjectHash, NotesError> {
    match crate::internal::head::Head::current_commit_result().await {
        Ok(Some(hash)) => Ok(hash),
        Ok(None) => Err(NotesError::HeadUnborn),
        Err(e) => Err(NotesError::InvalidObject("HEAD".to_string(), e.to_string())),
    }
}

async fn resolve_ref(s: &str) -> Result<ObjectHash, NotesError> {
    // When resolving HEAD explicitly, check for unborn HEAD via the Head API
    // so we surface the correct HeadUnborn error instead of a generic invalid-object.
    if s == "HEAD" {
        match crate::internal::head::Head::current_commit_result().await {
            Ok(Some(hash)) => return Ok(hash),
            Ok(None) => return Err(NotesError::HeadUnborn),
            Err(e) => return Err(NotesError::InvalidObject("HEAD".to_string(), e.to_string())),
        }
    }
    util::get_commit_base(s)
        .await
        .map_err(|e| NotesError::InvalidObject(s.to_string(), e))
}

/// Add a note directly against a [`crate::storage::GitStore`].
pub async fn add_with_store(
    store: &dyn crate::storage::GitStore,
    notes_ref: &str,
    object_str: &str,
    note_hash: &str,
    force: bool,
) -> Result<AddNoteResult, NotesError> {
    validate_notes_ref(notes_ref)?;
    let outcome = store
        .put_note(notes_ref, object_str, note_hash, force)
        .await
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
    match outcome {
        crate::storage::NoteWriteOutcome::AlreadyExists { .. } => Err(NotesError::AlreadyExists {
            notes_ref: notes_ref.to_string(),
            object: object_str.to_string(),
        }),
        _ => Ok(AddNoteResult {
            notes_ref: notes_ref.to_string(),
            object: object_str.to_string(),
            note_hash: note_hash.to_string(),
        }),
    }
}

/// List notes directly from a [`crate::storage::GitStore`].
pub async fn list_with_store(
    store: &dyn crate::storage::GitStore,
    notes_ref: &str,
    object_str: Option<&str>,
) -> Result<Vec<NoteEntry>, NotesError> {
    validate_notes_ref(notes_ref)?;
    if let Some(obj) = object_str {
        let note = store
            .get_note(notes_ref, obj)
            .await
            .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
        match note {
            Some(rec) => Ok(vec![NoteEntry {
                note_hash: Some(rec.blob),
                annotated_object: rec.object,
            }]),
            None => Ok(vec![NoteEntry {
                note_hash: None,
                annotated_object: obj.to_string(),
            }]),
        }
    } else {
        let notes = store
            .list_notes(notes_ref)
            .await
            .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
        Ok(notes
            .into_iter()
            .map(|rec| NoteEntry {
                note_hash: Some(rec.blob),
                annotated_object: rec.object,
            })
            .collect())
    }
}

/// Remove a note directly from a [`crate::storage::GitStore`].
pub async fn remove_with_store(
    store: &dyn crate::storage::GitStore,
    notes_ref: &str,
    object_str: &str,
) -> Result<String, NotesError> {
    validate_notes_ref(notes_ref)?;
    let existing = store
        .get_note(notes_ref, object_str)
        .await
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
        .ok_or_else(|| NotesError::NotFound {
            notes_ref: notes_ref.to_string(),
            object: object_str.to_string(),
        })?;
    let removed = store
        .remove_note_cas(notes_ref, object_str, Some(&existing.blob))
        .await
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
    if !removed {
        return Err(NotesError::NotFound {
            notes_ref: notes_ref.to_string(),
            object: object_str.to_string(),
        });
    }
    Ok(existing.blob)
}

/// Add a note to an object.
///
/// Creates a blob from `message`, stores it in the object database, and
/// inserts a row into the `notes` table. If `force` is true, overwrites an
/// existing note for the same (`notes_ref`, `object`) pair.
pub async fn add(
    notes_ref: &str,
    object: &str,
    message: &str,
    force: bool,
) -> Result<AddNoteResult, NotesError> {
    validate_notes_ref(notes_ref)?;
    let object_hash = resolve_object(Some(object)).await?;
    let object_str = object_hash.to_string();

    // Create and store the blob
    let blob = git_internal::internal::object::blob::Blob::from_content(message);
    let storage =
        crate::utils::client_storage::ClientStorage::try_init(crate::utils::path::objects())?;
    storage
        .put(&blob.id, &blob.data, blob.get_type())
        .map_err(NotesError::StoreBlobFailed)?;
    let note_hash = blob.id.to_string();

    match crate::storage::runtime::detect_current_repo_backend()
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
    {
        crate::storage::StorageBackendKind::Redb => {
            let store = crate::storage::runtime::open_current_git_store_read_write()
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
            add_with_store(store.as_ref(), notes_ref, &object_str, &note_hash, force).await
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db = get_db_conn_instance().await;

            if force {
                db.execute_raw(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Sqlite,
                    "INSERT INTO notes (notes_ref, object, blob) VALUES (?, ?, ?) \
                     ON CONFLICT(notes_ref, object) DO UPDATE SET blob = excluded.blob",
                    [
                        notes_ref.into(),
                        object_str.clone().into(),
                        note_hash.clone().into(),
                    ],
                ))
                .await?;
            } else {
                let result = db
                    .execute_raw(Statement::from_sql_and_values(
                        sea_orm::DatabaseBackend::Sqlite,
                        "INSERT OR IGNORE INTO notes (notes_ref, object, blob) VALUES (?, ?, ?)",
                        [
                            notes_ref.into(),
                            object_str.clone().into(),
                            note_hash.clone().into(),
                        ],
                    ))
                    .await?;

                if result.rows_affected() == 0 {
                    return Err(NotesError::AlreadyExists {
                        notes_ref: notes_ref.to_string(),
                        object: object_str,
                    });
                }
            }

            Ok(AddNoteResult {
                notes_ref: notes_ref.to_string(),
                object: object_str,
                note_hash,
            })
        }
    }
}

/// List notes in a notes ref.
///
/// When `object` is `Some`, returns only the note (if any) for that object.
/// When `None`, returns all notes in the given notes ref.
pub async fn list(notes_ref: &str, object: Option<&str>) -> Result<Vec<NoteEntry>, NotesError> {
    validate_notes_ref(notes_ref)?;
    let resolved_opt = match object {
        Some(obj) => Some(resolve_object(Some(obj)).await?.to_string()),
        None => None,
    };

    match crate::storage::runtime::detect_current_repo_backend()
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
    {
        crate::storage::StorageBackendKind::Redb => {
            let store = crate::storage::runtime::open_current_git_store_read_only()
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
            list_with_store(store.as_ref(), notes_ref, resolved_opt.as_deref()).await
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db = get_db_conn_instance().await;

            if let Some(obj_str) = resolved_opt {
                let rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        sea_orm::DatabaseBackend::Sqlite,
                        "SELECT blob, object FROM notes WHERE notes_ref = ? AND object = ?",
                        [notes_ref.into(), obj_str.clone().into()],
                    ))
                    .await?;
                if rows.is_empty() {
                    return Ok(vec![NoteEntry {
                        note_hash: None,
                        annotated_object: obj_str,
                    }]);
                }
                Ok(rows
                    .iter()
                    .map(|row| NoteEntry {
                        note_hash: Some(row.try_get::<String>("", "blob").unwrap_or_default()),
                        annotated_object: row.try_get::<String>("", "object").unwrap_or_default(),
                    })
                    .collect())
            } else {
                let rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        sea_orm::DatabaseBackend::Sqlite,
                        "SELECT blob, object FROM notes WHERE notes_ref = ?",
                        [notes_ref.into()],
                    ))
                    .await?;
                Ok(rows
                    .iter()
                    .map(|row| NoteEntry {
                        note_hash: Some(row.try_get::<String>("", "blob").unwrap_or_default()),
                        annotated_object: row.try_get::<String>("", "object").unwrap_or_default(),
                    })
                    .collect())
            }
        }
    }
}

/// Show the note text for an object.
pub async fn show(
    notes_ref: &str,
    object: Option<&str>,
) -> Result<(String, String, String), NotesError> {
    validate_notes_ref(notes_ref)?;
    let obj_hash = resolve_object(object).await?;
    let obj_str = obj_hash.to_string();

    let blob_hash = match crate::storage::runtime::detect_current_repo_backend()
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
    {
        crate::storage::StorageBackendKind::Redb => {
            let store = crate::storage::runtime::open_current_git_store_read_only()
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
            store
                .get_note(notes_ref, &obj_str)
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
                .map(|r| r.blob)
                .ok_or_else(|| NotesError::NotFound {
                    notes_ref: notes_ref.to_string(),
                    object: obj_str.clone(),
                })?
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db = get_db_conn_instance().await;
            find_note_blob(&db, notes_ref, &obj_str)
                .await?
                .ok_or_else(|| NotesError::NotFound {
                    notes_ref: notes_ref.to_string(),
                    object: obj_str.clone(),
                })?
        }
    };

    // Load the blob to get the text
    let blob_hash_parsed = crate::internal::object_format::parse_repo_oid(&blob_hash)
        .map_err(|e| NotesError::InvalidObject(blob_hash.clone(), e.to_string()))?;
    let storage =
        crate::utils::client_storage::ClientStorage::try_init(crate::utils::path::objects())?;
    let data = storage.get(&blob_hash_parsed).map_err(|e| {
        NotesError::InvalidObject(blob_hash.clone(), format!("failed to read blob: {e}"))
    })?;
    let text = String::from_utf8_lossy(&data).to_string();

    Ok((obj_str, blob_hash, text))
}

/// Remove notes for one or more objects.
///
/// Resolves and verifies every object before deleting any row so that a
/// partial-delete on mixed valid/invalid input is impossible: either all
/// targets are valid and the entire removal succeeds, or nothing is deleted
/// and the caller gets the first error.
///
/// Returns the list of (object, note_hash) that were removed.
pub async fn remove(
    notes_ref: &str,
    objects: &[String],
) -> Result<Vec<(String, String)>, NotesError> {
    validate_notes_ref(notes_ref)?;
    match crate::storage::runtime::detect_current_repo_backend()
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
    {
        crate::storage::StorageBackendKind::Redb => {
            let store = crate::storage::runtime::open_current_git_store_read_write()
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;

            // Phase 1: resolve and verify every target first.
            let mut to_delete: Vec<(String, String)> = Vec::new();
            for obj in objects {
                let resolved = resolve_object(Some(obj)).await?;
                let obj_str = resolved.to_string();
                let blob_hash = store
                    .get_note(notes_ref, &obj_str)
                    .await
                    .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
                    .map(|rec| rec.blob)
                    .ok_or_else(|| NotesError::NotFound {
                        notes_ref: notes_ref.to_string(),
                        object: obj_str.clone(),
                    })?;
                to_delete.push((obj_str, blob_hash));
            }

            // Phase 2: atomically delete all verified rows via CAS batch.
            let ops: Vec<crate::storage::NoteCasOp> = to_delete
                .iter()
                .map(
                    |(obj_str, blob_hash)| crate::storage::NoteCasOp::DeleteIfMatches {
                        object: obj_str.clone(),
                        expected_blob: blob_hash.clone(),
                    },
                )
                .collect();
            if !ops.is_empty() {
                match store
                    .apply_notes_cas_batch(notes_ref, &ops)
                    .await
                    .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
                {
                    crate::storage::NoteBatchOutcome::Applied { .. } => {}
                    crate::storage::NoteBatchOutcome::CasConflict { object, .. } => {
                        return Err(NotesError::NotFound {
                            notes_ref: notes_ref.to_string(),
                            object,
                        });
                    }
                }
            }

            Ok(to_delete)
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db = get_db_conn_instance().await;

            // Phase 1: resolve and verify every target first.
            let mut to_delete: Vec<(String, String)> = Vec::new();
            for obj in objects {
                let resolved = resolve_object(Some(obj)).await?;
                let obj_str = resolved.to_string();
                let blob_hash =
                    find_note_blob(&db, notes_ref, &obj_str)
                        .await?
                        .ok_or_else(|| NotesError::NotFound {
                            notes_ref: notes_ref.to_string(),
                            object: obj_str.clone(),
                        })?;
                to_delete.push((obj_str, blob_hash));
            }

            // Phase 2: delete all verified rows inside a single transaction.
            // Include `blob = ?` so that a concurrent `add -f` cannot overwrite
            // the blob between Phase 1 and Phase 2 — if the blob hash changed,
            // rows_affected() is 0 and we roll back the whole transaction.
            let txn = crate::internal::db::begin_write_transaction(&db).await?;
            for (obj_str, blob_hash) in &to_delete {
                let result = txn
                    .execute_raw(Statement::from_sql_and_values(
                        sea_orm::DatabaseBackend::Sqlite,
                        "DELETE FROM notes WHERE notes_ref = ? AND object = ? AND blob = ?",
                        [
                            notes_ref.into(),
                            obj_str.clone().into(),
                            blob_hash.clone().into(),
                        ],
                    ))
                    .await?;

                if result.rows_affected() == 0 {
                    txn.rollback().await?;
                    return Err(NotesError::NotFound {
                        notes_ref: notes_ref.to_string(),
                        object: obj_str.clone(),
                    });
                }
            }
            txn.commit().await?;

            Ok(to_delete)
        }
    }
}

/// Remove notes whose annotated object no longer exists in the object store
/// (`notes prune`). Returns the object ids actually pruned (sorted). With
/// `dry_run`, the stale notes are reported but not deleted.
///
/// A note is stale only when its annotated object is genuinely absent
/// (`GitError::ObjectNotFound`) or its id is malformed; any other object-store
/// read error aborts the whole prune (so a transient/corrupt read never deletes
/// a still-valid note). Unlike [`remove`], stale rows are NOT resolved through
/// `resolve_object` (their objects are gone) — instead they are deleted by a
/// `(notes_ref, object, blob)` compare-and-swap, so a note rewritten between
/// classification and deletion (0 rows affected) is left intact and not
/// reported as pruned.
pub async fn prune(notes_ref: &str, dry_run: bool) -> Result<Vec<String>, NotesError> {
    validate_notes_ref(notes_ref)?;

    let storage = util::objects_storage()?;
    // Each stale row carries its blob hash so the delete can compare-and-swap on
    // it (like `remove`): if the note was rewritten between classification and
    // deletion — e.g. its object was restored and re-annotated — the blob no
    // longer matches, the delete affects 0 rows, and the row is left intact.
    let mut stale: Vec<(String, Option<String>)> = Vec::new();
    for entry in list(notes_ref, None).await? {
        // A note is stale ONLY when its annotated object is genuinely absent
        // (`ObjectNotFound`) or its id is malformed and can never name an object.
        // Any other read error (transient/corrupt/tiered-storage failure) must
        // abort rather than risk deleting a note for a still-valid object.
        let missing = match crate::internal::object_format::parse_repo_oid(&entry.annotated_object)
        {
            Err(_) => true,
            Ok(hash) => match storage.get(&hash) {
                Ok(_) => false,
                Err(GitError::ObjectNotFound(_)) => true,
                Err(other) => {
                    return Err(NotesError::ResolveFailed(format!(
                        "failed to check object {} while pruning notes in {notes_ref}: {other}",
                        entry.annotated_object
                    )));
                }
            },
        };
        if missing {
            stale.push((entry.annotated_object, entry.note_hash));
        }
    }
    stale.sort();
    stale.dedup();

    if dry_run {
        return Ok(stale.into_iter().map(|(object, _)| object).collect());
    }

    match crate::storage::runtime::detect_current_repo_backend()
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
    {
        crate::storage::StorageBackendKind::Redb => {
            let store = crate::storage::runtime::open_current_git_store_read_write()
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
            let mut pruned = Vec::new();
            for (object, blob) in stale {
                let removed = store
                    .remove_note_cas(notes_ref, &object, blob.as_deref())
                    .await
                    .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
                if removed {
                    pruned.push(object);
                }
            }
            Ok(pruned)
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db = get_db_conn_instance().await;
            let txn = crate::internal::db::begin_write_transaction(&db).await?;
            let mut pruned = Vec::new();
            for (object, blob) in stale {
                let result = match &blob {
                    Some(blob) => {
                        txn.execute_raw(Statement::from_sql_and_values(
                            sea_orm::DatabaseBackend::Sqlite,
                            "DELETE FROM notes WHERE notes_ref = ? AND object = ? AND blob = ?",
                            [notes_ref.into(), object.clone().into(), blob.clone().into()],
                        ))
                        .await?
                    }
                    None => {
                        txn.execute_raw(Statement::from_sql_and_values(
                            sea_orm::DatabaseBackend::Sqlite,
                            "DELETE FROM notes WHERE notes_ref = ? AND object = ?",
                            [notes_ref.into(), object.clone().into()],
                        ))
                        .await?
                    }
                };
                // Only report rows actually removed: a 0-row delete means the note
                // changed concurrently and is no longer the stale row we inspected.
                if result.rows_affected() > 0 {
                    pruned.push(object);
                }
            }
            txn.commit().await?;

            Ok(pruned)
        }
    }
}

/// Merge the notes of `other_ref` into `notes_ref` (`notes merge <other-ref>`).
///
/// Libra notes are flat `(notes_ref, object, blob)` rows, not Git's commit-backed
/// notes trees, so there is no common base to 3-way against — this is a 2-way
/// merge: objects annotated only in `other_ref` are copied, identical notes are
/// skipped, and an object with a DIFFERING note on both sides is a conflict
/// resolved per `strategy` (`Manual` aborts the whole merge, applying nothing —
/// Libra has no NOTES_MERGE worktree for hand resolution). The merge is
/// all-or-nothing under `Manual`: conflicts are detected before any row changes.
pub async fn merge(
    notes_ref: &str,
    other_ref: &str,
    strategy: NoteMergeStrategy,
) -> Result<MergeNotesResult, NotesError> {
    validate_notes_ref(notes_ref)?;
    validate_notes_ref(other_ref)?;
    // All merge forms take the same worktree-scoped lock before touching state.
    let _lock = NotesMergeLock::acquire()?;
    ensure_no_merge_in_progress()?;
    let backend = crate::storage::runtime::detect_current_repo_backend()
        .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;

    // Classify every object annotated in `other_ref` against the current ref.
    let mut to_copy: Vec<(String, String)> = Vec::new(); // (object, other_blob)
    let mut conflicts: Vec<(String, String, String)> = Vec::new(); // (object, current_blob, other_blob)
    let mut skipped = 0usize;
    match backend {
        crate::storage::StorageBackendKind::Redb => {
            let store = crate::storage::runtime::open_current_git_store_read_only()
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
            let other_rows = store
                .list_notes(other_ref)
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
            for rec in other_rows {
                let current = store
                    .get_note(notes_ref, &rec.object)
                    .await
                    .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
                    .map(|r| r.blob);
                match current {
                    None => to_copy.push((rec.object, rec.blob)),
                    Some(current) if current == rec.blob => skipped += 1,
                    Some(current) => conflicts.push((rec.object, current, rec.blob)),
                }
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db = get_db_conn_instance().await;
            let other_rows = db
                .query_all_raw(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Sqlite,
                    "SELECT object, blob FROM notes WHERE notes_ref = ?",
                    [other_ref.into()],
                ))
                .await?;
            for row in &other_rows {
                let object = row.try_get::<String>("", "object")?;
                let other_blob = row.try_get::<String>("", "blob")?;
                match find_note_blob(&db, notes_ref, &object).await? {
                    None => to_copy.push((object, other_blob)),
                    Some(current) if current == other_blob => skipped += 1,
                    Some(current) => conflicts.push((object, current, other_blob)),
                }
            }
        }
    }

    // `Manual` leaves the `notes` table untouched and materializes the conflict
    // worktree plus the versioned state file for hand resolution.
    if strategy == NoteMergeStrategy::Manual && !conflicts.is_empty() {
        write_manual_conflict_state(notes_ref, other_ref, &to_copy, &conflicts)?;
        let objects = conflicts.into_iter().map(|(object, ..)| object).collect();
        return Err(NotesError::MergeConflict {
            notes_ref: notes_ref.to_string(),
            objects,
        });
    }

    // Precompute the resolved blob for each conflict BEFORE the transaction —
    // building union/cat_sort_uniq notes reads/writes the (content-addressed,
    // idempotent) object store, which must not happen inside the DB txn. `None`
    // means "keep current" (ours: no row change).
    let mut conflict_updates: Vec<(&str, Option<String>, &str)> = Vec::new(); // (object, new_blob, expected_current_blob)
    for (object, current_blob, other_blob) in &conflicts {
        let new_blob = match strategy {
            NoteMergeStrategy::Ours => None,
            NoteMergeStrategy::Theirs => Some(other_blob.clone()),
            NoteMergeStrategy::Union => {
                let combined = format!(
                    "{}\n{}",
                    load_blob_text(current_blob)?.trim_end_matches('\n'),
                    load_blob_text(other_blob)?
                );
                Some(store_blob_text(&combined)?)
            }
            NoteMergeStrategy::CatSortUniq => {
                let mut lines: Vec<String> = load_blob_text(current_blob)?
                    .lines()
                    .chain(load_blob_text(other_blob)?.lines())
                    .map(str::to_string)
                    .collect();
                lines.sort();
                lines.dedup();
                Some(store_blob_text(&format!("{}\n", lines.join("\n")))?)
            }
            NoteMergeStrategy::Manual => unreachable!("manual conflicts aborted above"),
        };
        conflict_updates.push((object.as_str(), new_blob, current_blob.as_str()));
    }

    // Apply every row mutation atomically with compare-and-swap guards: a copy
    // must still be absent (`ON CONFLICT DO NOTHING` + `rows_affected == 0` →
    // a note appeared), and a conflict update must still see the classified
    // current blob (`AND blob = <expected>`). Any mismatch means another writer
    // changed the notes between classification and apply, so the whole merge
    // rolls back rather than clobbering concurrent work or applying partially.
    match backend {
        crate::storage::StorageBackendKind::Redb => {
            let store = crate::storage::runtime::open_current_git_store_read_write()
                .await
                .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?;
            let mut ops: Vec<crate::storage::NoteCasOp> = Vec::new();
            for (object, blob) in &to_copy {
                ops.push(crate::storage::NoteCasOp::InsertIfAbsent {
                    object: object.clone(),
                    blob: blob.clone(),
                });
            }
            for (object, new_blob, expected_current) in &conflict_updates {
                if let Some(blob) = new_blob {
                    ops.push(crate::storage::NoteCasOp::UpdateIfMatches {
                        object: (*object).to_string(),
                        expected_blob: (*expected_current).to_string(),
                        new_blob: blob.clone(),
                    });
                }
            }
            if !ops.is_empty() {
                match store
                    .apply_notes_cas_batch(notes_ref, &ops)
                    .await
                    .map_err(|err| NotesError::from(DbErr::Custom(err.to_string())))?
                {
                    crate::storage::NoteBatchOutcome::Applied { .. } => {}
                    crate::storage::NoteBatchOutcome::CasConflict { object, .. } => {
                        return Err(NotesError::MergeRaced { object });
                    }
                }
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db = get_db_conn_instance().await;
            let txn = crate::internal::db::begin_write_transaction(&db).await?;
            for (object, blob) in &to_copy {
                let result = txn
                    .execute_raw(Statement::from_sql_and_values(
                        sea_orm::DatabaseBackend::Sqlite,
                        "INSERT INTO notes (notes_ref, object, blob) VALUES (?, ?, ?) \
                         ON CONFLICT(notes_ref, object) DO NOTHING",
                        [notes_ref.into(), object.clone().into(), blob.clone().into()],
                    ))
                    .await?;
                if result.rows_affected() == 0 {
                    txn.rollback().await?;
                    return Err(NotesError::MergeRaced {
                        object: object.clone(),
                    });
                }
            }
            for (object, new_blob, expected_current) in &conflict_updates {
                if let Some(blob) = new_blob {
                    let result = txn
                        .execute_raw(Statement::from_sql_and_values(
                            sea_orm::DatabaseBackend::Sqlite,
                            "UPDATE notes SET blob = ? WHERE notes_ref = ? AND object = ? AND blob = ?",
                            [
                                blob.clone().into(),
                                notes_ref.into(),
                                (*object).into(),
                                (*expected_current).into(),
                            ],
                        ))
                        .await?;
                    if result.rows_affected() == 0 {
                        txn.rollback().await?;
                        return Err(NotesError::MergeRaced {
                            object: (*object).to_string(),
                        });
                    }
                }
            }
            txn.commit().await?;
        }
    }

    let merged = to_copy.len() + conflicts.len();
    let resolved_conflicts: Vec<String> =
        conflicts.into_iter().map(|(object, ..)| object).collect();

    Ok(MergeNotesResult {
        notes_ref: notes_ref.to_string(),
        other_ref: other_ref.to_string(),
        merged,
        skipped,
        resolved_conflicts,
    })
}

/// Load a note blob's content as text (for the `union`/`cat_sort_uniq` strategies).
fn load_blob_text(blob_hash: &str) -> Result<String, NotesError> {
    let parsed = crate::internal::object_format::parse_repo_oid(blob_hash)
        .map_err(|e| NotesError::InvalidObject(blob_hash.to_string(), e.to_string()))?;
    let storage =
        crate::utils::client_storage::ClientStorage::try_init(crate::utils::path::objects())?;
    let data = storage.get(&parsed).map_err(|e| {
        NotesError::InvalidObject(blob_hash.to_string(), format!("failed to read blob: {e}"))
    })?;
    Ok(String::from_utf8_lossy(&data).to_string())
}

/// Store `text` as a note blob and return its hash (for strategies that build a
/// new merged note).
fn store_blob_text(text: &str) -> Result<String, NotesError> {
    let blob = git_internal::internal::object::blob::Blob::from_content(text);
    let storage =
        crate::utils::client_storage::ClientStorage::try_init(crate::utils::path::objects())?;
    storage
        .put(&blob.id, &blob.data, blob.get_type())
        .map_err(NotesError::StoreBlobFailed)?;
    Ok(blob.id.to_string())
}

/// Find the blob hash for a note, if it exists.
async fn find_note_blob(
    db: &sea_orm::DatabaseConnection,
    notes_ref: &str,
    object: &str,
) -> Result<Option<String>, DbErr> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT blob FROM notes WHERE notes_ref = ? AND object = ?",
            [notes_ref.into(), object.into()],
        ))
        .await?;
    Ok(rows
        .first()
        .map(|row| row.try_get::<String>("", "blob").unwrap_or_default()))
}
