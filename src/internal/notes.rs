//! Notes operations: add, list, show, and remove notes attached to commits.
//!
//! Notes are stored as blobs in the object store with mappings persisted in the
//! SQLite `notes` table. Each row maps a (`notes_ref`, `object`) pair to a blob
//! hash. The default notes ref is `refs/notes/commits`.

use std::str::FromStr;

use git_internal::{errors::GitError, hash::ObjectHash, internal::object::ObjectTrait};
use sea_orm::{ConnectionTrait, DbErr, Statement};

use crate::{internal::db::get_db_conn_instance, utils::util};

/// Default notes ref namespace.
pub const DEFAULT_NOTES_REF: &str = "refs/notes/commits";

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

    let db = get_db_conn_instance().await;

    // Create and store the blob
    let blob = git_internal::internal::object::blob::Blob::from_content(message);
    let storage = crate::utils::client_storage::ClientStorage::init(crate::utils::path::objects());
    storage
        .put(&blob.id, &blob.data, blob.get_type())
        .map_err(NotesError::StoreBlobFailed)?;
    let note_hash = blob.id.to_string();

    if force {
        // INSERT … ON CONFLICT DO UPDATE: atomic upsert — no check-then-act gap.
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
        // INSERT OR IGNORE: if the row already exists the statement is a
        // no-op and rows_affected() returns 0, so we detect the conflict
        // without a separate query that could land on a different pooled
        // connection.
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

/// List notes in a notes ref.
///
/// When `object` is `Some`, returns only the note (if any) for that object.
/// When `None`, returns all notes in the given notes ref.
pub async fn list(notes_ref: &str, object: Option<&str>) -> Result<Vec<NoteEntry>, NotesError> {
    validate_notes_ref(notes_ref)?;
    let db = get_db_conn_instance().await;

    if let Some(obj) = object {
        let resolved = resolve_object(Some(obj)).await?;
        let obj_str = resolved.to_string();
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

/// Show the note text for an object.
pub async fn show(
    notes_ref: &str,
    object: Option<&str>,
) -> Result<(String, String, String), NotesError> {
    validate_notes_ref(notes_ref)?;
    let obj_hash = resolve_object(object).await?;
    let obj_str = obj_hash.to_string();

    let db = get_db_conn_instance().await;
    let blob_hash = find_note_blob(&db, notes_ref, &obj_str)
        .await?
        .ok_or_else(|| NotesError::NotFound {
            notes_ref: notes_ref.to_string(),
            object: obj_str.clone(),
        })?;

    // Load the blob to get the text
    let blob_hash_parsed = ObjectHash::from_str(&blob_hash)
        .map_err(|e| NotesError::InvalidObject(blob_hash.clone(), e))?;
    let storage = crate::utils::client_storage::ClientStorage::init(crate::utils::path::objects());
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
    let db = get_db_conn_instance().await;

    // Phase 1: resolve and verify every target first.
    let mut to_delete: Vec<(String, String)> = Vec::new();
    for obj in objects {
        let resolved = resolve_object(Some(obj)).await?;
        let obj_str = resolved.to_string();
        let blob_hash = find_note_blob(&db, notes_ref, &obj_str)
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

    let storage = util::objects_storage();
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
        let missing = match ObjectHash::from_str(&entry.annotated_object) {
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
    let db = get_db_conn_instance().await;

    let other_rows = db
        .query_all_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT object, blob FROM notes WHERE notes_ref = ?",
            [other_ref.into()],
        ))
        .await?;

    // Classify every object annotated in `other_ref` against the current ref.
    let mut to_copy: Vec<(String, String)> = Vec::new(); // (object, other_blob)
    let mut conflicts: Vec<(String, String, String)> = Vec::new(); // (object, current_blob, other_blob)
    let mut skipped = 0usize;
    for row in &other_rows {
        let object = row.try_get::<String>("", "object")?;
        let other_blob = row.try_get::<String>("", "blob")?;
        match find_note_blob(&db, notes_ref, &object).await? {
            None => to_copy.push((object, other_blob)),
            Some(current) if current == other_blob => skipped += 1,
            Some(current) => conflicts.push((object, current, other_blob)),
        }
    }

    // `Manual` aborts (changing nothing) when any conflict remains.
    if strategy == NoteMergeStrategy::Manual && !conflicts.is_empty() {
        return Err(NotesError::MergeConflict {
            notes_ref: notes_ref.to_string(),
            objects: conflicts.into_iter().map(|(object, ..)| object).collect(),
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
    let parsed = ObjectHash::from_str(blob_hash)
        .map_err(|e| NotesError::InvalidObject(blob_hash.to_string(), e))?;
    let storage = crate::utils::client_storage::ClientStorage::init(crate::utils::path::objects());
    let data = storage.get(&parsed).map_err(|e| {
        NotesError::InvalidObject(blob_hash.to_string(), format!("failed to read blob: {e}"))
    })?;
    Ok(String::from_utf8_lossy(&data).to_string())
}

/// Store `text` as a note blob and return its hash (for strategies that build a
/// new merged note).
fn store_blob_text(text: &str) -> Result<String, NotesError> {
    let blob = git_internal::internal::object::blob::Blob::from_content(text);
    let storage = crate::utils::client_storage::ClientStorage::init(crate::utils::path::objects());
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
