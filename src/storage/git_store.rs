//! Object-safe [`GitStore`] core repository storage trait (`ADR-RB-04`,
//! `RB-04`, `RB-04b`).
//!
//! Covers all 7 core repository tables (`reference`, `reflog`, `object_index`,
//! `object_obliteration`, `notes`, `revision_ordinal`, `revision_ordinal_meta`)
//! plus same-transaction `agent_bridge_publication` bindings with identical
//! semantics across [`SqliteGitStore`] and [`RedbGitStore`].

use std::{collections::HashSet, fmt};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::storage::{
    backend::StorageBackendKind,
    error::{StorageBackendError, StorageResult},
};

#[cfg(feature = "storage-redb")]
#[path = "redb/git_store.rs"]
pub mod redb;
#[cfg(feature = "storage-sqlite")]
#[path = "sqlite/git_store.rs"]
pub mod sqlite;

#[cfg(feature = "storage-redb")]
pub use redb::RedbGitStore;
#[cfg(feature = "storage-sqlite")]
pub use sqlite::SqliteGitStore;

/// Canonical reflog target name for a worktree-scoped `HEAD` reflog.
pub const REFLOG_HEAD_REF_NAME: &str = "HEAD";

/// Maximum allowed byte length for an `agent_bridge_publication.operation_id`.
pub const MAX_BRIDGE_OPERATION_ID_LEN: usize = 128;

/// Required prefix for canonical Git/Libra notes references (`src/internal/notes.rs`).
pub const DEFAULT_NOTES_REF_PREFIX: &str = "refs/notes/";

/// Canonical error detail prefix emitted when `object_obliteration` fences a read or write.
pub const OBLITERATION_FENCE_ERROR_PREFIX: &str = "object_obliteration fence blocked";

/// Canonical Git object hash algorithm stored in `object_obliteration.hash_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ObjectHashKind {
    /// 40-character lowercase hexadecimal SHA-1 OID.
    Sha1,
    /// 64-character lowercase hexadecimal SHA-256 OID.
    Sha256,
}

impl ObjectHashKind {
    /// Canonical SQL text representation (`"sha1"` or `"sha256"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }

    /// Expected lowercase hex string length for OIDs of this hash kind.
    #[must_use]
    pub const fn oid_hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }

    /// Parse canonical SQL text representation (`"sha1"` or `"sha256"`).
    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "sha1" => Ok(Self::Sha1),
            "sha256" => Ok(Self::Sha256),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid object_obliteration hash_kind value".to_string(),
            }),
        }
    }
}

/// One row in the `object_index` table (`sql/sqlite_20260309_init.sql`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectIndexRecord {
    /// Repository UUID (`repo_id`).
    pub repo_id: String,
    /// Canonical lowercase hex object ID (`o_id`, SHA-1 40-hex or SHA-256 64-hex).
    pub o_id: String,
    /// Object type (`"blob"`, `"tree"`, `"commit"`, `"tag"`, or semantic `"agent_*"`).
    pub o_type: String,
    /// Uncompressed object payload size in bytes (`>= 0`).
    pub o_size: i64,
    /// Unix timestamp in seconds when the object was indexed.
    pub created_at: i64,
    /// Whether the object has been synchronized to remote cloud storage.
    pub is_synced: bool,
}

/// State machine phase stored in `object_obliteration.state` (`lore.md` §2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ObliterationState {
    /// Tombstone written and fsynced before physical payload removal.
    Obliterating,
    /// Payload physically deleted; permanent intentional-absence tombstone retained.
    Obliterated,
}

impl ObliterationState {
    /// Canonical SQL text representation (`"obliterating"` or `"obliterated"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Obliterating => "obliterating",
            Self::Obliterated => "obliterated",
        }
    }

    /// Parse canonical SQL text representation.
    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "obliterating" => Ok(Self::Obliterating),
            "obliterated" => Ok(Self::Obliterated),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid object_obliteration state value".to_string(),
            }),
        }
    }
}

/// Input payload to begin an index-flagged obliteration (`Live -> Obliterating`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObliterationRequest {
    /// Canonical lowercase hex OID whose payload is being obliterated.
    pub oid: String,
    /// Hash algorithm matching `oid` length (`Sha1` => 40, `Sha256` => 64).
    pub hash_kind: ObjectHashKind,
    /// Optional compliance erasure reason.
    pub reason: Option<String>,
    /// Optional actor initiating the obliteration.
    pub actor: Option<String>,
    /// Optional approval source identifier.
    pub approval_source: Option<String>,
    /// Timestamp string recorded for `requested_at`, `tombstone_written_at`, and `updated_at`.
    pub recorded_at: String,
}

/// Materialized permanent tombstone row in `object_obliteration`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectObliterationRecord {
    /// Content OID whose payload is being or has been removed.
    pub oid: String,
    /// Hash algorithm (`sha1` or `sha256`).
    pub hash_kind: ObjectHashKind,
    /// Current state (`obliterating` or `obliterated`).
    pub state: ObliterationState,
    /// Optional erasure reason.
    pub reason: Option<String>,
    /// Optional initiating actor.
    pub actor: Option<String>,
    /// Optional approval source.
    pub approval_source: Option<String>,
    /// Timestamp when obliteration was requested.
    pub requested_at: String,
    /// Timestamp when the durable tombstone was persisted.
    pub tombstone_written_at: String,
    /// Timestamp when payload bytes were physically deleted (`Some` once `Obliterated`).
    pub payload_deleted_at: Option<String>,
    /// Timestamp of the last state transition.
    pub updated_at: String,
}

/// One row in the `notes` table mapping `(notes_ref, object) -> blob`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteRecord {
    /// Notes reference starting with `"refs/notes/"`.
    pub notes_ref: String,
    /// Canonical lowercase hex OID of the annotated object.
    pub object: String,
    /// Canonical lowercase hex OID of the note blob.
    pub blob: String,
}

/// Result of [`GitStore::put_note`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteWriteOutcome {
    /// New note row inserted for `(notes_ref, object)`.
    Inserted,
    /// Existing note row replaced (`overwrite = true`).
    Replaced { previous_blob: String },
    /// Existing note row already present and left untouched (`overwrite = false`).
    AlreadyExists { existing_blob: String },
}

/// Single compare-and-swap operation inside [`GitStore::apply_notes_cas_batch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteCasOp {
    /// Insert `(notes_ref, object) -> blob` only if no note currently exists.
    InsertIfAbsent { object: String, blob: String },
    /// Update `(notes_ref, object)` to `new_blob` only if its current blob equals `expected_blob`.
    UpdateIfMatches {
        object: String,
        expected_blob: String,
        new_blob: String,
    },
    /// Delete `(notes_ref, object)` only if its current blob equals `expected_blob`.
    DeleteIfMatches {
        object: String,
        expected_blob: String,
    },
}

/// Result of [`GitStore::apply_notes_cas_batch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteBatchOutcome {
    /// Every operation's CAS precondition matched and committed in one transaction.
    Applied {
        inserted: usize,
        updated: usize,
        deleted: usize,
    },
    /// At least one operation's CAS precondition failed; entire transaction rolled back.
    CasConflict {
        object: String,
        actual_blob: Option<String>,
    },
}

/// Freshness metadata stored in `revision_ordinal_meta` (`lore.md` §1.16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionOrdinalMeta {
    /// Full reference name (e.g. `"refs/heads/main"`).
    pub ref_name: String,
    /// Canonical lowercase hex tip OID (SHA-1, SHA-256, or all-zero OID).
    pub tip_oid: String,
    /// Replace-set fingerprint digest (`""` when no replace refs exist).
    pub replace_sig: String,
    /// Highest 1-based ordinal indexed for `ref_name` (`0` when empty).
    pub max_ordinal: i64,
    /// Timestamp string when the index was built or last extended.
    pub built_at: String,
}

/// Result of [`GitStore::append_revision_ordinals`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionOrdinalAppendOutcome {
    /// Expected `(tip_oid, replace_sig)` matched; suffix appended and meta stamped atomically.
    Applied { meta: RevisionOrdinalMeta },
    /// Stored metadata was absent or did not match `(expected_tip_oid, expected_replace_sig)`;
    /// transaction aborted with zero writes.
    Stale { actual: Option<RevisionOrdinalMeta> },
}

/// Returns `true` when `error` indicates that `object_obliteration` fenced a
/// read or write attempt against an obliterated or obliterating object OID.
#[must_use]
pub fn is_obliteration_fenced_error(error: &StorageBackendError) -> bool {
    matches!(
        error,
        StorageBackendError::BackendInternal { detail }
            if detail.starts_with(OBLITERATION_FENCE_ERROR_PREFIX)
    )
}

pub(crate) fn obliteration_fenced_read_error(state: ObliterationState) -> StorageBackendError {
    StorageBackendError::BackendInternal {
        detail: format!(
            "{OBLITERATION_FENCE_ERROR_PREFIX} read of tombstoned object (state={})",
            state.as_str()
        ),
    }
}

pub(crate) fn obliteration_fenced_write_error(state: ObliterationState) -> StorageBackendError {
    StorageBackendError::BackendInternal {
        detail: format!(
            "{OBLITERATION_FENCE_ERROR_PREFIX} write of tombstoned object (state={})",
            state.as_str()
        ),
    }
}

/// Canonical identity of one row in the `reference` table (`RB-04`).
///
/// Enforces worktree scoping cleanly in the type system:
/// - [`Self::LocalHead`] is strictly scoped per worktree (`None` = main
///   worktree, `Some(id)` = linked worktree).
/// - [`Self::RemoteHead`], [`Self::LocalBranch`], [`Self::RemoteBranch`], and
///   [`Self::Tag`] are repository-wide shared references visible identically
///   across the main worktree and every linked worktree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RefKey {
    /// Worktree-local `HEAD` (`kind = 'Head'`, `remote IS NULL`).
    /// `worktree_id = None` identifies the main worktree; `Some(id)` identifies
    /// a linked worktree.
    LocalHead { worktree_id: Option<String> },
    /// Remote `HEAD` (`kind = 'Head'`, `remote = Some(remote)`, `worktree_id IS NULL`).
    RemoteHead { remote: String },
    /// Shared local branch (`kind = 'Branch'`, `remote IS NULL`, `worktree_id IS NULL`).
    LocalBranch { name: String },
    /// Shared remote-tracking branch (`kind = 'Branch'`, `remote = Some(remote)`, `worktree_id IS NULL`).
    RemoteBranch { remote: String, name: String },
    /// Shared tag (`kind = 'Tag'`, `remote IS NULL`, `worktree_id IS NULL`).
    Tag { name: String },
}

impl RefKey {
    /// Returns `true` if this reference is scoped to a specific worktree
    /// (`LocalHead`) rather than shared across all worktrees.
    #[must_use]
    pub const fn is_worktree_local(&self) -> bool {
        matches!(self, Self::LocalHead { .. })
    }

    /// Return the owning worktree ID if this is a [`Self::LocalHead`], or
    /// `None` for the main worktree and all shared references.
    #[must_use]
    pub fn worktree_id(&self) -> Option<&str> {
        match self {
            Self::LocalHead { worktree_id } => worktree_id.as_deref(),
            Self::RemoteHead { .. }
            | Self::LocalBranch { .. }
            | Self::RemoteBranch { .. }
            | Self::Tag { .. } => None,
        }
    }

    /// Returns `true` if this reference is visible to a worktree scope identified
    /// by `worktree_id` (`None` = main worktree, `Some(id)` = linked worktree).
    #[must_use]
    pub fn visible_to_worktree(&self, worktree_id: Option<&str>) -> bool {
        match self {
            Self::LocalHead { worktree_id: owner } => owner.as_deref() == worktree_id,
            Self::RemoteHead { .. }
            | Self::LocalBranch { .. }
            | Self::RemoteBranch { .. }
            | Self::Tag { .. } => true,
        }
    }
}

/// Target stored in a `reference` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefTarget {
    /// Direct object ID (commit/tag OID for branches, tags, or detached `HEAD`).
    Direct(String),
    /// Symbolic branch name (valid for `LocalHead` and `RemoteHead`).
    Symbolic(String),
}

/// One materialized `reference` entry returned by listing APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefRecord {
    /// Canonical reference key.
    pub key: RefKey,
    /// Current target (direct OID or symbolic branch name).
    pub target: RefTarget,
}

/// Input payload for appending one entry to `reflog` inside a ref update transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflogWrite {
    /// Reference name recorded in `reflog.ref_name` (e.g. `"HEAD"` or `"refs/heads/main"`).
    pub ref_name: String,
    /// Previous object ID (or all-zero OID on creation).
    pub old_oid: String,
    /// New object ID (or all-zero OID on deletion).
    pub new_oid: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
    /// Committer display name.
    pub committer_name: String,
    /// Committer email address.
    pub committer_email: String,
    /// Short reflog action tag (e.g. `"commit"`, `"checkout"`, `"update-ref"`).
    pub action: String,
    /// Human-readable reflog message body.
    pub message: String,
    /// Worktree scope for `HEAD` reflog rows (`None` for main worktree and all
    /// shared branch reflogs; `Some(id)` only when `ref_name == "HEAD"`).
    pub worktree_id: Option<String>,
}

/// Materialized `reflog` row returned by [`GitStore::list_reflog`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflogEntry {
    /// Monotonic primary key assigned by the backend (`id`).
    pub id: i64,
    /// Reference name recorded in `reflog.ref_name`.
    pub ref_name: String,
    /// Previous object ID.
    pub old_oid: String,
    /// New object ID.
    pub new_oid: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
    /// Committer display name.
    pub committer_name: String,
    /// Committer email address.
    pub committer_email: String,
    /// Short reflog action tag.
    pub action: String,
    /// Human-readable reflog message body.
    pub message: String,
    /// Owning worktree ID for linked-worktree `HEAD` reflog rows, or `None`.
    pub worktree_id: Option<String>,
}

/// Lifecycle phase recorded in `agent_bridge_publication.phase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BridgePublicationPhase {
    /// Commit object written before ref publication committed.
    Prepared,
    /// Reference move and operation-to-commit binding committed atomically.
    Published,
}

impl BridgePublicationPhase {
    /// Canonical SQL text representation matching `src/internal/ai/agent_bridge/publication.rs`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Published => "published",
        }
    }

    /// Parse canonical SQL text phase representation.
    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "prepared" => Ok(Self::Prepared),
            "published" => Ok(Self::Published),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_bridge_publication phase value".to_string(),
            }),
        }
    }
}

/// One row in `agent_bridge_publication` binding a bridge `operation_id` to
/// its published `commit_oid` inside the same transaction that moves `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgePublicationRecord {
    /// Bridge operation identifier (1..=128 bytes of `[A-Za-z0-9_.:-]`).
    pub operation_id: String,
    /// Published commit object ID.
    pub commit_oid: String,
    /// Publication lifecycle phase (`prepared` or `published`).
    pub phase: BridgePublicationPhase,
}

/// Result of an atomic compare-and-swap reference update (`RB-04` AC1–AC3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefCasOutcome {
    /// Expected old target matched and the ref update (plus any reflog and
    /// bridge publication records) committed atomically in a single transaction.
    Applied {
        /// Prior target before the update (`None` for creation).
        previous: Option<RefTarget>,
        /// Resulting target after the update (`None` for deletion).
        current: Option<RefTarget>,
    },
    /// Expected old target did not match; transaction aborted with zero ref,
    /// reflog, or bridge publication writes.
    CasConflict {
        /// Actual current target stored in the database.
        actual: Option<RefTarget>,
    },
}

/// Returns `true` when `operation_id` satisfies the bindable bridge identifier
/// invariant (`src/internal/ai/agent_bridge/publication.rs::operation_id_is_bindable`).
#[must_use]
pub fn is_valid_bridge_operation_id(operation_id: &str) -> bool {
    !operation_id.is_empty()
        && operation_id.len() <= MAX_BRIDGE_OPERATION_ID_LEN
        && operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

pub(crate) fn validate_non_empty_identifier(value: &str, label: &'static str) -> StorageResult<()> {
    if value.is_empty() || value.contains('\0') {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("{label} must be non-empty and contain no NUL bytes"),
        });
    }
    Ok(())
}

fn validate_optional_no_nul(value: Option<&str>, label: &'static str) -> StorageResult<()> {
    if let Some(v) = value
        && v.contains('\0')
    {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("{label} must not contain NUL bytes"),
        });
    }
    Ok(())
}

/// Validate a canonical lowercase hexadecimal Git object ID.
///
/// Accepts 40-char SHA-1 OIDs, 64-char SHA-256 OIDs, and all-zero OIDs of
/// either length. Rejects uppercase hex so `object_obliteration` fence lookups
/// cannot be bypassed via case variation.
pub(crate) fn validate_hex_oid(oid: &str, label: &'static str) -> StorageResult<()> {
    let valid_len = matches!(oid.len(), 40 | 64);
    let valid_hex = oid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !valid_len || !valid_hex {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("{label} must be a 40- or 64-character lowercase hexadecimal OID"),
        });
    }
    Ok(())
}

/// Validate that `oid` is canonical lowercase hex matching `hash_kind`.
pub(crate) fn validate_oid_for_hash_kind(
    oid: &str,
    hash_kind: ObjectHashKind,
) -> StorageResult<()> {
    validate_hex_oid(oid, "object OID")?;
    if oid.len() != hash_kind.oid_hex_len() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "object OID length does not match hash_kind `{}`",
                hash_kind.as_str()
            ),
        });
    }
    Ok(())
}

/// Validate one [`ObjectIndexRecord`] before entering a transaction.
pub(crate) fn validate_object_index_record(record: &ObjectIndexRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&record.repo_id, "object_index repo_id")?;
    validate_hex_oid(&record.o_id, "object_index o_id")?;
    validate_non_empty_identifier(&record.o_type, "object_index o_type")?;
    if record.o_size < 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "object_index o_size must be non-negative".to_string(),
        });
    }
    Ok(())
}

/// Canonical `object_index` conflict promotion rule (`src/utils/client_storage.rs`):
/// a semantic `agent_*` type promotes a generic non-`agent_*` row (resetting
/// `is_synced = false`), whereas a later generic write never demotes an
/// existing semantic row or reopens a synced upload.
#[cfg_attr(not(feature = "storage-redb"), allow(dead_code))]
pub(crate) fn should_promote_object_index_type(existing_type: &str, incoming_type: &str) -> bool {
    incoming_type.starts_with("agent_") && !existing_type.starts_with("agent_")
}

/// Validate an [`ObliterationRequest`] before entering a write transaction.
pub(crate) fn validate_obliteration_request(req: &ObliterationRequest) -> StorageResult<()> {
    validate_oid_for_hash_kind(&req.oid, req.hash_kind)?;
    validate_non_empty_identifier(&req.recorded_at, "obliteration recorded_at")?;
    validate_optional_no_nul(req.reason.as_deref(), "obliteration reason")?;
    validate_optional_no_nul(req.actor.as_deref(), "obliteration actor")?;
    validate_optional_no_nul(
        req.approval_source.as_deref(),
        "obliteration approval_source",
    )
}

/// Validate a canonical notes reference (`refs/notes/<name>`).
pub(crate) fn validate_notes_ref(notes_ref: &str) -> StorageResult<()> {
    if !notes_ref.starts_with(DEFAULT_NOTES_REF_PREFIX)
        || notes_ref.len() == DEFAULT_NOTES_REF_PREFIX.len()
        || notes_ref.contains('\0')
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "notes_ref must start with `refs/notes/` followed by a non-empty name"
                .to_string(),
        });
    }
    Ok(())
}

/// Validate one [`NoteCasOp`] before entering a write transaction.
pub(crate) fn validate_note_cas_op(op: &NoteCasOp) -> StorageResult<()> {
    match op {
        NoteCasOp::InsertIfAbsent { object, blob } => {
            validate_hex_oid(object, "note object OID")?;
            validate_hex_oid(blob, "note blob OID")
        }
        NoteCasOp::UpdateIfMatches {
            object,
            expected_blob,
            new_blob,
        } => {
            validate_hex_oid(object, "note object OID")?;
            validate_hex_oid(expected_blob, "note expected_blob OID")?;
            validate_hex_oid(new_blob, "note new_blob OID")
        }
        NoteCasOp::DeleteIfMatches {
            object,
            expected_blob,
        } => {
            validate_hex_oid(object, "note object OID")?;
            validate_hex_oid(expected_blob, "note expected_blob OID")
        }
    }
}

/// Validate a [`RevisionOrdinalMeta`] header.
pub(crate) fn validate_revision_ordinal_meta(meta: &RevisionOrdinalMeta) -> StorageResult<()> {
    validate_non_empty_identifier(&meta.ref_name, "revision_ordinal ref_name")?;
    validate_hex_oid(&meta.tip_oid, "revision_ordinal tip_oid")?;
    if meta.replace_sig.contains('\0') {
        return Err(StorageBackendError::BackendInternal {
            detail: "revision_ordinal replace_sig must not contain NUL bytes".to_string(),
        });
    }
    if meta.max_ordinal < 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "revision_ordinal max_ordinal must be non-negative".to_string(),
        });
    }
    validate_non_empty_identifier(&meta.built_at, "revision_ordinal built_at")
}

/// Validate a full `replace_revision_ordinals` payload (1-based contiguous ordinals,
/// unique canonical OIDs, and `meta.tip_oid` agreement).
pub(crate) fn validate_revision_ordinal_entries(
    meta: &RevisionOrdinalMeta,
    entries: &[(i64, String)],
) -> StorageResult<()> {
    validate_revision_ordinal_meta(meta)?;
    let expected_len =
        i64::try_from(entries.len()).map_err(|_| StorageBackendError::BackendInternal {
            detail: "revision_ordinal entry count overflowed i64".to_string(),
        })?;
    if meta.max_ordinal != expected_len {
        return Err(StorageBackendError::BackendInternal {
            detail: "revision_ordinal meta.max_ordinal must match entries length".to_string(),
        });
    }
    let mut seen_oids = HashSet::with_capacity(entries.len());
    for (idx, (ordinal, oid)) in entries.iter().enumerate() {
        let expected_ord =
            i64::try_from(idx + 1).map_err(|_| StorageBackendError::BackendInternal {
                detail: "revision_ordinal index overflowed i64".to_string(),
            })?;
        if *ordinal != expected_ord {
            return Err(StorageBackendError::BackendInternal {
                detail: "revision_ordinal entries must carry contiguous 1-based ordinals"
                    .to_string(),
            });
        }
        validate_hex_oid(oid, "revision_ordinal oid")?;
        if !seen_oids.insert(oid.as_str()) {
            return Err(StorageBackendError::BackendInternal {
                detail: "revision_ordinal entries must not contain duplicate OIDs for a ref"
                    .to_string(),
            });
        }
    }
    if let Some((_, tip_entry_oid)) = entries.last()
        && tip_entry_oid != &meta.tip_oid
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "revision_ordinal meta.tip_oid must match the highest-ordinal entry OID"
                .to_string(),
        });
    }
    Ok(())
}

/// Validate an incremental `append_revision_ordinals` request before opening a transaction.
pub(crate) fn validate_revision_ordinal_append(
    ref_name: &str,
    expected_tip_oid: &str,
    expected_replace_sig: &str,
    new_meta: &RevisionOrdinalMeta,
    suffix_oids: &[String],
) -> StorageResult<()> {
    validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
    validate_hex_oid(expected_tip_oid, "revision_ordinal expected_tip_oid")?;
    if expected_replace_sig.contains('\0') {
        return Err(StorageBackendError::BackendInternal {
            detail: "revision_ordinal expected_replace_sig must not contain NUL bytes".to_string(),
        });
    }
    validate_revision_ordinal_meta(new_meta)?;
    if new_meta.ref_name != ref_name {
        return Err(StorageBackendError::BackendInternal {
            detail: "revision_ordinal append new_meta.ref_name must match ref_name".to_string(),
        });
    }
    if new_meta.replace_sig != expected_replace_sig {
        return Err(StorageBackendError::BackendInternal {
            detail: "revision_ordinal append cannot change replace_sig; use full rebuild"
                .to_string(),
        });
    }
    if suffix_oids.is_empty() {
        return Err(StorageBackendError::BackendInternal {
            detail: "revision_ordinal append requires at least one suffix OID".to_string(),
        });
    }
    let mut seen = HashSet::with_capacity(suffix_oids.len());
    for oid in suffix_oids {
        validate_hex_oid(oid, "revision_ordinal suffix oid")?;
        if !seen.insert(oid.as_str()) {
            return Err(StorageBackendError::BackendInternal {
                detail: "revision_ordinal suffix must not contain duplicate OIDs".to_string(),
            });
        }
    }
    if suffix_oids.last().map(String::as_str) != Some(new_meta.tip_oid.as_str()) {
        return Err(StorageBackendError::BackendInternal {
            detail: "revision_ordinal append new_meta.tip_oid must match the final suffix OID"
                .to_string(),
        });
    }
    Ok(())
}

/// Validate a [`RefKey`] against SQLite and `redb` storage invariants.
pub(crate) fn validate_ref_key(key: &RefKey) -> StorageResult<()> {
    match key {
        RefKey::LocalHead { worktree_id } => {
            if let Some(wt) = worktree_id {
                validate_non_empty_identifier(wt, "worktree_id")?;
            }
            Ok(())
        }
        RefKey::RemoteHead { remote } => validate_non_empty_identifier(remote, "remote"),
        RefKey::LocalBranch { name } => validate_non_empty_identifier(name, "branch name"),
        RefKey::RemoteBranch { remote, name } => {
            validate_non_empty_identifier(remote, "remote")?;
            validate_non_empty_identifier(name, "branch name")
        }
        RefKey::Tag { name } => validate_non_empty_identifier(name, "tag name"),
    }
}

/// Validate that `target` is well-formed and permitted for `key`.
pub(crate) fn validate_ref_target(key: &RefKey, target: &RefTarget) -> StorageResult<()> {
    match target {
        RefTarget::Direct(oid) => validate_non_empty_identifier(oid, "ref commit OID"),
        RefTarget::Symbolic(branch_name) => {
            if !matches!(key, RefKey::LocalHead { .. } | RefKey::RemoteHead { .. }) {
                return Err(StorageBackendError::BackendInternal {
                    detail: "symbolic targets are only valid for LocalHead or RemoteHead"
                        .to_string(),
                });
            }
            validate_non_empty_identifier(branch_name, "symbolic target branch name")
        }
    }
}

/// Validate a [`ReflogWrite`] against worktree-scoping invariants (`src/internal/reflog.rs`).
pub(crate) fn validate_reflog_write(entry: &ReflogWrite) -> StorageResult<()> {
    validate_non_empty_identifier(&entry.ref_name, "reflog ref_name")?;
    validate_non_empty_identifier(&entry.old_oid, "reflog old_oid")?;
    validate_non_empty_identifier(&entry.new_oid, "reflog new_oid")?;
    if let Some(wt) = &entry.worktree_id {
        validate_non_empty_identifier(wt, "reflog worktree_id")?;
        if entry.ref_name != REFLOG_HEAD_REF_NAME {
            return Err(StorageBackendError::BackendInternal {
                detail: "non-HEAD reflog entries must not set a per-worktree worktree_id"
                    .to_string(),
            });
        }
    }
    Ok(())
}

/// Validate that a `reflog` batch respects worktree ownership for `key` (`RB-04`).
///
/// - When `key` is `LocalHead { worktree_id }`, any `"HEAD"` reflog entry must
///   target the exact same `worktree_id` (cannot cross main ↔ linked or
///   linked-A ↔ linked-B worktree boundaries).
/// - When `key` is `RemoteHead`, `RemoteBranch`, or `Tag`, `"HEAD"` reflog
///   entries are forbidden.
/// - When `key` is `LocalBranch`, any `"HEAD"` reflog entry is further checked
///   inside the write transaction to verify that worktree's `LocalHead` points
///   symbolically to that branch.
pub(crate) fn validate_reflog_writes_for_key(
    key: &RefKey,
    reflogs: &[ReflogWrite],
) -> StorageResult<()> {
    for entry in reflogs {
        validate_reflog_write(entry)?;
        if entry.ref_name == REFLOG_HEAD_REF_NAME {
            match key {
                RefKey::LocalHead { worktree_id } => {
                    if entry.worktree_id != *worktree_id {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "HEAD reflog worktree_id must match the LocalHead being updated"
                                    .to_string(),
                        });
                    }
                }
                RefKey::LocalBranch { .. } => {
                    // Verified against live `LocalHead` symbolic target inside the transaction.
                }
                RefKey::RemoteHead { .. } | RefKey::RemoteBranch { .. } | RefKey::Tag { .. } => {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "cannot append a HEAD reflog entry for a non-HEAD, non-local-branch reference"
                            .to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Validate that a bridge publication record is tightly bound to the reference
/// transition being committed (`ADR-RB-04` #1, `RB-04` AC3):
/// - `publication.phase` must be [`BridgePublicationPhase::Published`].
/// - `new_target` must be `Some(RefTarget::Direct(oid))` with `oid == publication.commit_oid`.
/// - `expected` must differ from `new_target` (refusing no-op publication bindings).
/// - `key` must be either [`RefKey::LocalHead`] (detached `HEAD` move) or
///   [`RefKey::LocalBranch`] (further verified inside the write transaction to
///   be the symbolic target of a worktree's `LocalHead`).
pub(crate) fn validate_bridge_publication_binding(
    key: &RefKey,
    expected: Option<&RefTarget>,
    new_target: Option<&RefTarget>,
    publication: &BridgePublicationRecord,
) -> StorageResult<()> {
    validate_bridge_publication(publication)?;
    if publication.phase != BridgePublicationPhase::Published {
        return Err(StorageBackendError::BackendInternal {
            detail: "update_ref_cas_with_bridge_publication requires phase=published".to_string(),
        });
    }
    if !matches!(key, RefKey::LocalHead { .. } | RefKey::LocalBranch { .. }) {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication may only bind a LocalHead or checked-out LocalBranch move"
                .to_string(),
        });
    }
    let Some(RefTarget::Direct(target_oid)) = new_target else {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication requires a direct commit OID ref target".to_string(),
        });
    };
    if target_oid != &publication.commit_oid {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication commit_oid must match the new ref target OID".to_string(),
        });
    }
    if expected == new_target {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication requires a ref transition (expected must differ from new_target)"
                .to_string(),
        });
    }
    Ok(())
}

/// Validate a [`BridgePublicationRecord`] before entering a write transaction.
pub(crate) fn validate_bridge_publication(record: &BridgePublicationRecord) -> StorageResult<()> {
    if !is_valid_bridge_operation_id(&record.operation_id) {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication operation_id is empty or contains invalid characters"
                .to_string(),
        });
    }
    validate_non_empty_identifier(&record.commit_oid, "bridge publication commit_oid")
}

/// Validate a worktree filter passed to read queries.
pub(crate) fn validate_worktree_filter(worktree_id: Option<&str>) -> StorageResult<()> {
    if let Some(wt) = worktree_id {
        validate_non_empty_identifier(wt, "worktree_id")?;
    }
    Ok(())
}

/// Object-safe core Git reference, reflog, and bridge publication store
/// abstraction backed by SQLite or `redb 5.0` (`ADR-RB-04` #1, `RB-04`).
#[async_trait]
pub trait GitStore: Send + Sync + fmt::Debug {
    /// Active storage backend kind (`Sqlite` or `Redb`).
    fn backend(&self) -> StorageBackendKind;

    /// Read a single reference target by exact [`RefKey`], or `Ok(None)` if absent.
    async fn get_ref(&self, key: &RefKey) -> StorageResult<Option<RefTarget>>;

    /// List all references visible to `worktree_id` (`None` = main worktree,
    /// `Some(id)` = linked worktree), sorted deterministically by [`RefKey`].
    ///
    /// Returns ONLY the requested worktree's own [`RefKey::LocalHead`] (if
    /// present) plus ALL shared non-local references (`RemoteHead`,
    /// `LocalBranch`, `RemoteBranch`, `Tag`).
    async fn list_refs(&self, worktree_id: Option<&str>) -> StorageResult<Vec<RefRecord>>;

    /// List every [`RefKey::LocalHead`] row across main and all linked worktrees,
    /// sorted deterministically by [`RefKey`].
    async fn list_all_local_heads(&self) -> StorageResult<Vec<RefRecord>>;

    /// Atomically compare `key` against `expected`, apply `new_target` (`Some`
    /// to upsert, `None` to delete), and append all `reflogs` in a single
    /// database write transaction (`RB-04` AC1–AC2).
    ///
    /// On mismatch, aborts the transaction with zero writes and returns
    /// [`RefCasOutcome::CasConflict`].
    async fn update_ref_cas(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
    ) -> StorageResult<RefCasOutcome>;

    /// Atomically execute [`Self::update_ref_cas`] together with binding
    /// `publication` in `agent_bridge_publication` inside the SAME database
    /// write transaction (`ADR-RB-04` #1, `RB-04` AC3).
    ///
    /// If the CAS check fails (`CasConflict`) OR `publication.operation_id` is
    /// already bound to a different `commit_oid` (or invalid), the entire
    /// transaction rolls back leaving zero orphan ref, reflog, or bridge
    /// publication rows.
    async fn update_ref_cas_with_bridge_publication(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
        publication: &BridgePublicationRecord,
    ) -> StorageResult<RefCasOutcome>;

    /// List reflog entries for `(ref_name, worktree_id)` ordered newest-first
    /// (`id DESC`, matching `HEAD@{0}` at index 0).
    ///
    /// For `ref_name == "HEAD"`, `worktree_id` strictly isolates the main
    /// worktree (`None`) from linked worktrees (`Some(id)`). For all other
    /// ref names, `worktree_id` must be `None`.
    async fn list_reflog(
        &self,
        ref_name: &str,
        worktree_id: Option<&str>,
        limit: Option<usize>,
    ) -> StorageResult<Vec<ReflogEntry>>;

    /// Record a `prepared` bridge publication row before the ref update commits
    /// (`src/internal/ai/agent_bridge/publication.rs::prepare_current`).
    async fn prepare_bridge_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<()>;

    /// Look up one `agent_bridge_publication` record by `operation_id`.
    async fn get_bridge_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgePublicationRecord>>;

    /// List all `agent_bridge_publication` records sorted by `operation_id ASC`.
    async fn list_bridge_publications(&self) -> StorageResult<Vec<BridgePublicationRecord>>;

    // -------------------------------------------------------------------------
    // `object_index` (`RB-04b` AC1–AC3)
    // -------------------------------------------------------------------------

    /// Atomically and idempotently upsert `records` into `object_index` in a
    /// single database write transaction (`RB-04b` AC1, AC3).
    ///
    /// Enforces the `object_obliteration` write fence across the entire batch:
    /// if ANY `record.o_id` is tombstoned (`obliterating` or `obliterated`),
    /// the entire transaction aborts with zero partial writes.
    async fn upsert_object_index_batch(&self, records: &[ObjectIndexRecord]) -> StorageResult<()>;

    /// Look up one `object_index` row for `(repo_id, o_id)` with atomic
    /// `object_obliteration` read-fence enforcement (`RB-04b` AC2).
    ///
    /// Fails closed with an obliteration-fenced error when `o_id` carries an
    /// `object_obliteration` tombstone (in either `obliterating` or
    /// `obliterated` state), even if the historical registry row is preserved.
    async fn get_object_index(
        &self,
        repo_id: &str,
        o_id: &str,
    ) -> StorageResult<Option<ObjectIndexRecord>>;

    /// Compliance/fsck read of a preserved `object_index` address row without
    /// triggering the payload-read obliteration fence (`lore.md` §2.5).
    async fn get_object_index_preserved_address(
        &self,
        repo_id: &str,
        o_id: &str,
    ) -> StorageResult<Option<ObjectIndexRecord>>;

    /// List live `object_index` rows for `repo_id` (optionally filtered by
    /// `is_synced`), deterministically sorted by `o_id ASC` and excluding any
    /// OID fenced by `object_obliteration`.
    async fn list_object_index_by_repo(
        &self,
        repo_id: &str,
        synced_filter: Option<bool>,
    ) -> StorageResult<Vec<ObjectIndexRecord>>;

    /// Atomically mark live `object_index` rows as synced (`is_synced = true`)
    /// inside a single write transaction, returning the number of rows
    /// transitioned from unsynced to synced. Fails closed with zero writes if
    /// any requested OID is tombstoned in `object_obliteration`.
    async fn mark_object_index_synced(
        &self,
        repo_id: &str,
        o_ids: &[String],
    ) -> StorageResult<usize>;

    // -------------------------------------------------------------------------
    // `object_obliteration` fence (`RB-04b` AC2–AC3)
    // -------------------------------------------------------------------------

    /// Record a permanent `obliterating` tombstone (`Live -> Obliterating`),
    /// idempotent on `(oid, hash_kind)`.
    async fn begin_object_obliteration(
        &self,
        req: &ObliterationRequest,
    ) -> StorageResult<ObjectObliterationRecord>;

    /// Finalize an existing tombstone (`Obliterating -> Obliterated`) after
    /// physical payload deletion; idempotent once `Obliterated`, and returns
    /// `NotFound` if no tombstone exists yet.
    async fn mark_object_obliterated(
        &self,
        oid: &str,
        hash_kind: ObjectHashKind,
        deleted_at: &str,
    ) -> StorageResult<ObjectObliterationRecord>;

    /// Look up the permanent `object_obliteration` tombstone for `oid`.
    async fn get_object_obliteration(
        &self,
        oid: &str,
    ) -> StorageResult<Option<ObjectObliterationRecord>>;

    /// Return `true` if `oid` has a tombstone in ANY state (`obliterating` or `obliterated`).
    async fn is_object_obliterated(&self, oid: &str) -> StorageResult<bool>;

    /// List all incomplete (`state = 'obliterating'`) tombstones sorted by `(oid, hash_kind)`.
    async fn list_incomplete_obliterations(&self) -> StorageResult<Vec<ObjectObliterationRecord>>;

    /// List all tombstoned OIDs (in either state) sorted ascending.
    async fn list_obliterated_oids(&self) -> StorageResult<Vec<String>>;

    /// Execute a payload read under the monotonic `object_obliteration` read
    /// fence (`RB-04b` AC2).
    ///
    /// Checks `object_obliteration` before invoking `read_payload` and
    /// re-verifies after `read_payload` completes (discarding loaded bytes or
    /// overriding post-unlink I/O misses if a tombstone committed mid-read).
    /// Because tombstones transition strictly monotonically
    /// (`Live -> Obliterating -> Obliterated`) and commit before payload
    /// unlinking, this closes the read-vs-obliteration TOCTOU window without
    /// taking write locks on reads.
    async fn run_fenced_object_read(
        &self,
        oid: &str,
        read_payload: Box<dyn FnOnce() -> StorageResult<Vec<u8>> + Send>,
    ) -> StorageResult<Vec<u8>>;

    /// Execute a payload write together with `object_index` registration inside
    /// a single exclusive cross-process write transaction guarded by the
    /// `object_obliteration` write fence (`RB-04b` AC3).
    ///
    /// Holds the backend's exclusive write transaction (`BEGIN IMMEDIATE` in
    /// SQLite, `WriteTransaction` in `redb`) across the tombstone check,
    /// `write_payload`, and `object_index` upsert so no concurrent
    /// `begin_object_obliteration` can interleave between the check and payload
    /// materialization. If `record.o_id` is tombstoned, `write_payload` is
    /// never invoked.
    async fn run_fenced_object_write(
        &self,
        record: &ObjectIndexRecord,
        write_payload: Box<dyn FnOnce() -> StorageResult<()> + Send>,
    ) -> StorageResult<()>;

    // -------------------------------------------------------------------------
    // `notes` (`RB-04b` AC4)
    // -------------------------------------------------------------------------

    /// Look up one note mapping `(notes_ref, object) -> blob`.
    async fn get_note(&self, notes_ref: &str, object: &str) -> StorageResult<Option<NoteRecord>>;

    /// List all notes under `notes_ref` sorted deterministically by `object ASC`.
    async fn list_notes(&self, notes_ref: &str) -> StorageResult<Vec<NoteRecord>>;

    /// Insert or (when `overwrite == true`) replace the note for `(notes_ref, object)`.
    async fn put_note(
        &self,
        notes_ref: &str,
        object: &str,
        blob: &str,
        overwrite: bool,
    ) -> StorageResult<NoteWriteOutcome>;

    /// Remove the note for `(notes_ref, object)`, optionally compare-and-swapping
    /// against `expected_blob`. Returns `true` if a row was deleted.
    async fn remove_note_cas(
        &self,
        notes_ref: &str,
        object: &str,
        expected_blob: Option<&str>,
    ) -> StorageResult<bool>;

    /// Atomically apply a batch of CAS note mutations inside one transaction,
    /// rolling back completely on the first CAS conflict.
    async fn apply_notes_cas_batch(
        &self,
        notes_ref: &str,
        ops: &[NoteCasOp],
    ) -> StorageResult<NoteBatchOutcome>;

    // -------------------------------------------------------------------------
    // `revision_ordinal` & `revision_ordinal_meta` (`RB-04b` AC5)
    // -------------------------------------------------------------------------

    /// Read freshness metadata for `ref_name`.
    async fn get_revision_ordinal_meta(
        &self,
        ref_name: &str,
    ) -> StorageResult<Option<RevisionOrdinalMeta>>;

    /// List all `revision_ordinal_meta` records sorted by `ref_name ASC`.
    async fn list_revision_ordinal_metas(&self) -> StorageResult<Vec<RevisionOrdinalMeta>>;

    /// Atomically replace all `revision_ordinal` rows and `revision_ordinal_meta`
    /// for `meta.ref_name` in one transaction (supports SHA-1, SHA-256, and
    /// all-zero OIDs).
    async fn replace_revision_ordinals(
        &self,
        meta: &RevisionOrdinalMeta,
        entries: &[(i64, String)],
    ) -> StorageResult<()>;

    /// Atomically append a fast-forward suffix of OIDs when stored metadata
    /// matches `(expected_tip_oid, expected_replace_sig)`, or abort cleanly
    /// returning [`RevisionOrdinalAppendOutcome::Stale`].
    async fn append_revision_ordinals(
        &self,
        ref_name: &str,
        expected_tip_oid: &str,
        expected_replace_sig: &str,
        new_meta: &RevisionOrdinalMeta,
        suffix_oids: &[String],
    ) -> StorageResult<RevisionOrdinalAppendOutcome>;

    /// Forward lookup: `(ref_name, ordinal) -> oid`.
    async fn find_revision_oid_by_ordinal(
        &self,
        ref_name: &str,
        ordinal: i64,
    ) -> StorageResult<Option<String>>;

    /// Reverse lookup: `(ref_name, oid) -> ordinal`.
    async fn find_revision_ordinal_by_oid(
        &self,
        ref_name: &str,
        oid: &str,
    ) -> StorageResult<Option<i64>>;

    /// List `(ordinal, oid)` rows for `ref_name` ordered by `ordinal ASC`.
    async fn list_revision_ordinals(&self, ref_name: &str) -> StorageResult<Vec<(i64, String)>>;

    /// Count indexed `revision_ordinal` rows for `ref_name`.
    async fn count_revision_ordinals(&self, ref_name: &str) -> StorageResult<u64>;

    /// Atomically prune `revision_ordinal` and `revision_ordinal_meta` rows for
    /// all refs not present in `live_refs`, returning the number of pruned refs.
    async fn prune_revision_ordinals_except(&self, live_refs: &[String]) -> StorageResult<usize>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u64) -> String {
        format!("{n:040x}")
    }

    fn make_reflog(
        ref_name: &str,
        old_n: u64,
        new_n: u64,
        action: &str,
        message: &str,
        worktree_id: Option<&str>,
    ) -> ReflogWrite {
        ReflogWrite {
            ref_name: ref_name.to_string(),
            old_oid: oid(old_n),
            new_oid: oid(new_n),
            timestamp: 1_795_360_000 + i64::try_from(new_n).unwrap_or(0),
            committer_name: "Test Committer".to_string(),
            committer_email: "committer@example.com".to_string(),
            action: action.to_string(),
            message: message.to_string(),
            worktree_id: worktree_id.map(str::to_string),
        }
    }

    /// `RB-04` (AC1–AC5): Verify exact dual-backend equivalence between
    /// `SqliteGitStore` and `RedbGitStore` across:
    /// - AC1: `update_ref_cas` atomically compares, updates ref, and appends reflog.
    /// - AC2: `update_ref_cas` mismatch returns `CasConflict` with zero orphan reflog rows.
    /// - AC3: `update_ref_cas_with_bridge_publication` atomically binds HEAD move
    ///   and `agent_bridge_publication` in one transaction, rolling back cleanly
    ///   with zero orphan rows on CAS conflict, conflicting `operation_id`, or
    ///   injected pre-commit abort.
    /// - AC4: Main worktree (`None`) and linked worktree (`Some(id)`) local
    ///   `HEAD` and `HEAD` reflogs are strictly isolated by `worktree_id`.
    /// - AC5: Shared non-local refs (`LocalBranch`, `RemoteBranch`, `RemoteHead`,
    ///   `Tag`) are identically visible from main and linked worktrees on both backends.
    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn dual_backend_core_refs_reflog_and_bridge_cas_equivalence() {
        use std::sync::Arc;

        use tempfile::tempdir;

        let dir = tempdir().expect("create RB-04 dual-backend tempdir");
        let sqlite_path = dir.path().join("libra.db");
        let redb_path = dir.path().join("git.redb");

        let sqlite_store = SqliteGitStore::create(&sqlite_path)
            .await
            .expect("create SqliteGitStore");
        let redb_store = RedbGitStore::create(&redb_path).expect("create RedbGitStore");

        let backends: [Arc<dyn GitStore>; 2] =
            [Arc::new(sqlite_store.clone()), Arc::new(redb_store.clone())];
        assert_eq!(backends[0].backend(), StorageBackendKind::Sqlite);
        assert_eq!(backends[1].backend(), StorageBackendKind::Redb);

        // ---------------------------------------------------------------------
        // AC1 & AC2: Atomic CAS create/update + reflog append & CasConflict
        // zero-orphan-reflog guarantee.
        // ---------------------------------------------------------------------
        let main_branch = RefKey::LocalBranch {
            name: "main".to_string(),
        };
        let c1 = RefTarget::Direct(oid(1));
        let c2 = RefTarget::Direct(oid(2));
        let c3 = RefTarget::Direct(oid(3));
        let c99 = RefTarget::Direct(oid(99));

        // Initial CAS creation on absent branch + reflog.
        let create_log = make_reflog("refs/heads/main", 0, 1, "commit", "initial commit", None);
        for store in &backends {
            let outcome = store
                .update_ref_cas(
                    &main_branch,
                    None,
                    Some(&c1),
                    std::slice::from_ref(&create_log),
                )
                .await
                .expect("AC1: initial CAS create");
            assert_eq!(
                outcome,
                RefCasOutcome::Applied {
                    previous: None,
                    current: Some(c1.clone()),
                }
            );
        }

        // AC2: Mismatched expected old value -> CasConflict, ZERO orphan reflog row!
        let conflict_log = make_reflog("refs/heads/main", 99, 2, "commit", "stale update", None);
        for store in &backends {
            let outcome = store
                .update_ref_cas(
                    &main_branch,
                    Some(&c99),
                    Some(&c2),
                    std::slice::from_ref(&conflict_log),
                )
                .await
                .expect("AC2: CAS mismatch returns Ok(CasConflict)");
            assert_eq!(
                outcome,
                RefCasOutcome::CasConflict {
                    actual: Some(c1.clone()),
                }
            );
            assert_eq!(
                store.get_ref(&main_branch).await.expect("get main branch"),
                Some(c1.clone())
            );
            let logs = store
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("list reflog after CasConflict");
            assert_eq!(
                logs.len(),
                1,
                "AC2: CasConflict must produce zero orphan reflog rows"
            );
            assert_eq!(logs[0].new_oid, oid(1));
        }

        // AC1: Matching expected old value -> advances to c2 and appends reflog #2.
        let advance_log = make_reflog("refs/heads/main", 1, 2, "commit", "second commit", None);
        for store in &backends {
            let outcome = store
                .update_ref_cas(
                    &main_branch,
                    Some(&c1),
                    Some(&c2),
                    std::slice::from_ref(&advance_log),
                )
                .await
                .expect("AC1: matching CAS update");
            assert_eq!(
                outcome,
                RefCasOutcome::Applied {
                    previous: Some(c1.clone()),
                    current: Some(c2.clone()),
                }
            );
        }
        assert_eq!(
            backends[0]
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("sqlite reflog")
                .iter()
                .map(|e| (&e.old_oid, &e.new_oid, &e.message))
                .collect::<Vec<_>>(),
            backends[1]
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("redb reflog")
                .iter()
                .map(|e| (&e.old_oid, &e.new_oid, &e.message))
                .collect::<Vec<_>>(),
        );
        // Verify newest-first ordering (`HEAD@{0}` is index 0).
        for store in &backends {
            let logs = store
                .list_reflog("refs/heads/main", None, Some(1))
                .await
                .expect("limit 1 reflog");
            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0].new_oid, oid(2));
        }

        // ---------------------------------------------------------------------
        // AC4 & AC5: Main vs linked worktree local HEAD isolation + shared refs
        // visibility across both backends.
        // ---------------------------------------------------------------------
        let main_head = RefKey::LocalHead { worktree_id: None };
        let wt1_head = RefKey::LocalHead {
            worktree_id: Some("wt-alpha".to_string()),
        };
        let wt2_head = RefKey::LocalHead {
            worktree_id: Some("wt-beta".to_string()),
        };
        let feature_branch = RefKey::LocalBranch {
            name: "feature/worktree".to_string(),
        };
        let remote_branch = RefKey::RemoteBranch {
            remote: "origin".to_string(),
            name: "main".to_string(),
        };
        let remote_head = RefKey::RemoteHead {
            remote: "origin".to_string(),
        };
        let release_tag = RefKey::Tag {
            name: "v1.0.0".to_string(),
        };

        let main_sym = RefTarget::Symbolic("main".to_string());
        let wt1_sym = RefTarget::Symbolic("feature/worktree".to_string());
        let wt2_detached = RefTarget::Direct(oid(2));

        let main_head_log = make_reflog("HEAD", 0, 2, "checkout", "main checkout", None);
        let wt1_head_log = make_reflog(
            "HEAD",
            0,
            3,
            "checkout",
            "wt-alpha checkout",
            Some("wt-alpha"),
        );
        let wt2_head_log = make_reflog("HEAD", 0, 2, "checkout", "wt-beta detach", Some("wt-beta"));

        for store in &backends {
            store
                .update_ref_cas(&feature_branch, None, Some(&c3), &[])
                .await
                .expect("create feature branch");
            store
                .update_ref_cas(&remote_branch, None, Some(&c2), &[])
                .await
                .expect("create remote branch");
            store
                .update_ref_cas(&remote_head, None, Some(&main_sym), &[])
                .await
                .expect("create remote head");
            store
                .update_ref_cas(&release_tag, None, Some(&c1), &[])
                .await
                .expect("create tag");

            store
                .update_ref_cas(
                    &main_head,
                    None,
                    Some(&main_sym),
                    std::slice::from_ref(&main_head_log),
                )
                .await
                .expect("create main HEAD");
            store
                .update_ref_cas(
                    &wt1_head,
                    None,
                    Some(&wt1_sym),
                    std::slice::from_ref(&wt1_head_log),
                )
                .await
                .expect("create wt-alpha HEAD");
            store
                .update_ref_cas(
                    &wt2_head,
                    None,
                    Some(&wt2_detached),
                    std::slice::from_ref(&wt2_head_log),
                )
                .await
                .expect("create wt-beta detached HEAD");
        }

        // Duplicate symbolic branch checkout across two worktrees must fail closed on BOTH backends!
        for store in &backends {
            let dup_err = store
                .update_ref_cas(&wt2_head, Some(&wt2_detached), Some(&main_sym), &[])
                .await
                .expect_err("two worktrees must not hold symbolic HEAD to the same branch");
            assert_eq!(dup_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                store.get_ref(&wt2_head).await.expect("wt2 unchanged"),
                Some(wt2_detached.clone())
            );
        }

        // Verify AC4 (strict per-worktree HEAD & HEAD-reflog isolation) + AC5 (shared refs visible identically).
        for scope in [
            None,
            Some("wt-alpha"),
            Some("wt-beta"),
            Some("wt-uninitialized"),
        ] {
            let sq_refs = backends[0]
                .list_refs(scope)
                .await
                .expect("sqlite list_refs");
            let rb_refs = backends[1].list_refs(scope).await.expect("redb list_refs");
            assert_eq!(
                sq_refs, rb_refs,
                "AC4/AC5: list_refs({scope:?}) must match identically across SQLite and Redb"
            );

            // Count LocalHead rows returned: 1 for initialized scopes, 0 for wt-uninitialized.
            let local_heads: Vec<_> = sq_refs
                .iter()
                .filter(|rec| rec.key.is_worktree_local())
                .collect();
            if scope == Some("wt-uninitialized") {
                assert!(local_heads.is_empty());
            } else {
                assert_eq!(local_heads.len(), 1);
                assert_eq!(local_heads[0].key.worktree_id(), scope);
            }

            // All 5 shared refs (main, feature/worktree, origin/main, origin/HEAD, v1.0.0) are visible!
            let shared_keys: Vec<_> = sq_refs
                .iter()
                .filter(|rec| !rec.key.is_worktree_local())
                .map(|rec| rec.key.clone())
                .collect();
            assert_eq!(
                shared_keys,
                vec![
                    remote_head.clone(),
                    feature_branch.clone(),
                    main_branch.clone(),
                    remote_branch.clone(),
                    release_tag.clone(),
                ],
                "AC5: shared refs must be identically visible to {scope:?}"
            );

            // HEAD reflog strictly isolated per worktree.
            let sq_hlog = backends[0]
                .list_reflog("HEAD", scope, None)
                .await
                .expect("sqlite HEAD reflog");
            let rb_hlog = backends[1]
                .list_reflog("HEAD", scope, None)
                .await
                .expect("redb HEAD reflog");
            assert_eq!(
                sq_hlog
                    .iter()
                    .map(|e| (&e.message, &e.worktree_id))
                    .collect::<Vec<_>>(),
                rb_hlog
                    .iter()
                    .map(|e| (&e.message, &e.worktree_id))
                    .collect::<Vec<_>>()
            );
            match scope {
                None => {
                    assert_eq!(sq_hlog.len(), 1);
                    assert_eq!(sq_hlog[0].message, "main checkout");
                }
                Some("wt-alpha") => {
                    assert_eq!(sq_hlog.len(), 1);
                    assert_eq!(sq_hlog[0].message, "wt-alpha checkout");
                }
                Some("wt-beta") => {
                    assert_eq!(sq_hlog.len(), 1);
                    assert_eq!(sq_hlog[0].message, "wt-beta detach");
                }
                _ => assert!(sq_hlog.is_empty()),
            }
        }

        // `list_all_local_heads` returns all 3 worktree HEADs identically on both backends.
        assert_eq!(
            backends[0]
                .list_all_local_heads()
                .await
                .expect("sqlite all local heads"),
            backends[1]
                .list_all_local_heads()
                .await
                .expect("redb all local heads")
        );

        // ---------------------------------------------------------------------
        // AC3: `update_ref_cas_with_bridge_publication` atomic commit AND
        // zero-orphan rollback across both backends (plus injected redb abort).
        // ---------------------------------------------------------------------
        let c4 = RefTarget::Direct(oid(4));
        let c5 = RefTarget::Direct(oid(5));
        let c6 = RefTarget::Direct(oid(6));
        let pub_op1 = BridgePublicationRecord {
            operation_id: "op-bridge-001".to_string(),
            commit_oid: oid(4),
            phase: BridgePublicationPhase::Published,
        };
        let head_move_log = make_reflog("HEAD", 2, 4, "commit", "bridge commit 1", Some("wt-beta"));

        for store in &backends {
            // Prepare first, then promote to Published atomically with HEAD move.
            store
                .prepare_bridge_publication("op-bridge-001", &oid(4))
                .await
                .expect("prepare op-bridge-001");
            assert_eq!(
                store
                    .get_bridge_publication("op-bridge-001")
                    .await
                    .expect("get prepared"),
                Some(BridgePublicationRecord {
                    operation_id: "op-bridge-001".to_string(),
                    commit_oid: oid(4),
                    phase: BridgePublicationPhase::Prepared,
                })
            );

            // 1. CAS mismatch must NOT promote/insert publication OR append reflog!
            let pub_op_stale = BridgePublicationRecord {
                operation_id: "op-stale-never-committed".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Published,
            };
            let stale_log = make_reflog("HEAD", 99, 5, "commit", "stale bridge", Some("wt-beta"));
            let cas_res = store
                .update_ref_cas_with_bridge_publication(
                    &wt2_head,
                    Some(&c99),
                    Some(&c5),
                    &[stale_log],
                    &pub_op_stale,
                )
                .await
                .expect("AC3: CAS conflict on bridge publication");
            assert_eq!(
                cas_res,
                RefCasOutcome::CasConflict {
                    actual: Some(wt2_detached.clone()),
                }
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-stale-never-committed")
                    .await
                    .expect("stale publication absent"),
                None
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-beta"), None)
                    .await
                    .expect("wt-beta reflog unchanged")
                    .len(),
                1
            );

            // 2. Valid CAS + matching prepared op-bridge-001 -> commits atomically!
            let ok_res = store
                .update_ref_cas_with_bridge_publication(
                    &wt2_head,
                    Some(&wt2_detached),
                    Some(&c4),
                    std::slice::from_ref(&head_move_log),
                    &pub_op1,
                )
                .await
                .expect("AC3: atomic bridge publication commit");
            assert_eq!(
                ok_res,
                RefCasOutcome::Applied {
                    previous: Some(wt2_detached.clone()),
                    current: Some(c4.clone()),
                }
            );
            assert_eq!(
                store.get_ref(&wt2_head).await.expect("wt2 moved to c4"),
                Some(c4.clone())
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-bridge-001")
                    .await
                    .expect("op-bridge-001 published"),
                Some(pub_op1.clone())
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-beta"), None)
                    .await
                    .expect("wt-beta reflog has 2 rows")
                    .len(),
                2
            );

            // 3. Reusing already-bound `op-bridge-001` for a DIFFERENT commit_oid (c5)
            //    must roll back the ENTIRE transaction: HEAD stays at c4, reflog stays 2,
            //    publication stays bound to oid(4)!
            let conflicting_pub = BridgePublicationRecord {
                operation_id: "op-bridge-001".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Published,
            };
            let rollback_log =
                make_reflog("HEAD", 4, 5, "commit", "must rollback", Some("wt-beta"));
            let err = store
                .update_ref_cas_with_bridge_publication(
                    &wt2_head,
                    Some(&c4),
                    Some(&c5),
                    &[rollback_log],
                    &conflicting_pub,
                )
                .await
                .expect_err("AC3: conflicting bridge operation_id must abort entire transaction");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                store
                    .get_ref(&wt2_head)
                    .await
                    .expect("HEAD unchanged after rollback"),
                Some(c4.clone())
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-beta"), None)
                    .await
                    .expect("reflog unchanged after rollback")
                    .len(),
                2
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-bridge-001")
                    .await
                    .expect("publication unchanged after rollback"),
                Some(pub_op1.clone())
            );

            // 4. Bridge publication binding rejections (P1 #1):
            //    (a) commit_oid mismatch between ref target and publication;
            //    (b) non-`Published` phase (`Prepared`);
            //    (c) non-HEAD / unchecked-out ref (`Tag`, `RemoteBranch`, unchecked-out `LocalBranch`);
            //    (d) unchanged/deleted ref (`expected == new_target`, `new_target == None`).
            let mismatched_oid_pub = BridgePublicationRecord {
                operation_id: "op-mismatch-oid".to_string(),
                commit_oid: oid(7),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &wt2_head,
                        Some(&c4),
                        Some(&c5),
                        &[],
                        &mismatched_oid_pub,
                    )
                    .await
                    .is_err(),
                "mismatched publication commit_oid must be rejected"
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-mismatch-oid")
                    .await
                    .expect("get op-mismatch-oid"),
                None
            );

            let wrong_phase_pub = BridgePublicationRecord {
                operation_id: "op-wrong-phase".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Prepared,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &wt2_head,
                        Some(&c4),
                        Some(&c5),
                        &[],
                        &wrong_phase_pub,
                    )
                    .await
                    .is_err(),
                "phase=Prepared must be rejected on publish"
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-wrong-phase")
                    .await
                    .expect("get op-wrong-phase"),
                None
            );

            let unrelated_tag_pub = BridgePublicationRecord {
                operation_id: "op-unrelated-tag".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &release_tag,
                        Some(&c1),
                        Some(&c5),
                        &[],
                        &unrelated_tag_pub,
                    )
                    .await
                    .is_err(),
                "binding a bridge publication to a Tag must be rejected"
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-unrelated-tag")
                    .await
                    .expect("get op-unrelated-tag"),
                None
            );

            // Unchecked-out LocalBranch (no HEAD points to `unchecked-out`) must be rejected.
            let unchecked_branch = RefKey::LocalBranch {
                name: "unchecked-out".to_string(),
            };
            let unchecked_pub = BridgePublicationRecord {
                operation_id: "op-unchecked-branch".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &unchecked_branch,
                        None,
                        Some(&c5),
                        &[],
                        &unchecked_pub,
                    )
                    .await
                    .is_err(),
                "binding a bridge publication to an unchecked-out branch must be rejected"
            );
            assert_eq!(
                store
                    .get_ref(&unchecked_branch)
                    .await
                    .expect("unchecked branch remains absent"),
                None
            );

            // No-op transition (`expected == new_target`) must be rejected.
            let noop_pub = BridgePublicationRecord {
                operation_id: "op-noop".to_string(),
                commit_oid: oid(4),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &wt2_head,
                        Some(&c4),
                        Some(&c4),
                        &[],
                        &noop_pub,
                    )
                    .await
                    .is_err(),
                "no-op ref update must be rejected for bridge publication"
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-noop")
                    .await
                    .expect("get op-noop"),
                None
            );

            // 5. Symbolic HEAD branch commit + bridge publication succeeds atomically
            //    when `main` IS checked out by main worktree's `LocalHead`!
            let c8 = RefTarget::Direct(oid(8));
            let sym_pub = BridgePublicationRecord {
                operation_id: "op-bridge-symbolic-main".to_string(),
                commit_oid: oid(8),
                phase: BridgePublicationPhase::Published,
            };
            let main_branch_log =
                make_reflog("refs/heads/main", 2, 8, "commit", "sym bridge branch", None);
            let main_sym_head_log = make_reflog("HEAD", 2, 8, "commit", "sym bridge head", None);
            let sym_res = store
                .update_ref_cas_with_bridge_publication(
                    &main_branch,
                    Some(&c2),
                    Some(&c8),
                    &[main_branch_log, main_sym_head_log],
                    &sym_pub,
                )
                .await
                .expect("symbolic HEAD checked-out branch bridge publication must succeed");
            assert_eq!(
                sym_res,
                RefCasOutcome::Applied {
                    previous: Some(c2.clone()),
                    current: Some(c8.clone()),
                }
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-bridge-symbolic-main")
                    .await
                    .expect("get op-bridge-symbolic-main"),
                Some(sym_pub)
            );

            // Detaching symbolic HEAD (`main` -> oid(8)) directly to the SAME resolved
            // commit `Direct(oid(8))` does NOT move HEAD to a new commit and MUST be
            // rejected for bridge publication on both backends!
            let same_resolved_pub = BridgePublicationRecord {
                operation_id: "op-same-resolved-oid".to_string(),
                commit_oid: oid(8),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &main_head,
                        Some(&main_sym),
                        Some(&c8),
                        &[],
                        &same_resolved_pub,
                    )
                    .await
                    .is_err(),
                "detaching symbolic HEAD onto the exact same resolved commit OID must be rejected for bridge publication"
            );
            assert_eq!(
                store
                    .get_ref(&main_head)
                    .await
                    .expect("main HEAD remains symbolic main"),
                Some(main_sym.clone())
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-same-resolved-oid")
                    .await
                    .expect("get op-same-resolved-oid"),
                None
            );

            // 6. Cross-worktree HEAD reflog boundary rejection (P1 #2):
            //    - Main LocalHead update with linked worktree HEAD reflog -> rejected!
            //    - Linked LocalHead update with main (None) or other linked worktree HEAD reflog -> rejected!
            //    - Branch update with HEAD reflog for a worktree NOT checked out on that branch -> rejected!
            let cross_main_to_linked = make_reflog(
                "HEAD",
                8,
                6,
                "checkout",
                "illegal main->wt-beta log",
                Some("wt-beta"),
            );
            assert!(
                store
                    .update_ref_cas(
                        &main_head,
                        Some(&main_sym),
                        Some(&c6),
                        &[cross_main_to_linked],
                    )
                    .await
                    .is_err(),
                "main LocalHead update must reject linked-worktree HEAD reflog"
            );
            let cross_linked_to_main =
                make_reflog("HEAD", 4, 6, "checkout", "illegal wt-beta->main log", None);
            assert!(
                store
                    .update_ref_cas(&wt2_head, Some(&c4), Some(&c6), &[cross_linked_to_main],)
                    .await
                    .is_err(),
                "linked LocalHead update must reject main-worktree HEAD reflog"
            );
            let cross_linked_to_other = make_reflog(
                "HEAD",
                4,
                6,
                "checkout",
                "illegal wt-beta->wt-alpha log",
                Some("wt-alpha"),
            );
            assert!(
                store
                    .update_ref_cas(&wt2_head, Some(&c4), Some(&c6), &[cross_linked_to_other],)
                    .await
                    .is_err(),
                "linked LocalHead update must reject another linked-worktree HEAD reflog"
            );
            let wrong_wt_branch_head_log = make_reflog(
                "HEAD",
                8,
                6,
                "commit",
                "wt-alpha does not have main checked out",
                Some("wt-alpha"),
            );
            assert!(
                store
                    .update_ref_cas(
                        &main_branch,
                        Some(&c8),
                        Some(&c6),
                        &[wrong_wt_branch_head_log],
                    )
                    .await
                    .is_err(),
                "LocalBranch update must reject HEAD reflog for worktree not checked out on that branch"
            );
            // Confirm none of the rejected cross-worktree attempts mutated refs or reflogs.
            assert_eq!(
                store.get_ref(&main_head).await.expect("main HEAD intact"),
                Some(main_sym.clone())
            );
            assert_eq!(
                store.get_ref(&wt2_head).await.expect("wt-beta HEAD intact"),
                Some(c4.clone())
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", None, None)
                    .await
                    .expect("main HEAD reflog intact")
                    .len(),
                2
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-alpha"), None)
                    .await
                    .expect("wt-alpha HEAD reflog intact")
                    .len(),
                1
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-beta"), None)
                    .await
                    .expect("wt-beta HEAD reflog intact")
                    .len(),
                2
            );
        }

        // 4. Explicit injected pre-commit rollback inside `RedbGitStore`:
        //    proves staged HEAD move + reflog + new bridge publication row are
        //    all atomically discarded when `WriteTransaction` aborts before commit.
        let aborted_pub = BridgePublicationRecord {
            operation_id: "op-bridge-aborted".to_string(),
            commit_oid: oid(6),
            phase: BridgePublicationPhase::Published,
        };
        let aborted_log = make_reflog("HEAD", 4, 6, "commit", "aborted tx", Some("wt-beta"));
        let abort_err = redb_store
            .update_ref_cas_with_bridge_publication_abort_before_commit_for_test(
                &wt2_head,
                Some(&c4),
                Some(&c6),
                &[aborted_log],
                &aborted_pub,
            )
            .await
            .expect_err("injected pre-commit abort must return Err");
        assert_eq!(abort_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            redb_store
                .get_ref(&wt2_head)
                .await
                .expect("redb HEAD untouched after pre-commit abort"),
            Some(c4.clone())
        );
        assert_eq!(
            redb_store
                .list_reflog("HEAD", Some("wt-beta"), None)
                .await
                .expect("redb reflog untouched after pre-commit abort")
                .len(),
            2
        );
        assert_eq!(
            redb_store
                .get_bridge_publication("op-bridge-aborted")
                .await
                .expect("redb publication absent after pre-commit abort"),
            None
        );

        // Final bridge publications list matches across both backends.
        assert_eq!(
            backends[0]
                .list_bridge_publications()
                .await
                .expect("sqlite publications"),
            backends[1]
                .list_bridge_publications()
                .await
                .expect("redb publications")
        );

        // Also verify strict read-only handles on both backends see identical state and reject mutations.
        let sq_ro = SqliteGitStore::open_read_only(&sqlite_path)
            .await
            .expect("open SqliteGitStore read-only");
        let rb_ro = RedbGitStore::open_read_only(&redb_path).expect("open RedbGitStore read-only");
        assert_eq!(
            sq_ro.list_refs(None).await.expect("sqlite ro list_refs"),
            rb_ro.list_refs(None).await.expect("redb ro list_refs")
        );
        assert!(
            sq_ro
                .update_ref_cas(&main_branch, Some(&c2), Some(&c3), &[])
                .await
                .is_err()
        );
        assert!(
            rb_ro
                .update_ref_cas(&main_branch, Some(&c2), Some(&c3), &[])
                .await
                .is_err()
        );
    }

    /// `RB-04` (AC6): Real 12-process concurrent stress test against a single
    /// `git.redb` file: 4 independent writer OS processes looping
    /// `update_ref_cas` (20 successful linearizable CAS increments each = 80
    /// total) + 8 independent `open_read_only` reader OS processes continuously
    /// calling `list_refs`/`get_ref`/`list_reflog`, proving strict linearizability,
    /// MVCC non-blocking reads, zero lock errors, and zero orphan reflog rows.
    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn redb_multiprocess_cas_linearizability_and_mvcc_reads() {
        use std::{
            collections::BTreeSet,
            env, fs,
            path::PathBuf,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };

        use tempfile::tempdir;

        const WRITERS: usize = 4;
        const READERS: usize = 8;
        const MIN_INCREMENTS_PER_WRITER: u64 = 20;
        const MAX_INCREMENTS_PER_WRITER: u64 = 2_000;
        const MIN_DISTINCT_LIVE_VALUES: usize = 3;
        const MIN_OVERLAP_READS: u64 = 20;

        fn parse_counter_oid(raw: &str) -> u64 {
            u64::from_str_radix(raw, 16).expect("counter OID must be valid hex u64")
        }

        // Child worker entrypoint (writer or reader).
        if let Ok(role) = env::var("LIBRA_RB04_CHILD_ROLE") {
            let db_path = PathBuf::from(env::var("LIBRA_RB04_DB_PATH").expect("RB04 DB path"));
            let sync_dir = PathBuf::from(env::var("LIBRA_RB04_SYNC_DIR").expect("RB04 sync dir"));
            let worker_id: usize = env::var("LIBRA_RB04_WORKER_ID")
                .expect("RB04 worker id")
                .parse()
                .expect("parse worker id");

            let go_flag = sync_dir.join("go.flag");
            let readers_overlap_done_flag = sync_dir.join("readers-overlap-done.flag");
            let done_flag = sync_dir.join("done.flag");
            let counter_key = RefKey::LocalBranch {
                name: "counter".to_string(),
            };

            match role.as_str() {
                "writer" => {
                    let store = RedbGitStore::open_read_write(&db_path)
                        .expect("child writer open RedbGitStore")
                        .with_write_timeout(Duration::from_secs(30));
                    assert!(
                        store
                            .get_ref(&counter_key)
                            .await
                            .expect("writer warmup get_ref")
                            .is_some()
                    );
                    fs::write(
                        sync_dir.join(format!("ready-writer-{worker_id}.flag")),
                        b"ready",
                    )
                    .expect("signal writer ready");

                    let barrier_deadline = Instant::now() + Duration::from_secs(30);
                    while !go_flag.exists() {
                        assert!(
                            Instant::now() < barrier_deadline,
                            "writer {worker_id} timed out waiting for go.flag"
                        );
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }

                    let mut applied = 0u64;
                    let mut conflicts = 0u64;
                    // Continuously commit without pausing until BOTH:
                    // 1. this writer has applied >= MIN_INCREMENTS_PER_WRITER CAS updates, AND
                    // 2. all 8 concurrent readers have observed >= 3 distinct live advancing commits.
                    while applied < MIN_INCREMENTS_PER_WRITER
                        || (!readers_overlap_done_flag.exists()
                            && applied < MAX_INCREMENTS_PER_WRITER)
                    {
                        let current = store
                            .get_ref(&counter_key)
                            .await
                            .expect("writer get_ref must not hit lock error")
                            .expect("counter branch must exist");
                        let cur_n = match &current {
                            RefTarget::Direct(hex) => parse_counter_oid(hex),
                            RefTarget::Symbolic(_) => panic!("counter branch must be direct"),
                        };
                        let next_n = cur_n + 1;
                        let next_target = RefTarget::Direct(oid(next_n));
                        let log = make_reflog(
                            "refs/heads/counter",
                            cur_n,
                            next_n,
                            "update-ref",
                            &format!("writer-{worker_id} step {cur_n}->{next_n}"),
                            None,
                        );
                        match store
                            .update_ref_cas(
                                &counter_key,
                                Some(&current),
                                Some(&next_target),
                                &[log],
                            )
                            .await
                            .expect("writer update_ref_cas must not return lock error")
                        {
                            RefCasOutcome::Applied { .. } => {
                                applied += 1;
                            }
                            RefCasOutcome::CasConflict { .. } => {
                                conflicts += 1;
                            }
                        }
                    }
                    assert!(
                        readers_overlap_done_flag.exists(),
                        "writer {worker_id} hit MAX_INCREMENTS_PER_WRITER before readers finished live overlap observation"
                    );
                    println!(
                        "RB04_WRITER_OK id={worker_id} applied={applied} conflicts={conflicts}"
                    );
                }
                "reader" => {
                    let store = RedbGitStore::open_read_only(&db_path)
                        .expect("child reader open RedbGitStore read-only");
                    assert!(
                        store
                            .get_ref(&counter_key)
                            .await
                            .expect("reader warmup get_ref")
                            .is_some()
                    );
                    fs::write(
                        sync_dir.join(format!("ready-reader-{worker_id}.flag")),
                        b"ready",
                    )
                    .expect("signal reader ready");

                    let barrier_deadline = Instant::now() + Duration::from_secs(30);
                    while !go_flag.exists() {
                        assert!(
                            Instant::now() < barrier_deadline,
                            "reader {worker_id} timed out waiting for go.flag"
                        );
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }

                    let mut last_seen = 0u64;
                    let mut reads_during_writes = 0u64;
                    let mut distinct_live_values = BTreeSet::new();
                    let mut overlap_signaled = false;
                    let deadline = Instant::now() + Duration::from_secs(60);
                    loop {
                        assert!(
                            Instant::now() < deadline,
                            "reader {worker_id} timed out waiting for done.flag (reads_during_writes={reads_during_writes}, distinct_live={}, last_seen={last_seen})",
                            distinct_live_values.len()
                        );
                        let done_snapshot = fs::read_to_string(&done_flag).ok();

                        let refs = store
                            .list_refs(None)
                            .await
                            .expect("reader list_refs must not fail under write contention");
                        let counter_from_list = refs
                            .iter()
                            .find(|r| r.key == counter_key)
                            .expect("counter ref must always be visible in list_refs");
                        let list_n = match &counter_from_list.target {
                            RefTarget::Direct(hex) => parse_counter_oid(hex),
                            RefTarget::Symbolic(_) => panic!("counter must be direct"),
                        };
                        assert!(
                            list_n >= last_seen,
                            "non-monotonic counter observed in list_refs: prev={last_seen}, now={list_n}"
                        );
                        last_seen = list_n;

                        let got = store
                            .get_ref(&counter_key)
                            .await
                            .expect("reader get_ref must not fail under write contention")
                            .expect("counter ref must exist");
                        let get_n = match &got {
                            RefTarget::Direct(hex) => parse_counter_oid(hex),
                            RefTarget::Symbolic(_) => panic!("counter must be direct"),
                        };
                        assert!(
                            get_n >= last_seen,
                            "non-monotonic counter observed in get_ref: prev={last_seen}, now={get_n}"
                        );
                        last_seen = get_n;

                        let logs = store
                            .list_reflog("refs/heads/counter", None, Some(1))
                            .await
                            .expect("reader list_reflog must not fail");
                        if let Some(top) = logs.first() {
                            let top_n = parse_counter_oid(&top.new_oid);
                            assert!(
                                top_n >= last_seen,
                                "reflog head regressed: last_seen={last_seen}, top={top_n}"
                            );
                            last_seen = top_n;
                        }

                        if let Some(expected_final_str) = done_snapshot {
                            let expected_final: u64 = expected_final_str
                                .trim()
                                .parse()
                                .expect("parse expected final total from done.flag");
                            assert_eq!(
                                last_seen, expected_final,
                                "reader {worker_id} must observe exact converged counter {expected_final} after done.flag"
                            );
                            break;
                        }

                        // Count ONLY reads performed after writers have begun committing (`last_seen > 0`).
                        if last_seen > 0 {
                            reads_during_writes += 1;
                            distinct_live_values.insert(last_seen);
                            if !overlap_signaled
                                && reads_during_writes >= MIN_OVERLAP_READS
                                && distinct_live_values.len() >= MIN_DISTINCT_LIVE_VALUES
                            {
                                overlap_signaled = true;
                                fs::write(
                                    sync_dir.join(format!("reader-overlap-{worker_id}.flag")),
                                    b"overlap",
                                )
                                .expect("signal reader observed live advancing writes");
                            }
                        }
                    }
                    assert!(
                        overlap_signaled
                            && reads_during_writes >= MIN_OVERLAP_READS
                            && distinct_live_values.len() >= MIN_DISTINCT_LIVE_VALUES,
                        "reader {worker_id} must observe >= {MIN_DISTINCT_LIVE_VALUES} live advancing values and >= {MIN_OVERLAP_READS} reads during active writes"
                    );
                    println!(
                        "RB04_READER_OK id={worker_id} reads_during_writes={reads_during_writes} distinct_live={} last_seen={last_seen}",
                        distinct_live_values.len()
                    );
                }
                other => panic!("unexpected RB04 child role: {other}"),
            }
            return;
        }

        // Parent orchestrator.
        let dir = tempdir().expect("create RB-04 multiprocess tempdir");
        let db_path = dir.path().join("git.redb");
        let sync_dir = dir.path().join("sync");
        fs::create_dir_all(&sync_dir).expect("create sync dir");
        let go_flag = sync_dir.join("go.flag");
        let readers_overlap_done_flag = sync_dir.join("readers-overlap-done.flag");
        let done_flag = sync_dir.join("done.flag");

        {
            let store = RedbGitStore::create(&db_path).expect("create shared git.redb");
            store
                .update_ref_cas(
                    &RefKey::LocalHead { worktree_id: None },
                    None,
                    Some(&RefTarget::Symbolic("counter".to_string())),
                    &[],
                )
                .await
                .expect("seed main HEAD");
            store
                .update_ref_cas(
                    &RefKey::LocalHead {
                        worktree_id: Some("wt-stress".to_string()),
                    },
                    None,
                    Some(&RefTarget::Direct(oid(0))),
                    &[],
                )
                .await
                .expect("seed linked worktree HEAD");
            store
                .update_ref_cas(
                    &RefKey::LocalBranch {
                        name: "counter".to_string(),
                    },
                    None,
                    Some(&RefTarget::Direct(oid(0))),
                    &[],
                )
                .await
                .expect("seed counter branch at 0");
        }

        let exe = env::current_exe().expect("locate current test executable");
        let spawn_worker = |role: &str, id: usize| {
            Command::new(&exe)
                .arg("--exact")
                .arg("storage::git_store::tests::redb_multiprocess_cas_linearizability_and_mvcc_reads")
                .arg("--nocapture")
                .env("LIBRA_RB04_CHILD_ROLE", role)
                .env("LIBRA_RB04_DB_PATH", &db_path)
                .env("LIBRA_RB04_SYNC_DIR", &sync_dir)
                .env("LIBRA_RB04_WORKER_ID", id.to_string())
                .env("LIBRA_TEST", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn RB-04 worker child")
        };

        let mut writers = Vec::with_capacity(WRITERS);
        for id in 0..WRITERS {
            writers.push(spawn_worker("writer", id));
        }
        let mut readers = Vec::with_capacity(READERS);
        for id in 0..READERS {
            readers.push(spawn_worker("reader", id));
        }

        // Phase 1: Wait until ALL 12 processes (4 writers + 8 readers) have opened
        // `git.redb` and signaled readiness before releasing `go.flag`.
        let ready_deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let writers_ready =
                (0..WRITERS).all(|id| sync_dir.join(format!("ready-writer-{id}.flag")).exists());
            let readers_ready =
                (0..READERS).all(|id| sync_dir.join(format!("ready-reader-{id}.flag")).exists());
            if writers_ready && readers_ready {
                break;
            }
            assert!(
                Instant::now() < ready_deadline,
                "timed out waiting for all 12 RB-04 workers to open git.redb"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        fs::write(&go_flag, b"go").expect("write go flag");

        // Phase 2: While all 4 writers are actively committing without pausing,
        // wait until ALL 8 readers have observed >= 3 distinct live advancing
        // values and >= 20 active reads, then let writers finish cleanly.
        let overlap_deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let readers_overlapped =
                (0..READERS).all(|id| sync_dir.join(format!("reader-overlap-{id}.flag")).exists());
            if readers_overlapped {
                break;
            }
            assert!(
                Instant::now() < overlap_deadline,
                "timed out waiting for all 8 readers to observe live advancing writes"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        fs::write(&readers_overlap_done_flag, b"overlap-done")
            .expect("signal writers that reader overlap is verified");

        let mut total_applied = 0u64;
        for (id, child) in writers.into_iter().enumerate() {
            let output = child.wait_with_output().expect("wait for writer child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let prefix = format!("RB04_WRITER_OK id={id} applied=");
            assert!(
                output.status.success()
                    && stdout.contains(&prefix)
                    && stdout.contains("1 passed; 0 failed"),
                "writer {id} failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
            let applied: u64 = stdout
                .lines()
                .find_map(|line| line.strip_prefix(&prefix))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|num| num.parse().ok())
                .expect("parse writer applied count");
            assert!(
                applied >= MIN_INCREMENTS_PER_WRITER,
                "writer {id} must apply >= {MIN_INCREMENTS_PER_WRITER} increments, got {applied}"
            );
            total_applied += applied;
        }
        assert!(
            total_applied >= (WRITERS as u64) * MIN_INCREMENTS_PER_WRITER,
            "total_applied must be >= {}",
            (WRITERS as u64) * MIN_INCREMENTS_PER_WRITER
        );

        fs::write(&done_flag, total_applied.to_string()).expect("write done flag with total");

        for (id, child) in readers.into_iter().enumerate() {
            let output = child.wait_with_output().expect("wait for reader child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success()
                    && stdout.contains(&format!("RB04_READER_OK id={id}"))
                    && stdout.contains("1 passed; 0 failed"),
                "reader {id} failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
        }

        // Verify strict linearizability & zero orphan reflog rows!
        let verify = RedbGitStore::open_read_only(&db_path).expect("open final git.redb RO");
        let final_val = verify
            .get_ref(&RefKey::LocalBranch {
                name: "counter".to_string(),
            })
            .await
            .expect("read final counter")
            .expect("counter exists");
        assert_eq!(
            final_val,
            RefTarget::Direct(oid(total_applied)),
            "AC6: all {total_applied} CAS increments across {WRITERS} processes must linearize with zero lost updates"
        );

        let reflog = verify
            .list_reflog("refs/heads/counter", None, None)
            .await
            .expect("read final reflog");
        assert_eq!(
            reflog.len() as u64,
            total_applied,
            "AC6: reflog must contain exactly {total_applied} entries (zero orphan rows from CAS conflicts)"
        );
        // Newest-first (`total_applied` down to 1): verify unbroken chain `0->1->2->...->total_applied`.
        for (idx, entry) in reflog.iter().enumerate() {
            let expected_new = total_applied - (idx as u64);
            let expected_old = expected_new - 1;
            assert_eq!(
                entry.old_oid,
                oid(expected_old),
                "broken linearizable chain at reflog[{idx}] old_oid"
            );
            assert_eq!(
                entry.new_oid,
                oid(expected_new),
                "broken linearizable chain at reflog[{idx}] new_oid"
            );
        }
    }

    /// `RB-04` single-feature SQLite regression smoke test (`--no-default-features
    /// --features storage-sqlite`).
    #[cfg(feature = "storage-sqlite")]
    #[tokio::test]
    async fn sqlite_core_refs_reflog_smoke() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create sqlite smoke tempdir");
        let db_path = dir.path().join("libra.db");
        let store = SqliteGitStore::create(&db_path)
            .await
            .expect("create SqliteGitStore");
        assert_eq!(store.backend(), StorageBackendKind::Sqlite);

        let main_branch = RefKey::LocalBranch {
            name: "main".to_string(),
        };
        let main_head = RefKey::LocalHead { worktree_id: None };
        let linked_head = RefKey::LocalHead {
            worktree_id: Some("wt-1".to_string()),
        };

        let c1 = RefTarget::Direct(oid(1));
        let c2 = RefTarget::Direct(oid(2));

        let applied = store
            .update_ref_cas(
                &main_branch,
                None,
                Some(&c1),
                &[make_reflog("refs/heads/main", 0, 1, "commit", "init", None)],
            )
            .await
            .expect("create main branch");
        assert_eq!(
            applied,
            RefCasOutcome::Applied {
                previous: None,
                current: Some(c1.clone()),
            }
        );

        let conflict = store
            .update_ref_cas(
                &main_branch,
                Some(&c2),
                Some(&RefTarget::Direct(oid(3))),
                &[make_reflog(
                    "refs/heads/main",
                    2,
                    3,
                    "commit",
                    "conflict",
                    None,
                )],
            )
            .await
            .expect("cas conflict");
        assert_eq!(
            conflict,
            RefCasOutcome::CasConflict {
                actual: Some(c1.clone()),
            }
        );
        assert_eq!(
            store
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("reflog count")
                .len(),
            1
        );

        // Worktree-isolated HEAD + atomic bridge publication binding.
        store
            .update_ref_cas(
                &main_head,
                None,
                Some(&RefTarget::Symbolic("main".to_string())),
                &[make_reflog("HEAD", 0, 1, "checkout", "main", None)],
            )
            .await
            .expect("set main HEAD");
        let pub_rec = BridgePublicationRecord {
            operation_id: "sqlite-smoke-op".to_string(),
            commit_oid: oid(2),
            phase: BridgePublicationPhase::Published,
        };
        let pub_outcome = store
            .update_ref_cas_with_bridge_publication(
                &linked_head,
                None,
                Some(&c2),
                &[make_reflog(
                    "HEAD",
                    0,
                    2,
                    "commit",
                    "wt-1 bridge",
                    Some("wt-1"),
                )],
                &pub_rec,
            )
            .await
            .expect("atomic bridge publication");
        assert_eq!(
            pub_outcome,
            RefCasOutcome::Applied {
                previous: None,
                current: Some(c2.clone()),
            }
        );
        assert_eq!(
            store
                .get_bridge_publication("sqlite-smoke-op")
                .await
                .expect("get publication"),
            Some(pub_rec)
        );
        assert_eq!(
            store
                .list_refs(None)
                .await
                .expect("main refs")
                .iter()
                .filter(|r| r.key.is_worktree_local())
                .count(),
            1
        );
        assert_eq!(
            store
                .list_refs(Some("wt-1"))
                .await
                .expect("wt-1 refs")
                .iter()
                .filter(|r| r.key == main_branch)
                .count(),
            1
        );
    }

    #[cfg(feature = "storage-sqlite")]
    fn sha1_oid(n: u64) -> String {
        format!("{n:040x}")
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    fn sha256_oid(n: u64) -> String {
        format!("{n:064x}")
    }

    /// `RB-04b` (AC1–AC5): Verify exact dual-backend equivalence between
    /// `SqliteGitStore` and `RedbGitStore` across:
    /// - AC1: `upsert_object_index_batch` idempotently upserts >= 1,000 rows in
    ///   one write transaction (plus `RedbGitStore` same-tx `OBJECT_BY_PACK` index).
    /// - AC2: `object_obliteration` fence blocks read path for tombstoned OIDs
    ///   (`obliterating` and `obliterated`) across SHA-1 & SHA-256 while preserving
    ///   the historical `object_index` address row.
    /// - AC3: `object_obliteration` fence blocks write path atomically with zero
    ///   partial writes for tombstoned OIDs.
    /// - AC4: `notes` CRUD + CAS batch parity across SQLite and Redb.
    /// - AC5: `revision_ordinal` & `revision_ordinal_meta` parity across SHA-1,
    ///   SHA-256, and all-zero OIDs.
    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dual_backend_object_index_obliteration_notes_and_ordinals() {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };

        use tempfile::tempdir;

        let dir = tempdir().expect("create RB-04b dual-backend tempdir");
        let sqlite_path = dir.path().join("libra.db");
        let redb_path = dir.path().join("git.redb");
        let sqlite_store = SqliteGitStore::create(&sqlite_path)
            .await
            .expect("create SqliteGitStore");
        let redb_store = RedbGitStore::create(&redb_path).expect("create RedbGitStore");

        let backends: [Arc<dyn GitStore>; 2] =
            [Arc::new(sqlite_store.clone()), Arc::new(redb_store.clone())];

        // ---------------------------------------------------------------------
        // AC1: Single-transaction batch upsert of >= 1,000 object_index records
        // with idempotent replay, semantic `agent_*` promotion, non-demotion,
        // and `mark_object_index_synced` parity.
        // ---------------------------------------------------------------------
        const BATCH_LEN: usize = 1_024;
        let repo_a = "repo-alpha";
        let repo_b = "repo-beta";
        let mut batch = Vec::with_capacity(BATCH_LEN);
        for idx in 1..=BATCH_LEN {
            let n = u64::try_from(idx).expect("batch idx fits u64");
            batch.push(ObjectIndexRecord {
                repo_id: repo_a.to_string(),
                o_id: sha1_oid(n),
                o_type: if idx % 4 == 0 {
                    "commit".to_string()
                } else {
                    "blob".to_string()
                },
                o_size: i64::try_from(idx * 16).expect("size fits i64"),
                created_at: 1_795_370_000 + i64::try_from(idx).expect("ts fits i64"),
                is_synced: false,
            });
        }
        // Include an extra repo_b row to verify multi-repo isolation.
        let repo_b_row = ObjectIndexRecord {
            repo_id: repo_b.to_string(),
            o_id: sha256_oid(77),
            o_type: "tree".to_string(),
            o_size: 4096,
            created_at: 1_795_370_999,
            is_synced: true,
        };

        for store in &backends {
            store
                .upsert_object_index_batch(&batch)
                .await
                .expect("AC1: initial 1,024-row upsert");
            // Idempotent re-upsert must not duplicate or corrupt rows.
            store
                .upsert_object_index_batch(&batch)
                .await
                .expect("AC1: idempotent replay upsert");
            store
                .upsert_object_index_batch(std::slice::from_ref(&repo_b_row))
                .await
                .expect("upsert repo_b row");
        }

        let sq_all = backends[0]
            .list_object_index_by_repo(repo_a, None)
            .await
            .expect("sqlite list repo_a");
        let rb_all = backends[1]
            .list_object_index_by_repo(repo_a, None)
            .await
            .expect("redb list repo_a");
        assert_eq!(sq_all.len(), BATCH_LEN);
        assert_eq!(
            sq_all, rb_all,
            "AC1: 1,024 object_index rows must match identically"
        );

        // Mark first 10 rows synced, then test semantic `agent_*` promotion vs generic non-demotion.
        let synced_ids: Vec<String> = (1u64..=10).map(sha1_oid).collect();
        let promoted_oid = sha1_oid(1);
        let kept_synced_oid = sha1_oid(2);
        for store in &backends {
            let count = store
                .mark_object_index_synced(repo_a, &synced_ids)
                .await
                .expect("mark synced");
            assert_eq!(count, 10);
            // Re-marking already synced rows returns 0.
            assert_eq!(
                store
                    .mark_object_index_synced(repo_a, &synced_ids)
                    .await
                    .expect("idempotent mark synced"),
                0
            );
            // Promote oid(1) from generic `blob` -> `agent_session` (reopens is_synced = false)
            // and replay generic `blob` on oid(2) (keeps is_synced = true and original size).
            let promo_batch = vec![
                ObjectIndexRecord {
                    repo_id: repo_a.to_string(),
                    o_id: promoted_oid.clone(),
                    o_type: "agent_session".to_string(),
                    o_size: 999,
                    created_at: 1_795_399_999,
                    is_synced: true,
                },
                ObjectIndexRecord {
                    repo_id: repo_a.to_string(),
                    o_id: kept_synced_oid.clone(),
                    o_type: "blob".to_string(),
                    o_size: 888,
                    created_at: 1_795_399_999,
                    is_synced: false,
                },
            ];
            store
                .upsert_object_index_batch(&promo_batch)
                .await
                .expect("promotion upsert");
            // Subsequent generic write must NOT demote `agent_session`.
            store
                .upsert_object_index_batch(&[ObjectIndexRecord {
                    repo_id: repo_a.to_string(),
                    o_id: promoted_oid.clone(),
                    o_type: "blob".to_string(),
                    o_size: 1,
                    created_at: 1_795_400_000,
                    is_synced: true,
                }])
                .await
                .expect("non-demotion upsert");
        }

        for store in &backends {
            let promoted = store
                .get_object_index(repo_a, &promoted_oid)
                .await
                .expect("get promoted")
                .expect("promoted row exists");
            assert_eq!(promoted.o_type, "agent_session");
            assert_eq!(promoted.o_size, 999);
            assert!(!promoted.is_synced, "promotion must reset is_synced=false");
            assert_eq!(
                promoted.created_at, 1_795_370_001,
                "original created_at preserved"
            );

            let kept = store
                .get_object_index(repo_a, &kept_synced_oid)
                .await
                .expect("get kept")
                .expect("kept row exists");
            assert_eq!(kept.o_type, "blob");
            assert_eq!(kept.o_size, 32);
            assert!(
                kept.is_synced,
                "generic conflict must not reopen synced upload"
            );

            let synced_only = store
                .list_object_index_by_repo(repo_a, Some(true))
                .await
                .expect("list synced");
            assert_eq!(synced_only.len(), 9);
        }
        assert_eq!(
            backends[0]
                .list_object_index_by_repo(repo_a, Some(true))
                .await
                .expect("sq synced"),
            backends[1]
                .list_object_index_by_repo(repo_a, Some(true))
                .await
                .expect("rb synced"),
        );

        // Same-transaction `OBJECT_BY_PACK` secondary index verification on RedbGitStore.
        let pack_obj_a = ObjectIndexRecord {
            repo_id: repo_a.to_string(),
            o_id: sha1_oid(5_001),
            o_type: "blob".to_string(),
            o_size: 128,
            created_at: 1_795_371_000,
            is_synced: false,
        };
        let pack_obj_b = ObjectIndexRecord {
            repo_id: repo_a.to_string(),
            o_id: sha1_oid(5_002),
            o_type: "tree".to_string(),
            o_size: 256,
            created_at: 1_795_371_001,
            is_synced: false,
        };
        redb_store
            .upsert_object_index_batch_with_pack(
                &[pack_obj_a.clone(), pack_obj_b.clone()],
                Some("pack-001"),
            )
            .await
            .expect("bind objects to pack-001");
        assert_eq!(
            redb_store
                .list_object_index_by_pack("pack-001", Some(repo_a))
                .await
                .expect("list pack-001"),
            vec![pack_obj_a.clone(), pack_obj_b.clone()]
        );
        // Repack `pack_obj_a` into `pack-002` and verify stale `pack-001` entry is removed atomically.
        redb_store
            .upsert_object_index_batch_with_pack(
                std::slice::from_ref(&pack_obj_a),
                Some("pack-002"),
            )
            .await
            .expect("repack pack_obj_a to pack-002");
        assert_eq!(
            redb_store
                .list_object_index_by_pack("pack-001", Some(repo_a))
                .await
                .expect("list pack-001 after repack"),
            vec![pack_obj_b.clone()]
        );
        assert_eq!(
            redb_store
                .list_object_index_by_pack("pack-002", Some(repo_a))
                .await
                .expect("list pack-002 after repack"),
            vec![pack_obj_a.clone()]
        );

        // ---------------------------------------------------------------------
        // AC2 & AC3: `object_obliteration` fence blocks BOTH read and write
        // paths across `Obliterating` and `Obliterated` states (SHA-1 & SHA-256)
        // with atomic zero-partial-write rollback.
        // ---------------------------------------------------------------------
        let oblit_sha1_existing = sha1_oid(5); // already indexed in repo_a
        let oblit_sha256_unindexed = sha256_oid(999); // never indexed before

        let req_sha1 = ObliterationRequest {
            oid: oblit_sha1_existing.clone(),
            hash_kind: ObjectHashKind::Sha1,
            reason: Some("gdpr-erasure".to_string()),
            actor: Some("compliance-bot".to_string()),
            approval_source: Some("ticket-42".to_string()),
            recorded_at: "2026-11-22T18:00:00Z".to_string(),
        };
        let req_sha256 = ObliterationRequest {
            oid: oblit_sha256_unindexed.clone(),
            hash_kind: ObjectHashKind::Sha256,
            reason: Some("secret-purge".to_string()),
            actor: Some("security-ops".to_string()),
            approval_source: Some("incident-7".to_string()),
            recorded_at: "2026-11-22T18:01:00Z".to_string(),
        };

        for store in &backends {
            // Cannot mark_object_obliterated before writing the `obliterating` tombstone.
            let not_found_err = store
                .mark_object_obliterated(
                    &oblit_sha1_existing,
                    ObjectHashKind::Sha1,
                    "2026-11-22T18:02:00Z",
                )
                .await
                .expect_err("mark_object_obliterated before begin must fail closed");
            assert!(matches!(
                not_found_err,
                StorageBackendError::NotFound { .. }
            ));

            // Live -> Obliterating (idempotent).
            let t1 = store
                .begin_object_obliteration(&req_sha1)
                .await
                .expect("begin sha1 obliteration");
            assert_eq!(t1.state, ObliterationState::Obliterating);
            assert_eq!(t1.payload_deleted_at, None);
            let t1_replay = store
                .begin_object_obliteration(&req_sha1)
                .await
                .expect("idempotent begin sha1");
            assert_eq!(t1_replay, t1);

            let t2 = store
                .begin_object_obliteration(&req_sha256)
                .await
                .expect("begin sha256 obliteration");
            assert_eq!(t2.state, ObliterationState::Obliterating);

            // AC2 (Obliterating state): read path MUST be intercepted!
            let read_err = store
                .get_object_index(repo_a, &oblit_sha1_existing)
                .await
                .expect_err("AC2: get_object_index must fence obliterating OID");
            assert!(
                is_obliteration_fenced_error(&read_err),
                "unexpected error: {read_err}"
            );
            let read_invoked = Arc::new(AtomicBool::new(false));
            let read_invoked_c = Arc::clone(&read_invoked);
            assert!(is_obliteration_fenced_error(
                &store
                    .run_fenced_object_read(
                        &oblit_sha1_existing,
                        Box::new(move || {
                            read_invoked_c.store(true, Ordering::SeqCst);
                            Ok(b"must-not-read".to_vec())
                        }),
                    )
                    .await
                    .expect_err("AC2: run_fenced_object_read must fence obliterating OID")
            ));
            assert!(
                !read_invoked.load(Ordering::SeqCst),
                "pre-check must reject tombstoned read without invoking payload loader"
            );
            // Un-indexed tombstoned OID must ALSO be fenced on read (not Ok(None)).
            assert!(is_obliteration_fenced_error(
                &store
                    .get_object_index(repo_a, &oblit_sha256_unindexed)
                    .await
                    .expect_err("AC2: get_object_index must fence unindexed tombstoned OID")
            ));
            // Historical `object_index` address row is preserved for compliance/fsck.
            assert!(
                store
                    .get_object_index_preserved_address(repo_a, &oblit_sha1_existing)
                    .await
                    .expect("preserved address read")
                    .is_some()
            );
            // `list_object_index_by_repo` excludes tombstoned OIDs.
            assert!(
                !store
                    .list_object_index_by_repo(repo_a, None)
                    .await
                    .expect("list repo_a excludes tombstoned")
                    .iter()
                    .any(|r| r.o_id == oblit_sha1_existing)
            );

            // AC3 (Obliterating state): write path MUST be intercepted atomically!
            let fresh_live_oid = sha1_oid(99_001);
            let poisoned_batch = vec![
                ObjectIndexRecord {
                    repo_id: repo_a.to_string(),
                    o_id: fresh_live_oid.clone(),
                    o_type: "blob".to_string(),
                    o_size: 42,
                    created_at: 1_795_372_000,
                    is_synced: false,
                },
                ObjectIndexRecord {
                    repo_id: repo_a.to_string(),
                    o_id: oblit_sha1_existing.clone(),
                    o_type: "blob".to_string(),
                    o_size: 99,
                    created_at: 1_795_372_001,
                    is_synced: false,
                },
            ];
            let write_err = store
                .upsert_object_index_batch(&poisoned_batch)
                .await
                .expect_err("AC3: batch containing obliterating OID must fail");
            assert!(
                is_obliteration_fenced_error(&write_err),
                "unexpected error: {write_err}"
            );
            // Zero partial writes: `fresh_live_oid` must NOT have been committed!
            assert_eq!(
                store
                    .get_object_index(repo_a, &fresh_live_oid)
                    .await
                    .expect("verify atomic rollback"),
                None
            );
            let write_invoked = Arc::new(AtomicBool::new(false));
            let write_invoked_c = Arc::clone(&write_invoked);
            assert!(is_obliteration_fenced_error(
                &store
                    .run_fenced_object_write(
                        &ObjectIndexRecord {
                            repo_id: repo_a.to_string(),
                            o_id: oblit_sha256_unindexed.clone(),
                            o_type: "blob".to_string(),
                            o_size: 7,
                            created_at: 1_795_372_002,
                            is_synced: false,
                        },
                        Box::new(move || {
                            write_invoked_c.store(true, Ordering::SeqCst);
                            Ok(())
                        }),
                    )
                    .await
                    .expect_err("AC3: run_fenced_object_write must fence obliterating OID")
            ));
            assert!(
                !write_invoked.load(Ordering::SeqCst),
                "tombstoned write must fail before invoking payload writer"
            );

            // Obliterating -> Obliterated (idempotent).
            let done_t1 = store
                .mark_object_obliterated(
                    &oblit_sha1_existing,
                    ObjectHashKind::Sha1,
                    "2026-11-22T18:05:00Z",
                )
                .await
                .expect("finalize sha1 obliteration");
            assert_eq!(done_t1.state, ObliterationState::Obliterated);
            assert_eq!(
                done_t1.payload_deleted_at.as_deref(),
                Some("2026-11-22T18:05:00Z")
            );
            // Replay mark_object_obliterated keeps original payload_deleted_at.
            let done_t1_replay = store
                .mark_object_obliterated(
                    &oblit_sha1_existing,
                    ObjectHashKind::Sha1,
                    "2026-11-22T19:00:00Z",
                )
                .await
                .expect("idempotent finalize");
            assert_eq!(done_t1_replay, done_t1);

            // AC2 & AC3 (Obliterated state): read & write paths remain fenced permanently!
            assert!(is_obliteration_fenced_error(
                &store
                    .get_object_index(repo_a, &oblit_sha1_existing)
                    .await
                    .expect_err("AC2: get_object_index fences obliterated OID")
            ));
            assert!(is_obliteration_fenced_error(
                &store
                    .run_fenced_object_read(
                        &oblit_sha1_existing,
                        Box::new(|| Ok(b"erased".to_vec())),
                    )
                    .await
                    .expect_err("AC2: run_fenced_object_read fences obliterated OID")
            ));
            assert!(is_obliteration_fenced_error(
                &store
                    .run_fenced_object_write(
                        &ObjectIndexRecord {
                            repo_id: repo_a.to_string(),
                            o_id: oblit_sha1_existing.clone(),
                            o_type: "blob".to_string(),
                            o_size: 12,
                            created_at: 1_795_372_003,
                            is_synced: false,
                        },
                        Box::new(|| Ok(())),
                    )
                    .await
                    .expect_err("AC3: run_fenced_object_write fences obliterated OID")
            ));
            // `mark_object_index_synced` also fails atomically when an obliterated OID is included.
            let sync_target_live = sha1_oid(20);
            let sync_err = store
                .mark_object_index_synced(
                    repo_a,
                    &[sync_target_live.clone(), oblit_sha1_existing.clone()],
                )
                .await
                .expect_err("AC3: mark_object_index_synced fences obliterated OID");
            assert!(is_obliteration_fenced_error(&sync_err));
            assert!(
                !store
                    .get_object_index(repo_a, &sync_target_live)
                    .await
                    .expect("check rollback of live sync row")
                    .expect("live row exists")
                    .is_synced,
                "AC3: failed mark_object_index_synced must leave zero partial updates"
            );
            assert!(
                store
                    .is_object_obliterated(&oblit_sha1_existing)
                    .await
                    .expect("is_oblit")
            );
            assert!(
                !store
                    .is_object_obliterated(&sha1_oid(20))
                    .await
                    .expect("live not oblit")
            );
        }

        // Verify `RedbGitStore` `OBJECT_BY_PACK` also rolls back on obliteration fence
        // and excludes tombstoned objects from pack reads.
        let pack_rollback_err = redb_store
            .upsert_object_index_batch_with_pack(
                &[
                    ObjectIndexRecord {
                        repo_id: repo_a.to_string(),
                        o_id: sha1_oid(5_099),
                        o_type: "blob".to_string(),
                        o_size: 10,
                        created_at: 1_795_373_000,
                        is_synced: false,
                    },
                    ObjectIndexRecord {
                        repo_id: repo_a.to_string(),
                        o_id: oblit_sha1_existing.clone(),
                        o_type: "blob".to_string(),
                        o_size: 10,
                        created_at: 1_795_373_000,
                        is_synced: false,
                    },
                ],
                Some("pack-rollback"),
            )
            .await
            .expect_err("pack upsert with obliterated OID must roll back");
        assert!(is_obliteration_fenced_error(&pack_rollback_err));
        assert!(
            redb_store
                .list_object_index_by_pack("pack-rollback", Some(repo_a))
                .await
                .expect("empty pack after rollback")
                .is_empty()
        );
        // Tombstoning `pack_obj_b` hides it from `list_object_index_by_pack`.
        redb_store
            .begin_object_obliteration(&ObliterationRequest {
                oid: pack_obj_b.o_id.clone(),
                hash_kind: ObjectHashKind::Sha1,
                reason: Some("pack-read-fence".to_string()),
                actor: None,
                approval_source: None,
                recorded_at: "2026-11-22T18:06:00Z".to_string(),
            })
            .await
            .expect("tombstone pack_obj_b");
        assert!(
            redb_store
                .list_object_index_by_pack("pack-001", Some(repo_a))
                .await
                .expect("pack-001 excludes tombstoned object")
                .is_empty()
        );

        // Mirror `pack_obj_b` tombstone onto SQLite too so global obliteration lists match 1:1.
        sqlite_store
            .begin_object_obliteration(&ObliterationRequest {
                oid: pack_obj_b.o_id.clone(),
                hash_kind: ObjectHashKind::Sha1,
                reason: Some("pack-read-fence".to_string()),
                actor: None,
                approval_source: None,
                recorded_at: "2026-11-22T18:06:00Z".to_string(),
            })
            .await
            .expect("mirror tombstone on sqlite");

        assert_eq!(
            backends[0]
                .list_incomplete_obliterations()
                .await
                .expect("sq incomplete"),
            backends[1]
                .list_incomplete_obliterations()
                .await
                .expect("rb incomplete"),
        );
        assert_eq!(
            backends[0]
                .list_obliterated_oids()
                .await
                .expect("sq oblit oids"),
            backends[1]
                .list_obliterated_oids()
                .await
                .expect("rb oblit oids"),
        );
        assert_eq!(
            backends[0]
                .get_object_obliteration(&oblit_sha1_existing)
                .await
                .expect("sq get oblit"),
            backends[1]
                .get_object_obliteration(&oblit_sha1_existing)
                .await
                .expect("rb get oblit"),
        );

        // ---------------------------------------------------------------------
        // AC4: `notes` CRUD + CAS batch equivalence across SQLite and Redb.
        // ---------------------------------------------------------------------
        let notes_commits = "refs/notes/commits";
        let notes_review = "refs/notes/review";
        let obj1 = sha1_oid(101);
        let obj2 = sha1_oid(102);
        let obj3 = sha256_oid(103);
        let blob1 = sha1_oid(201);
        let blob2 = sha1_oid(202);
        let blob3 = sha256_oid(203);

        for store in &backends {
            // Invalid notes ref rejected identically.
            assert!(
                store
                    .put_note("refs/heads/main", &obj1, &blob1, false)
                    .await
                    .is_err()
            );

            assert_eq!(
                store
                    .put_note(notes_commits, &obj1, &blob1, false)
                    .await
                    .expect("insert note 1"),
                NoteWriteOutcome::Inserted
            );
            assert_eq!(
                store
                    .put_note(notes_commits, &obj1, &blob2, false)
                    .await
                    .expect("conflict without overwrite"),
                NoteWriteOutcome::AlreadyExists {
                    existing_blob: blob1.clone()
                }
            );
            assert_eq!(
                store
                    .put_note(notes_commits, &obj1, &blob2, true)
                    .await
                    .expect("overwrite note 1"),
                NoteWriteOutcome::Replaced {
                    previous_blob: blob1.clone()
                }
            );
            assert_eq!(
                store
                    .put_note(notes_review, &obj3, &blob3, false)
                    .await
                    .expect("insert sha256 review note"),
                NoteWriteOutcome::Inserted
            );

            // Atomic CAS batch conflict rolls back ALL earlier ops in the batch.
            let conflict = store
                .apply_notes_cas_batch(
                    notes_commits,
                    &[
                        NoteCasOp::InsertIfAbsent {
                            object: obj2.clone(),
                            blob: blob1.clone(),
                        },
                        NoteCasOp::UpdateIfMatches {
                            object: obj1.clone(),
                            expected_blob: blob1.clone(), // stale! actual is blob2
                            new_blob: blob3.clone(),
                        },
                    ],
                )
                .await
                .expect("batch CAS conflict returns Ok(CasConflict)");
            assert_eq!(
                conflict,
                NoteBatchOutcome::CasConflict {
                    object: obj1.clone(),
                    actual_blob: Some(blob2.clone()),
                }
            );
            assert_eq!(
                store
                    .get_note(notes_commits, &obj2)
                    .await
                    .expect("obj2 rolled back"),
                None
            );

            // Atomic CAS batch apply succeeds across insert + update.
            let applied = store
                .apply_notes_cas_batch(
                    notes_commits,
                    &[
                        NoteCasOp::InsertIfAbsent {
                            object: obj2.clone(),
                            blob: blob1.clone(),
                        },
                        NoteCasOp::UpdateIfMatches {
                            object: obj1.clone(),
                            expected_blob: blob2.clone(),
                            new_blob: blob3.clone(),
                        },
                    ],
                )
                .await
                .expect("batch CAS apply");
            assert_eq!(
                applied,
                NoteBatchOutcome::Applied {
                    inserted: 1,
                    updated: 1,
                    deleted: 0,
                }
            );

            // `remove_note_cas` respects expected_blob guard.
            assert!(
                !store
                    .remove_note_cas(notes_commits, &obj2, Some(&blob2))
                    .await
                    .expect("cas mismatch remove")
            );
            assert!(
                store
                    .remove_note_cas(notes_commits, &obj2, Some(&blob1))
                    .await
                    .expect("cas match remove")
            );
        }

        for r in [notes_commits, notes_review] {
            assert_eq!(
                backends[0].list_notes(r).await.expect("sq notes"),
                backends[1].list_notes(r).await.expect("rb notes"),
                "AC4: list_notes({r}) must match identically"
            );
        }

        // ---------------------------------------------------------------------
        // AC5: `revision_ordinal` & `revision_ordinal_meta` parity across
        // SHA-1 (40-hex), SHA-256 (64-hex), and all-zero OIDs.
        // ---------------------------------------------------------------------
        let zero_sha1 = "0".repeat(40);
        let zero_sha256 = "0".repeat(64);

        let sha1_ref = "refs/heads/sha1-main";
        let sha1_entries = vec![
            (1, zero_sha1.clone()),
            (2, sha1_oid(301)),
            (3, sha1_oid(302)),
        ];
        let sha1_meta = RevisionOrdinalMeta {
            ref_name: sha1_ref.to_string(),
            tip_oid: sha1_oid(302),
            replace_sig: "sig-v1".to_string(),
            max_ordinal: 3,
            built_at: "2026-11-22T18:10:00Z".to_string(),
        };

        let sha256_ref = "refs/heads/sha256-main";
        let sha256_entries = vec![(1, sha256_oid(401)), (2, zero_sha256.clone())];
        let sha256_meta = RevisionOrdinalMeta {
            ref_name: sha256_ref.to_string(),
            tip_oid: zero_sha256.clone(),
            replace_sig: String::new(),
            max_ordinal: 2,
            built_at: "2026-11-22T18:11:00Z".to_string(),
        };

        let empty_zero_ref = "refs/heads/unborn-zero";
        let empty_zero_meta = RevisionOrdinalMeta {
            ref_name: empty_zero_ref.to_string(),
            tip_oid: zero_sha1.clone(),
            replace_sig: String::new(),
            max_ordinal: 0,
            built_at: "2026-11-22T18:12:00Z".to_string(),
        };

        for store in &backends {
            store
                .replace_revision_ordinals(&sha1_meta, &sha1_entries)
                .await
                .expect("replace sha1 ordinals");
            store
                .replace_revision_ordinals(&sha256_meta, &sha256_entries)
                .await
                .expect("replace sha256 ordinals");
            store
                .replace_revision_ordinals(&empty_zero_meta, &[])
                .await
                .expect("replace empty zero-oid meta");

            // Stale replace_sig append returns Stale with zero writes.
            let stale_meta = RevisionOrdinalMeta {
                ref_name: sha1_ref.to_string(),
                tip_oid: sha1_oid(303),
                replace_sig: "sig-v2".to_string(),
                max_ordinal: 4,
                built_at: "2026-11-22T18:13:00Z".to_string(),
            };
            let stale_out = store
                .append_revision_ordinals(
                    sha1_ref,
                    &sha1_oid(302),
                    "sig-v2",
                    &stale_meta,
                    &[sha1_oid(303)],
                )
                .await
                .expect("stale append returns Ok(Stale)");
            assert_eq!(
                stale_out,
                RevisionOrdinalAppendOutcome::Stale {
                    actual: Some(sha1_meta.clone())
                }
            );

            // Matching fast-forward append advances max_ordinal 3 -> 5 atomically.
            let ff_meta = RevisionOrdinalMeta {
                ref_name: sha1_ref.to_string(),
                tip_oid: sha1_oid(304),
                replace_sig: "sig-v1".to_string(),
                max_ordinal: 5,
                built_at: "2026-11-22T18:14:00Z".to_string(),
            };
            let ff_out = store
                .append_revision_ordinals(
                    sha1_ref,
                    &sha1_oid(302),
                    "sig-v1",
                    &ff_meta,
                    &[sha1_oid(303), sha1_oid(304)],
                )
                .await
                .expect("fast-forward append");
            assert_eq!(
                ff_out,
                RevisionOrdinalAppendOutcome::Applied {
                    meta: ff_meta.clone()
                }
            );

            // Duplicate OID in append must fail closed leaving rows untouched!
            let dup_meta = RevisionOrdinalMeta {
                ref_name: sha1_ref.to_string(),
                tip_oid: sha1_oid(301),
                replace_sig: "sig-v1".to_string(),
                max_ordinal: 6,
                built_at: "2026-11-22T18:15:00Z".to_string(),
            };
            assert!(
                store
                    .append_revision_ordinals(
                        sha1_ref,
                        &sha1_oid(304),
                        "sig-v1",
                        &dup_meta,
                        &[sha1_oid(301)],
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .count_revision_ordinals(sha1_ref)
                    .await
                    .expect("count after dup"),
                5
            );

            // Full rebuild shrinking `sha256_ref` must clear stale reverse-index entries.
            let rebuilt_sha256_meta = RevisionOrdinalMeta {
                ref_name: sha256_ref.to_string(),
                tip_oid: zero_sha256.clone(),
                replace_sig: "sig-rebuilt".to_string(),
                max_ordinal: 1,
                built_at: "2026-11-22T18:16:00Z".to_string(),
            };
            store
                .replace_revision_ordinals(&rebuilt_sha256_meta, &[(1, zero_sha256.clone())])
                .await
                .expect("rebuild sha256 ref");
            assert_eq!(
                store
                    .find_revision_ordinal_by_oid(sha256_ref, &sha256_oid(401))
                    .await
                    .expect("old oid cleared from reverse index"),
                None
            );
            assert_eq!(
                store
                    .find_revision_ordinal_by_oid(sha256_ref, &zero_sha256)
                    .await
                    .expect("all-zero sha256 reverse lookup"),
                Some(1)
            );
            assert_eq!(
                store
                    .find_revision_oid_by_ordinal(sha1_ref, 1)
                    .await
                    .expect("all-zero sha1 forward lookup"),
                Some(zero_sha1.clone())
            );
            assert_eq!(
                store
                    .find_revision_ordinal_by_oid(sha1_ref, &zero_sha1)
                    .await
                    .expect("all-zero sha1 reverse lookup"),
                Some(1)
            );

            // Prune `empty_zero_ref`, keeping `sha1_ref` and `sha256_ref`.
            let pruned = store
                .prune_revision_ordinals_except(&[sha1_ref.to_string(), sha256_ref.to_string()])
                .await
                .expect("prune missing refs");
            assert_eq!(pruned, 1);
            assert_eq!(
                store
                    .get_revision_ordinal_meta(empty_zero_ref)
                    .await
                    .expect("pruned meta gone"),
                None
            );
        }

        // Exact final parity assertions across all revision ordinal queries.
        assert_eq!(
            backends[0]
                .list_revision_ordinal_metas()
                .await
                .expect("sq metas"),
            backends[1]
                .list_revision_ordinal_metas()
                .await
                .expect("rb metas"),
        );
        for ref_name in [sha1_ref, sha256_ref] {
            assert_eq!(
                backends[0]
                    .list_revision_ordinals(ref_name)
                    .await
                    .expect("sq ordinals"),
                backends[1]
                    .list_revision_ordinals(ref_name)
                    .await
                    .expect("rb ordinals"),
                "AC5: revision_ordinal rows for {ref_name} must match identically"
            );
            assert_eq!(
                backends[0]
                    .count_revision_ordinals(ref_name)
                    .await
                    .expect("sq count"),
                backends[1]
                    .count_revision_ordinals(ref_name)
                    .await
                    .expect("rb count"),
            );
        }

        // ---------------------------------------------------------------------
        // Contested obliteration interleavings on BOTH SQLite and Redb (separate
        // store handles): verifies race-free `run_fenced_object_read` and
        // mutually-exclusive `run_fenced_object_write`.
        // ---------------------------------------------------------------------
        let sqlite_peer = SqliteGitStore::open(&sqlite_path)
            .await
            .expect("open second SqliteGitStore handle");
        let redb_peer =
            RedbGitStore::open_read_write(&redb_path).expect("open second RedbGitStore handle");
        let peer_pairs: [(Arc<dyn GitStore>, Arc<dyn GitStore>); 2] = [
            (Arc::clone(&backends[0]), Arc::new(sqlite_peer)),
            (Arc::clone(&backends[1]), Arc::new(redb_peer)),
        ];

        for (idx, (primary, peer)) in peer_pairs.iter().enumerate() {
            let base = u64::try_from(70_000 + idx * 100).expect("fits u64");

            // (a) Contested read interleaving: tombstone commits AFTER pre-check
            // and BEFORE `read_payload` returns `Ok(secret)` -> post-check MUST
            // discard the payload bytes and return `obliteration_fenced_read_error`.
            let race_read_oid = sha1_oid(base + 1);
            let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
            let (tombstoned_tx, tombstoned_rx) = std::sync::mpsc::channel::<()>();
            let peer_for_read = Arc::clone(peer);
            let oid_for_tomb = race_read_oid.clone();
            let oblit_task = tokio::spawn(async move {
                entered_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("read_payload entered");
                peer_for_read
                    .begin_object_obliteration(&ObliterationRequest {
                        oid: oid_for_tomb,
                        hash_kind: ObjectHashKind::Sha1,
                        reason: Some("contested-read-race".to_string()),
                        actor: None,
                        approval_source: None,
                        recorded_at: "2026-11-22T18:30:00Z".to_string(),
                    })
                    .await
                    .expect("commit tombstone during payload read");
                tombstoned_tx.send(()).expect("signal tombstone committed");
            });
            let raced_read_err = primary
                .run_fenced_object_read(
                    &race_read_oid,
                    Box::new(move || {
                        entered_tx.send(()).expect("signal read entered");
                        tombstoned_rx
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .expect("tombstone committed before read returns");
                        Ok(b"sensitive-erased-payload".to_vec())
                    }),
                )
                .await
                .expect_err("contested read must discard bytes and fail closed");
            oblit_task.await.expect("join read-race oblit task");
            assert!(
                is_obliteration_fenced_error(&raced_read_err),
                "contested read must return obliteration fence error: {raced_read_err}"
            );

            // (b) Contested read interleaving when payload returns `NotFound`
            // after concurrent unlink -> post-check overrides with obliteration fence error.
            let race_read_miss_oid = sha1_oid(base + 2);
            let (entered_tx2, entered_rx2) = std::sync::mpsc::channel::<()>();
            let (tombstoned_tx2, tombstoned_rx2) = std::sync::mpsc::channel::<()>();
            let peer_for_miss = Arc::clone(peer);
            let oid_for_miss = race_read_miss_oid.clone();
            let oblit_miss_task = tokio::spawn(async move {
                entered_rx2
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("read_payload miss entered");
                peer_for_miss
                    .begin_object_obliteration(&ObliterationRequest {
                        oid: oid_for_miss,
                        hash_kind: ObjectHashKind::Sha1,
                        reason: Some("contested-read-unlink".to_string()),
                        actor: None,
                        approval_source: None,
                        recorded_at: "2026-11-22T18:30:30Z".to_string(),
                    })
                    .await
                    .expect("commit tombstone before NotFound");
                tombstoned_tx2.send(()).expect("signal miss tombstone");
            });
            let miss_err = primary
                .run_fenced_object_read(
                    &race_read_miss_oid,
                    Box::new(move || {
                        entered_tx2.send(()).expect("signal miss read entered");
                        tombstoned_rx2
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .expect("miss tombstone committed");
                        Err(StorageBackendError::NotFound {
                            resource: "loose payload unlinked".to_string(),
                        })
                    }),
                )
                .await
                .expect_err("post-unlink NotFound must surface obliteration fence error");
            oblit_miss_task.await.expect("join miss oblit task");
            assert!(
                is_obliteration_fenced_error(&miss_err),
                "expected obliteration fence error over NotFound, got: {miss_err}"
            );

            // (c) Contested write interleaving: `run_fenced_object_write` holds
            // the exclusive write transaction across `write_payload` + index
            // registration, serializing any concurrent `begin_object_obliteration`
            // strictly AFTER the write commits; subsequent fenced writes fail
            // closed without invoking `write_payload`.
            let race_write_oid = sha1_oid(base + 3);
            let timeline = Arc::new(Mutex::new(Vec::<&'static str>::new()));
            let (write_entered_tx, write_entered_rx) = std::sync::mpsc::channel::<()>();
            let peer_for_write = Arc::clone(peer);
            let oid_for_write_oblit = race_write_oid.clone();
            let timeline_oblit = Arc::clone(&timeline);
            let oblit_write_task = tokio::spawn(async move {
                write_entered_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("write_payload entered");
                peer_for_write
                    .begin_object_obliteration(&ObliterationRequest {
                        oid: oid_for_write_oblit,
                        hash_kind: ObjectHashKind::Sha1,
                        reason: Some("contested-write-race".to_string()),
                        actor: None,
                        approval_source: None,
                        recorded_at: "2026-11-22T18:31:00Z".to_string(),
                    })
                    .await
                    .expect("tombstone completes strictly after fenced write commits");
                timeline_oblit
                    .lock()
                    .expect("lock timeline")
                    .push("oblit_done");
            });
            let timeline_writer = Arc::clone(&timeline);
            let write_rec = ObjectIndexRecord {
                repo_id: repo_a.to_string(),
                o_id: race_write_oid.clone(),
                o_type: "blob".to_string(),
                o_size: 512,
                created_at: 1_795_375_000,
                is_synced: false,
            };
            primary
                .run_fenced_object_write(
                    &write_rec,
                    Box::new(move || {
                        timeline_writer
                            .lock()
                            .expect("lock timeline")
                            .push("write_start");
                        write_entered_tx.send(()).expect("signal write entered");
                        std::thread::sleep(std::time::Duration::from_millis(120));
                        timeline_writer
                            .lock()
                            .expect("lock timeline")
                            .push("write_end");
                        Ok(())
                    }),
                )
                .await
                .expect("first fenced write succeeds before concurrent tombstone");
            oblit_write_task.await.expect("join write-race oblit task");
            assert_eq!(
                *timeline.lock().expect("lock timeline"),
                vec!["write_start", "write_end", "oblit_done"],
                "begin_object_obliteration must serialize strictly after run_fenced_object_write"
            );
            // Subsequent fenced write against the now-tombstoned OID must fail
            // closed before invoking `write_payload`.
            let second_write_invoked = Arc::new(AtomicBool::new(false));
            let second_write_invoked_c = Arc::clone(&second_write_invoked);
            let second_write_err = primary
                .run_fenced_object_write(
                    &write_rec,
                    Box::new(move || {
                        second_write_invoked_c.store(true, Ordering::SeqCst);
                        Ok(())
                    }),
                )
                .await
                .expect_err("post-obliteration fenced write must fail closed");
            assert!(is_obliteration_fenced_error(&second_write_err));
            assert!(
                !second_write_invoked.load(Ordering::SeqCst),
                "post-obliteration write_payload must never run"
            );

            // (d) Cancelled writer safety on BOTH SQLite and Redb: aborting the
            // caller's `run_fenced_object_write` future while `write_payload` is
            // still running MUST keep the DB write-lock fence held until
            // `write_payload` exits, then roll back the transaction before any
            // concurrent `begin_object_obliteration` can commit.
            let cancel_oid = sha1_oid(base + 4);
            let cancel_timeline = Arc::new(Mutex::new(Vec::<&'static str>::new()));
            let (cancel_entered_tx, cancel_entered_rx) = std::sync::mpsc::channel::<()>();
            let (cancel_release_tx, cancel_release_rx) = std::sync::mpsc::channel::<()>();
            let cancel_rec = ObjectIndexRecord {
                repo_id: repo_a.to_string(),
                o_id: cancel_oid.clone(),
                o_type: "blob".to_string(),
                o_size: 64,
                created_at: 1_795_375_100,
                is_synced: false,
            };
            let primary_for_cancel = Arc::clone(primary);
            let cancel_rec_for_task = cancel_rec.clone();
            let cancel_timeline_writer = Arc::clone(&cancel_timeline);
            let cancel_write_task = tokio::spawn(async move {
                primary_for_cancel
                    .run_fenced_object_write(
                        &cancel_rec_for_task,
                        Box::new(move || {
                            cancel_timeline_writer
                                .lock()
                                .expect("lock cancel timeline")
                                .push("cancel_write_entered");
                            cancel_entered_tx
                                .send(())
                                .expect("signal cancel writer entered");
                            cancel_release_rx
                                .recv_timeout(std::time::Duration::from_secs(5))
                                .expect("wait for cancel writer release");
                            cancel_timeline_writer
                                .lock()
                                .expect("lock cancel timeline")
                                .push("cancel_write_exited");
                            Ok(())
                        }),
                    )
                    .await
            });

            cancel_entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("paused writer entered payload callback");
            // Cancel/drop the caller future while `write_payload` is still paused.
            cancel_write_task.abort();
            let _ = cancel_write_task.await;

            // Start concurrent obliteration on peer handle; it MUST remain
            // blocked while the detached payload writer still holds the fence.
            let peer_for_cancel = Arc::clone(peer);
            let cancel_oid_for_oblit = cancel_oid.clone();
            let cancel_timeline_oblit = Arc::clone(&cancel_timeline);
            let mut cancel_oblit_task = tokio::spawn(async move {
                peer_for_cancel
                    .begin_object_obliteration(&ObliterationRequest {
                        oid: cancel_oid_for_oblit,
                        hash_kind: ObjectHashKind::Sha1,
                        reason: Some("cancelled-writer-fence".to_string()),
                        actor: None,
                        approval_source: None,
                        recorded_at: "2026-11-22T18:32:00Z".to_string(),
                    })
                    .await
                    .expect("obliteration commits after cancelled writer exits");
                cancel_timeline_oblit
                    .lock()
                    .expect("lock cancel timeline")
                    .push("cancel_oblit_done");
            });

            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(120),
                    &mut cancel_oblit_task,
                )
                .await
                .is_err(),
                "obliteration must stay blocked while cancelled writer's payload task is still running"
            );

            // Release the paused writer; it must roll back its transaction and
            // only then let `begin_object_obliteration` commit.
            cancel_release_tx
                .send(())
                .expect("release paused cancelled writer");
            cancel_oblit_task
                .await
                .expect("join cancel-case oblit task");
            assert_eq!(
                *cancel_timeline.lock().expect("lock cancel timeline"),
                vec![
                    "cancel_write_entered",
                    "cancel_write_exited",
                    "cancel_oblit_done",
                ],
                "cancelled writer must hold fence until payload exit before obliteration commits"
            );
            assert_eq!(
                primary
                    .get_object_index_preserved_address(repo_a, &cancel_oid)
                    .await
                    .expect("cancelled writer must roll back object_index row"),
                None
            );
            assert!(
                primary
                    .is_object_obliterated(&cancel_oid)
                    .await
                    .expect("tombstone persisted after cancelled writer rollback")
            );
        }

        // (e) Runtime-shutdown fence safety on BOTH SQLite and Redb: pausing
        // `write_payload` inside a dedicated Tokio runtime and calling
        // `shutdown_background()` MUST keep the DB write-lock fence held until
        // `write_payload` exits before a concurrent obliteration on another
        // handle/runtime can commit.
        for is_redb in [false, true] {
            let sd_oid = sha1_oid(if is_redb { 71_002 } else { 71_001 });
            let sd_timeline = Arc::new(Mutex::new(Vec::<&'static str>::new()));
            let (sd_entered_tx, sd_entered_rx) = std::sync::mpsc::channel::<()>();
            let (sd_release_tx, sd_release_rx) = std::sync::mpsc::channel::<()>();
            let sd_rec = ObjectIndexRecord {
                repo_id: repo_a.to_string(),
                o_id: sd_oid.clone(),
                o_type: "blob".to_string(),
                o_size: 80,
                created_at: 1_795_375_200,
                is_synced: false,
            };

            let ephemeral_rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("build ephemeral runtime");
            let sd_timeline_writer = Arc::clone(&sd_timeline);
            let sd_rec_clone = sd_rec.clone();
            let sqlite_path_clone = sqlite_path.clone();
            let redb_path_clone = redb_path.clone();
            ephemeral_rt.spawn(async move {
                let writer_store: Arc<dyn GitStore> = if is_redb {
                    Arc::new(
                        RedbGitStore::open_read_write(&redb_path_clone)
                            .expect("open redb store for shutdown test"),
                    )
                } else {
                    Arc::new(
                        SqliteGitStore::open(&sqlite_path_clone)
                            .await
                            .expect("open sqlite store for shutdown test"),
                    )
                };
                let _ = writer_store
                    .run_fenced_object_write(
                        &sd_rec_clone,
                        Box::new(move || {
                            sd_timeline_writer
                                .lock()
                                .expect("lock shutdown timeline")
                                .push("shutdown_write_entered");
                            sd_entered_tx
                                .send(())
                                .expect("signal shutdown writer entered");
                            sd_release_rx
                                .recv_timeout(std::time::Duration::from_secs(5))
                                .expect("wait for shutdown writer release");
                            sd_timeline_writer
                                .lock()
                                .expect("lock shutdown timeline")
                                .push("shutdown_write_exited");
                            Ok(())
                        }),
                    )
                    .await;
            });

            sd_entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("ephemeral writer entered payload callback");
            // Shut down the originating runtime in the background while the
            // blocking payload callback is still paused.
            ephemeral_rt.shutdown_background();

            let observer_store: Arc<dyn GitStore> = if is_redb {
                Arc::clone(&backends[1])
            } else {
                Arc::clone(&backends[0])
            };
            let oblit_store = Arc::clone(&observer_store);
            let sd_oid_oblit = sd_oid.clone();
            let sd_timeline_oblit = Arc::clone(&sd_timeline);
            let mut sd_oblit_task = tokio::spawn(async move {
                oblit_store
                    .begin_object_obliteration(&ObliterationRequest {
                        oid: sd_oid_oblit,
                        hash_kind: ObjectHashKind::Sha1,
                        reason: Some("runtime-shutdown-fence".to_string()),
                        actor: None,
                        approval_source: None,
                        recorded_at: "2026-11-22T18:33:00Z".to_string(),
                    })
                    .await
                    .expect("obliteration commits after paused writer exits");
                sd_timeline_oblit
                    .lock()
                    .expect("lock shutdown timeline")
                    .push("shutdown_oblit_done");
            });

            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(120), &mut sd_oblit_task,)
                    .await
                    .is_err(),
                "obliteration must stay blocked across runtime shutdown until payload callback exits (is_redb={is_redb})"
            );

            sd_release_tx
                .send(())
                .expect("release paused writer after runtime shutdown");
            sd_oblit_task
                .await
                .expect("join runtime-shutdown oblit task");
            assert_eq!(
                *sd_timeline.lock().expect("lock shutdown timeline"),
                vec![
                    "shutdown_write_entered",
                    "shutdown_write_exited",
                    "shutdown_oblit_done",
                ],
                "fence must remain held through runtime shutdown until payload exits (is_redb={is_redb})"
            );
            assert_eq!(
                observer_store
                    .get_object_index_preserved_address(repo_a, &sd_oid)
                    .await
                    .expect("abandoned writer row must not commit"),
                None
            );
            assert!(
                observer_store
                    .is_object_obliterated(&sd_oid)
                    .await
                    .expect("tombstone persisted after abandoned writer exited")
            );
        }

        // ---------------------------------------------------------------------
        // RB-04 -> RB-04b backward-compatible reopening & safe upgrade test on
        // RedbGitStore:
        // 1. A 5-table RB-04 store opens cleanly read-only (serving refs/reflog
        //    and empty RB-04b reads without mutating disk).
        // 2. A corrupt 6-table partial store (object_index without
        //    object_obliteration) fails closed on BOTH read-only & writable open.
        // 3. Writable open of the 5-table RB-04 store safely upgrades to all 12
        //    tables while preserving seeded refs/reflogs and enabling RB-04b writes.
        // ---------------------------------------------------------------------
        let legacy_path = dir.path().join("legacy_rb04.redb");
        let legacy_seed_oid = sha1_oid(888);
        RedbGitStore::create_legacy_rb04_five_table_store_for_test(
            &legacy_path,
            &legacy_seed_oid,
            false,
        )
        .expect("create legacy 5-table RB-04 store");

        let legacy_ro = RedbGitStore::open_read_only(&legacy_path)
            .expect("legacy 5-table RB-04 store must open read-only");
        let main_key = RefKey::LocalBranch {
            name: "main".to_string(),
        };
        assert_eq!(
            legacy_ro.get_ref(&main_key).await.expect("read legacy ref"),
            Some(RefTarget::Direct(legacy_seed_oid.clone()))
        );
        assert_eq!(
            legacy_ro
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("read legacy reflog")
                .len(),
            1
        );
        // Uninitialized RB-04b tables answer cleanly as empty on read-only open.
        assert_eq!(
            legacy_ro
                .get_object_index(repo_a, &legacy_seed_oid)
                .await
                .expect("legacy ro object_index"),
            None
        );
        assert!(
            legacy_ro
                .list_object_index_by_repo(repo_a, None)
                .await
                .expect("legacy ro list object_index")
                .is_empty()
        );
        assert!(
            legacy_ro
                .list_object_index_by_pack("pack-001", None)
                .await
                .expect("legacy ro list pack")
                .is_empty()
        );
        assert_eq!(
            legacy_ro
                .get_object_obliteration(&legacy_seed_oid)
                .await
                .expect("legacy ro oblit"),
            None
        );
        assert!(
            legacy_ro
                .list_incomplete_obliterations()
                .await
                .expect("legacy ro incomplete")
                .is_empty()
        );
        assert!(
            legacy_ro
                .list_obliterated_oids()
                .await
                .expect("legacy ro oblit oids")
                .is_empty()
        );
        assert_eq!(
            legacy_ro
                .get_note("refs/notes/commits", &legacy_seed_oid)
                .await
                .expect("legacy ro get note"),
            None
        );
        assert!(
            legacy_ro
                .list_notes("refs/notes/commits")
                .await
                .expect("legacy ro list notes")
                .is_empty()
        );
        assert_eq!(
            legacy_ro
                .get_revision_ordinal_meta("refs/heads/main")
                .await
                .expect("legacy ro ordinal meta"),
            None
        );
        assert!(
            legacy_ro
                .list_revision_ordinal_metas()
                .await
                .expect("legacy ro list metas")
                .is_empty()
        );
        assert_eq!(
            legacy_ro
                .find_revision_oid_by_ordinal("refs/heads/main", 1)
                .await
                .expect("legacy ro find by ord"),
            None
        );
        assert_eq!(
            legacy_ro
                .find_revision_ordinal_by_oid("refs/heads/main", &legacy_seed_oid)
                .await
                .expect("legacy ro find by oid"),
            None
        );
        assert!(
            legacy_ro
                .list_revision_ordinals("refs/heads/main")
                .await
                .expect("legacy ro list ordinals")
                .is_empty()
        );
        assert_eq!(
            legacy_ro
                .count_revision_ordinals("refs/heads/main")
                .await
                .expect("legacy ro count ordinals"),
            0
        );
        drop(legacy_ro);

        // Corrupt partial-schema store (object_index present without object_obliteration)
        // must fail closed on BOTH read-only and writable open.
        let corrupt_partial_path = dir.path().join("corrupt_partial_rb04b.redb");
        RedbGitStore::create_legacy_rb04_five_table_store_for_test(
            &corrupt_partial_path,
            &legacy_seed_oid,
            true,
        )
        .expect("create corrupt partial 6-table store");
        assert!(
            RedbGitStore::open_read_only(&corrupt_partial_path).is_err(),
            "partial RB-04b schema missing object_obliteration must fail closed read-only"
        );
        assert!(
            RedbGitStore::open_read_write(&corrupt_partial_path).is_err(),
            "partial RB-04b schema missing object_obliteration must fail closed read-write"
        );

        // Safe writable upgrade of the valid 5-table RB-04 store to all 12 tables.
        let upgraded_rw = RedbGitStore::open_read_write(&legacy_path)
            .expect("writable open must safely upgrade 5-table RB-04 store");
        assert_eq!(
            upgraded_rw
                .get_ref(&main_key)
                .await
                .expect("preserved ref after upgrade"),
            Some(RefTarget::Direct(legacy_seed_oid.clone()))
        );
        assert_eq!(
            upgraded_rw
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("preserved reflog after upgrade")
                .len(),
            1
        );
        upgraded_rw
            .upsert_object_index_batch(&[ObjectIndexRecord {
                repo_id: repo_a.to_string(),
                o_id: legacy_seed_oid.clone(),
                o_type: "commit".to_string(),
                o_size: 256,
                created_at: 1_795_376_000,
                is_synced: false,
            }])
            .await
            .expect("RB-04b write succeeds on upgraded store");
        assert_eq!(
            upgraded_rw
                .put_note(
                    "refs/notes/commits",
                    &legacy_seed_oid,
                    &sha1_oid(889),
                    false
                )
                .await
                .expect("notes write succeeds on upgraded store"),
            NoteWriteOutcome::Inserted
        );
        drop(upgraded_rw);

        let upgraded_ro =
            RedbGitStore::open_read_only(&legacy_path).expect("reopen upgraded store read-only");
        assert!(
            upgraded_ro
                .get_object_index(repo_a, &legacy_seed_oid)
                .await
                .expect("read upgraded object_index")
                .is_some()
        );
    }

    /// `RB-04b` single-feature SQLite regression smoke test covering
    /// `object_index` batch upsert, `object_obliteration` read/write fences,
    /// `notes`, and `revision_ordinal` + `revision_ordinal_meta` without
    /// requiring `storage-redb`.
    #[cfg(feature = "storage-sqlite")]
    #[tokio::test]
    async fn sqlite_object_index_and_obliteration_smoke() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create sqlite RB-04b smoke tempdir");
        let store = SqliteGitStore::create(dir.path().join("libra.db"))
            .await
            .expect("create SqliteGitStore");
        assert_eq!(store.backend(), StorageBackendKind::Sqlite);

        let repo_id = "sqlite-smoke-repo";
        let records: Vec<ObjectIndexRecord> = (1u64..=1_000)
            .map(|n| ObjectIndexRecord {
                repo_id: repo_id.to_string(),
                o_id: sha1_oid(n),
                o_type: "blob".to_string(),
                o_size: i64::try_from(n).expect("fits i64"),
                created_at: 1_795_380_000,
                is_synced: false,
            })
            .collect();
        store
            .upsert_object_index_batch(&records)
            .await
            .expect("upsert 1,000 rows");
        store
            .upsert_object_index_batch(&records)
            .await
            .expect("idempotent re-upsert");
        assert_eq!(
            store
                .list_object_index_by_repo(repo_id, None)
                .await
                .expect("list 1,000 rows")
                .len(),
            1_000
        );

        // Obliteration fence blocks both read and write paths.
        let target = sha1_oid(42);
        store
            .begin_object_obliteration(&ObliterationRequest {
                oid: target.clone(),
                hash_kind: ObjectHashKind::Sha1,
                reason: Some("smoke".to_string()),
                actor: None,
                approval_source: None,
                recorded_at: "2026-11-22T18:20:00Z".to_string(),
            })
            .await
            .expect("begin obliteration");
        assert!(is_obliteration_fenced_error(
            &store
                .get_object_index(repo_id, &target)
                .await
                .expect_err("read fenced")
        ));
        assert!(is_obliteration_fenced_error(
            &store
                .upsert_object_index_batch(&[ObjectIndexRecord {
                    repo_id: repo_id.to_string(),
                    o_id: target.clone(),
                    o_type: "blob".to_string(),
                    o_size: 1,
                    created_at: 1_795_380_001,
                    is_synced: false,
                }])
                .await
                .expect_err("write fenced")
        ));
        store
            .mark_object_obliterated(&target, ObjectHashKind::Sha1, "2026-11-22T18:21:00Z")
            .await
            .expect("mark obliterated");
        let read_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let read_called_clone = std::sync::Arc::clone(&read_called);
        assert!(is_obliteration_fenced_error(
            &store
                .run_fenced_object_read(
                    &target,
                    Box::new(move || {
                        read_called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(b"should-not-run".to_vec())
                    }),
                )
                .await
                .expect_err("fenced read blocked")
        ));
        assert!(!read_called.load(std::sync::atomic::Ordering::SeqCst));

        let write_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let write_called_clone = std::sync::Arc::clone(&write_called);
        assert!(is_obliteration_fenced_error(
            &store
                .run_fenced_object_write(
                    &ObjectIndexRecord {
                        repo_id: repo_id.to_string(),
                        o_id: target.clone(),
                        o_type: "blob".to_string(),
                        o_size: 1,
                        created_at: 1_795_380_002,
                        is_synced: false,
                    },
                    Box::new(move || {
                        write_called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(())
                    }),
                )
                .await
                .expect_err("fenced write blocked")
        ));
        assert!(!write_called.load(std::sync::atomic::Ordering::SeqCst));

        // Notes & revision_ordinal smoke (including all-zero SHA-256 OID).
        let zero256 = "0".repeat(64);
        assert_eq!(
            store
                .put_note("refs/notes/commits", &sha1_oid(1), &sha1_oid(2), false)
                .await
                .expect("put note"),
            NoteWriteOutcome::Inserted
        );
        assert_eq!(
            store
                .list_notes("refs/notes/commits")
                .await
                .expect("list notes")
                .len(),
            1
        );
        let meta = RevisionOrdinalMeta {
            ref_name: "refs/heads/main".to_string(),
            tip_oid: zero256.clone(),
            replace_sig: String::new(),
            max_ordinal: 1,
            built_at: "2026-11-22T18:22:00Z".to_string(),
        };
        store
            .replace_revision_ordinals(&meta, &[(1, zero256.clone())])
            .await
            .expect("replace ordinals");
        assert_eq!(
            store
                .find_revision_oid_by_ordinal("refs/heads/main", 1)
                .await
                .expect("forward lookup"),
            Some(zero256.clone())
        );
        assert_eq!(
            store
                .find_revision_ordinal_by_oid("refs/heads/main", &zero256)
                .await
                .expect("reverse lookup"),
            Some(1)
        );
    }
}
