//! `redb 5.0` implementation of [`GitStore`] (`RB-04`, `RB-04b`).
//!
//! Stores all 7 core repository tables (`reference`, `reflog`, `object_index`,
//! `object_obliteration`, `notes`, `revision_ordinal`, `revision_ordinal_meta`),
//! same-transaction secondary indexes (`reflog_by_ref`, `object_by_pack`,
//! `revision_ordinal_by_oid`), and atomic bridge operation bindings
//! (`agent_bridge_publication`) inside `git.redb` using the versioned
//! `[schema_ver: u16_le][postcard]` envelope (`crate::storage::codec`).
//!
//! Every read and write transaction validates `META_SCHEMA` (`Repository` role,
//! format version, and `u32::MAX` handle migration tombstone fence) as step 1
//! (`ADR-RB-03` #3). Read-only handles open via [`open_redb_read_only`] (`16 MiB`
//! cache, `MultiWriter`, zero write upgrades).

use std::{
    collections::HashSet,
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
            BridgePublicationPhase, BridgePublicationRecord, GitStore, NoteBatchOutcome, NoteCasOp,
            NoteRecord, NoteWriteOutcome, ObjectHashKind, ObjectIndexRecord,
            ObjectObliterationRecord, ObliterationRequest, ObliterationState, REFLOG_HEAD_REF_NAME,
            RefCasOutcome, RefKey, RefRecord, RefTarget, ReflogEntry, ReflogWrite,
            RevisionOrdinalAppendOutcome, RevisionOrdinalMeta, is_valid_bridge_operation_id,
            obliteration_fenced_read_error, obliteration_fenced_write_error,
            should_promote_object_index_type, validate_bridge_publication_binding,
            validate_hex_oid, validate_non_empty_identifier, validate_note_cas_op,
            validate_notes_ref, validate_object_index_record, validate_obliteration_request,
            validate_oid_for_hash_kind, validate_ref_key, validate_ref_target,
            validate_reflog_writes_for_key, validate_revision_ordinal_append,
            validate_revision_ordinal_entries, validate_worktree_filter,
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
/// Current postcard codec version for secondary index markers.
pub const GIT_REFLOG_INDEX_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `agent_bridge_publication` rows.
pub const GIT_BRIDGE_PUB_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for monotonic sequence counters.
pub const GIT_SEQ_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `object_index` rows.
pub const GIT_OBJECT_INDEX_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `object_by_pack` secondary index markers.
pub const GIT_OBJECT_BY_PACK_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `object_obliteration` rows.
pub const GIT_OBJECT_OBLITERATION_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `notes` rows.
pub const GIT_NOTES_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `revision_ordinal` rows.
pub const GIT_REVISION_ORDINAL_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `revision_ordinal_by_oid` secondary index rows.
pub const GIT_REVISION_ORDINAL_BY_OID_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `revision_ordinal_meta` rows.
pub const GIT_REVISION_ORDINAL_META_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for cross-store `meta_generation` ledger (`RB-06b`).
pub const GIT_META_GENERATION_CODEC_VERSION: u16 = 1;

/// Default bounded write-lock timeout for [`RedbGitStore`] (`30s`).
pub const DEFAULT_GIT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

const REFERENCE_TABLE_NAME: &str = "reference";
const REFLOG_TABLE_NAME: &str = "reflog";
const REFLOG_BY_REF_INDEX_NAME: &str = "reflog_by_ref";
const BRIDGE_PUBLICATION_TABLE_NAME: &str = "agent_bridge_publication";
const GIT_SEQ_TABLE_NAME: &str = "git_sequences";
const OBJECT_INDEX_TABLE_NAME: &str = "object_index";
const OBJECT_BY_PACK_INDEX_NAME: &str = "object_by_pack";
const OBJECT_OBLITERATION_TABLE_NAME: &str = "object_obliteration";
const NOTES_TABLE_NAME: &str = "notes";
const REVISION_ORDINAL_TABLE_NAME: &str = "revision_ordinal";
const REVISION_ORDINAL_BY_OID_INDEX_NAME: &str = "revision_ordinal_by_oid";
const REVISION_ORDINAL_META_TABLE_NAME: &str = "revision_ordinal_meta";
const META_GENERATION_TABLE_NAME: &str = "meta_generation";
const META_CROSS_STORE_INTENT_TABLE_NAME: &str = "meta_cross_store_intent";

const SEQ_KEY_REFLOG: &str = "reflog";
const META_GENERATION_SINGLETON_KEY: &str = "singleton";

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

/// Primary table for `object_index` (`(repo_id, o_id) -> versioned ObjectIndexStoredRow`).
pub const OBJECT_INDEX_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new(OBJECT_INDEX_TABLE_NAME);

/// Same-transaction secondary index `OBJECT_BY_PACK` keyed by `(pack_id, repo_id, o_id)` (`RB-04b`).
pub const OBJECT_BY_PACK_TABLE: TableDefinition<'static, (&str, &str, &str), &[u8]> =
    TableDefinition::new(OBJECT_BY_PACK_INDEX_NAME);

/// Primary table for permanent compliance tombstones
/// (`(oid, hash_kind) -> versioned ObjectObliterationStoredRow`).
pub const OBJECT_OBLITERATION_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new(OBJECT_OBLITERATION_TABLE_NAME);

/// Primary table for `notes` (`(notes_ref, object) -> versioned NoteStoredRow`).
pub const NOTES_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new(NOTES_TABLE_NAME);

/// Primary table for `revision_ordinal` (`(ref_name, ordinal) -> versioned RevisionOrdinalStoredRow`).
pub const REVISION_ORDINAL_TABLE: TableDefinition<'static, (&str, i64), &[u8]> =
    TableDefinition::new(REVISION_ORDINAL_TABLE_NAME);

/// Same-transaction reverse index for `revision_ordinal` (`(ref_name, oid) -> versioned ordinal`).
pub const REVISION_ORDINAL_BY_OID_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new(REVISION_ORDINAL_BY_OID_INDEX_NAME);

/// Primary table for `revision_ordinal_meta` (`ref_name -> versioned RevisionOrdinalMetaStoredRow`).
pub const REVISION_ORDINAL_META_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new(REVISION_ORDINAL_META_TABLE_NAME);

/// Cross-store monotonic generation ledger (`ADR-RB-06`, `RB-06b`).
///
/// Created lazily inside the first cross-store write transaction so freshly
/// created `git.redb` files keep exact 61-table repository manifest parity and
/// pre-`RB-06b` stores open read-only with generation `0`.
pub const META_GENERATION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new(META_GENERATION_TABLE_NAME);

/// Cross-store durable intent outbox keyed by monotonic `generation: u64`
/// (`ADR-RB-06`, `RB-06b`).
pub const META_CROSS_STORE_INTENT_TABLE: TableDefinition<'static, u64, &[u8]> =
    TableDefinition::new(META_CROSS_STORE_INTENT_TABLE_NAME);

static GIT_STORE_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct GitMetaGenerationValue {
    cross_store_gen: u64,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ObjectIndexStoredRow {
    o_type: String,
    o_size: i64,
    created_at: i64,
    is_synced: bool,
    pack_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ObjectByPackMarker;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ObjectObliterationStoredRow {
    state: ObliterationState,
    reason: Option<String>,
    actor: Option<String>,
    approval_source: Option<String>,
    requested_at: String,
    tombstone_written_at: String,
    payload_deleted_at: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NoteStoredRow {
    blob: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RevisionOrdinalStoredRow {
    oid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct RevisionOrdinalByOidStoredRow {
    ordinal: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RevisionOrdinalMetaStoredRow {
    tip_oid: String,
    replace_sig: String,
    max_ordinal: i64,
    built_at: String,
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

fn decode_object_index_row(bytes: &[u8]) -> StorageResult<ObjectIndexStoredRow> {
    decode_versioned(bytes, GIT_OBJECT_INDEX_CODEC_VERSION)
}

fn object_index_record_from_stored(
    repo_id: String,
    o_id: String,
    row: ObjectIndexStoredRow,
) -> ObjectIndexRecord {
    ObjectIndexRecord {
        repo_id,
        o_id,
        o_type: row.o_type,
        o_size: row.o_size,
        created_at: row.created_at,
        is_synced: row.is_synced,
    }
}

fn decode_obliteration_row(
    oid: String,
    hash_kind_str: &str,
    bytes: &[u8],
) -> StorageResult<ObjectObliterationRecord> {
    let row: ObjectObliterationStoredRow =
        decode_versioned(bytes, GIT_OBJECT_OBLITERATION_CODEC_VERSION)?;
    Ok(ObjectObliterationRecord {
        oid,
        hash_kind: ObjectHashKind::parse(hash_kind_str)?,
        state: row.state,
        reason: row.reason,
        actor: row.actor,
        approval_source: row.approval_source,
        requested_at: row.requested_at,
        tombstone_written_at: row.tombstone_written_at,
        payload_deleted_at: row.payload_deleted_at,
        updated_at: row.updated_at,
    })
}

fn decode_note_row(bytes: &[u8]) -> StorageResult<String> {
    let row: NoteStoredRow = decode_versioned(bytes, GIT_NOTES_CODEC_VERSION)?;
    Ok(row.blob)
}

fn decode_revision_ordinal_row(bytes: &[u8]) -> StorageResult<String> {
    let row: RevisionOrdinalStoredRow =
        decode_versioned(bytes, GIT_REVISION_ORDINAL_CODEC_VERSION)?;
    Ok(row.oid)
}

fn decode_revision_ordinal_by_oid(bytes: &[u8]) -> StorageResult<i64> {
    let row: RevisionOrdinalByOidStoredRow =
        decode_versioned(bytes, GIT_REVISION_ORDINAL_BY_OID_CODEC_VERSION)?;
    Ok(row.ordinal)
}

fn decode_revision_ordinal_meta(
    ref_name: String,
    bytes: &[u8],
) -> StorageResult<RevisionOrdinalMeta> {
    let row: RevisionOrdinalMetaStoredRow =
        decode_versioned(bytes, GIT_REVISION_ORDINAL_META_CODEC_VERSION)?;
    Ok(RevisionOrdinalMeta {
        ref_name,
        tip_oid: row.tip_oid,
        replace_sig: row.replace_sig,
        max_ordinal: row.max_ordinal,
        built_at: row.built_at,
    })
}

fn lookup_obliteration_in_table<T: ReadableTable<(&'static str, &'static str), &'static [u8]>>(
    table: &T,
    oid: &str,
) -> StorageResult<Option<ObjectObliterationRecord>> {
    let mut range = table
        .range((oid, "")..)
        .map_err(|err| map_redb_storage_err(err, "range scan object_obliteration by oid"))?;
    if let Some(entry) = range.next() {
        let (k, v) =
            entry.map_err(|err| map_redb_storage_err(err, "read object_obliteration entry"))?;
        let (found_oid, found_kind) = k.value();
        if found_oid == oid {
            return decode_obliteration_row(found_oid.to_string(), found_kind, v.value()).map(Some);
        }
    }
    Ok(None)
}

fn initialize_rb04b_git_tables(wtx: &WriteTransaction) -> StorageResult<()> {
    let _ = wtx
        .open_table(OBJECT_INDEX_TABLE)
        .map_err(|err| map_redb_table_err(err, "init object_index table"))?;
    let _ = wtx
        .open_table(OBJECT_BY_PACK_TABLE)
        .map_err(|err| map_redb_table_err(err, "init object_by_pack index"))?;
    let _ = wtx
        .open_table(OBJECT_OBLITERATION_TABLE)
        .map_err(|err| map_redb_table_err(err, "init object_obliteration table"))?;
    let _ = wtx
        .open_table(NOTES_TABLE)
        .map_err(|err| map_redb_table_err(err, "init notes table"))?;
    let _ = wtx
        .open_table(REVISION_ORDINAL_TABLE)
        .map_err(|err| map_redb_table_err(err, "init revision_ordinal table"))?;
    let _ = wtx
        .open_table(REVISION_ORDINAL_BY_OID_TABLE)
        .map_err(|err| map_redb_table_err(err, "init revision_ordinal_by_oid index"))?;
    let _ = wtx
        .open_table(REVISION_ORDINAL_META_TABLE)
        .map_err(|err| map_redb_table_err(err, "init revision_ordinal_meta table"))?;
    Ok(())
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
    drop(seq);
    initialize_rb04b_git_tables(wtx)
}

fn required_base_table_names() -> [&'static str; 5] {
    [
        REFERENCE_TABLE_NAME,
        REFLOG_TABLE_NAME,
        REFLOG_BY_REF_INDEX_NAME,
        BRIDGE_PUBLICATION_TABLE_NAME,
        GIT_SEQ_TABLE_NAME,
    ]
}

fn rb04b_extended_table_names() -> [&'static str; 7] {
    [
        OBJECT_INDEX_TABLE_NAME,
        OBJECT_BY_PACK_INDEX_NAME,
        OBJECT_OBLITERATION_TABLE_NAME,
        NOTES_TABLE_NAME,
        REVISION_ORDINAL_TABLE_NAME,
        REVISION_ORDINAL_BY_OID_INDEX_NAME,
        REVISION_ORDINAL_META_TABLE_NAME,
    ]
}

/// Inspect existing table names against the RB-04 (5 base tables) and RB-04b
/// (+7 extended tables) schema generations.
///
/// Returns `Ok(true)` if all 12 tables exist, `Ok(false)` if this is a clean
/// legacy RB-04 store with only the 5 base tables, or `Err` if any required
/// base table is missing or only a corrupt partial subset of RB-04b tables exists.
fn classify_table_layout(existing: &[String]) -> StorageResult<bool> {
    for required in required_base_table_names() {
        if !existing.iter().any(|name| name == required) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!("redb git store is missing required table `{required}`"),
            });
        }
    }
    let extended = rb04b_extended_table_names();
    let present_extended = extended
        .iter()
        .filter(|name| existing.iter().any(|e| e == *name))
        .count();
    if present_extended == extended.len() {
        Ok(true)
    } else if present_extended == 0 {
        Ok(false)
    } else {
        let missing = extended
            .iter()
            .find(|name| !existing.iter().any(|e| e == *name))
            .copied()
            .unwrap_or("unknown");
        Err(StorageBackendError::BackendInternal {
            detail: format!(
                "redb git store has incomplete RB-04b schema; missing table `{missing}`"
            ),
        })
    }
}

fn verify_required_tables_read(rtx: &ReadTransaction) -> StorageResult<bool> {
    let existing: Vec<String> = rtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list redb git store tables"))?
        .map(|handle| handle.name().to_string())
        .collect();
    classify_table_layout(&existing)
}

fn verify_required_tables_write(wtx: &WriteTransaction) -> StorageResult<()> {
    let existing: Vec<String> = wtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list redb git store tables for write"))?
        .map(|handle| handle.name().to_string())
        .collect();
    if !classify_table_layout(&existing)? {
        return Err(StorageBackendError::BackendInternal {
            detail: "redb git store requires RB-04b schema upgrade before write".to_string(),
        });
    }
    Ok(())
}

fn upgrade_rb04_tables_if_needed(db: &Database, timeout: Duration) -> StorageResult<()> {
    let rtx = begin_checked_read(db, DatabaseRole::Repository)?;
    let has_extended = verify_required_tables_read(&rtx)?;
    drop(rtx);
    if has_extended {
        return Ok(());
    }
    let wtx = begin_checked_write_blocking(db, DatabaseRole::Repository, timeout)?;
    let existing: Vec<String> = wtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list redb git store tables for upgrade"))?
        .map(|handle| handle.name().to_string())
        .collect();
    if !classify_table_layout(&existing)? {
        initialize_rb04b_git_tables(&wtx)?;
        wtx.commit()
            .map_err(|err| map_redb_commit_err(err, "commit RB-04b redb git schema upgrade"))?;
    }
    Ok(())
}

fn open_optional_read_table<K: redb::Key + 'static, V: redb::Value + 'static>(
    rtx: &ReadTransaction,
    definition: TableDefinition<'_, K, V>,
    context: &'static str,
) -> StorageResult<Option<redb::ReadOnlyTable<K, V>>> {
    match rtx.open_table(definition) {
        Ok(table) => Ok(Some(table)),
        Err(redb::TableError::TableDoesNotExist(_)) => Ok(None),
        Err(err) => Err(map_redb_table_err(err, context)),
    }
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
    ///
    /// Clean RB-04 stores containing only the original 5 core tables are safely
    /// upgraded in one transaction to initialize the 7 RB-04b tables without
    /// touching existing reference/reflog/bridge data.
    pub fn open_read_write_with_context(
        path: impl AsRef<Path>,
        context: RedbRepairContext,
    ) -> StorageResult<Self> {
        let db = open_redb_writable(path, DatabaseRole::Repository, context)?;
        upgrade_rb04_tables_if_needed(&db, DEFAULT_GIT_WRITE_TIMEOUT)?;
        Ok(Self {
            handle: RedbGitHandle::ReadWrite(Arc::new(db)),
            write_timeout: DEFAULT_GIT_WRITE_TIMEOUT,
        })
    }

    /// Open an existing `git.redb` store strictly read-only (`open_read_only`,
    /// `16 MiB` cache, `MultiWriter`, zero write upgrades).
    ///
    /// Accepts both full 12-table RB-04b stores and legacy 5-table RB-04 stores
    /// without mutating disk; fails closed if base tables are missing or only a
    /// partial corrupt subset of RB-04b tables exists.
    pub fn open_read_only(path: impl AsRef<Path>) -> StorageResult<Self> {
        let db = open_redb_read_only(path, DatabaseRole::Repository)?;
        let rtx = begin_checked_read(&db, DatabaseRole::Repository)?;
        let _ = verify_required_tables_read(&rtx)?;
        drop(rtx);
        Ok(Self {
            handle: RedbGitHandle::ReadOnly(Arc::new(db)),
            write_timeout: DEFAULT_GIT_WRITE_TIMEOUT,
        })
    }

    /// Wrap an already-opened writable `Arc<Database>` whose `META_SCHEMA` and
    /// core Git tables are initialized for [`DatabaseRole::Repository`].
    pub fn from_database(db: Arc<Database>) -> StorageResult<Self> {
        upgrade_rb04_tables_if_needed(db.as_ref(), DEFAULT_GIT_WRITE_TIMEOUT)?;
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

    /// Begin a `META_SCHEMA`-verified read transaction on this `git.redb` handle
    /// without upgrading read-only handles (`RB-06b` cross-store snapshots).
    pub(crate) fn begin_checked_read_tx(&self) -> StorageResult<ReadTransaction> {
        let rtx = match &self.handle {
            RedbGitHandle::ReadWrite(db) => {
                begin_checked_read(db.as_ref(), DatabaseRole::Repository)?
            }
            RedbGitHandle::ReadOnly(db) => {
                begin_checked_read(db.as_ref(), DatabaseRole::Repository)?
            }
        };
        let _ = verify_required_tables_read(&rtx)?;
        Ok(rtx)
    }

    /// Begin a `META_SCHEMA`-verified bounded write transaction on this
    /// `git.redb` handle (`RB-06b` cross-store coordinator).
    pub(crate) fn begin_checked_write_tx(&self) -> StorageResult<WriteTransaction> {
        let db = self.writable_db()?;
        let wtx = begin_checked_write_blocking(&db, DatabaseRole::Repository, self.write_timeout)?;
        verify_required_tables_write(&wtx)?;
        Ok(wtx)
    }

    /// Read a reference inside an existing read transaction (used by frozen
    /// cross-store consistent snapshots).
    pub(crate) fn read_ref_in_rtx(
        rtx: &ReadTransaction,
        key: &RefKey,
    ) -> StorageResult<Option<RefTarget>> {
        validate_ref_key(key)?;
        let ref_table = rtx
            .open_table(REFERENCE_TABLE)
            .map_err(|err| map_redb_table_err(err, "open reference table for snapshot read"))?;
        match ref_table
            .get(encode_ref_key(key))
            .map_err(|err| map_redb_storage_err(err, "read reference in snapshot"))?
        {
            Some(guard) => Ok(Some(decode_ref_target(guard.value())?)),
            None => Ok(None),
        }
    }

    /// Read the persisted cross-store `META_GENERATION` inside a read transaction.
    ///
    /// Returns `0` only when neither `meta_generation` nor `meta_cross_store_intent`
    /// has been initialized yet (preserving fresh 61-table repository parity and
    /// pre-`RB-06b` read-only compatibility). Fails closed if `meta_generation`
    /// exists without its singleton entry or if `meta_cross_store_intent` exists
    /// without `meta_generation`.
    pub(crate) fn read_cross_store_generation_in_rtx(rtx: &ReadTransaction) -> StorageResult<u64> {
        let Some(table) =
            open_optional_read_table(rtx, META_GENERATION_TABLE, "open git meta_generation table")?
        else {
            if open_optional_read_table(
                rtx,
                META_CROSS_STORE_INTENT_TABLE,
                "check git meta_cross_store_intent consistency",
            )?
            .is_some()
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: "git redb database has meta_cross_store_intent without meta_generation"
                        .to_string(),
                });
            }
            return Ok(0);
        };
        match table
            .get(META_GENERATION_SINGLETON_KEY)
            .map_err(|err| map_redb_storage_err(err, "read git meta_generation"))?
        {
            Some(guard) => {
                let decoded: GitMetaGenerationValue =
                    decode_versioned(guard.value(), GIT_META_GENERATION_CODEC_VERSION)?;
                Ok(decoded.cross_store_gen)
            }
            None => Err(StorageBackendError::BackendInternal {
                detail: "git redb database is missing required meta_generation singleton"
                    .to_string(),
            }),
        }
    }

    /// Read the persisted cross-store `META_GENERATION` inside a write transaction
    /// without eagerly creating `meta_generation` on an uninitialized store.
    pub(crate) fn read_cross_store_generation_in_wtx(wtx: &WriteTransaction) -> StorageResult<u64> {
        let mut has_meta_gen = false;
        let mut has_intent_table = false;
        for handle in wtx
            .list_tables()
            .map_err(|err| map_redb_storage_err(err, "list git tables for meta_generation check"))?
        {
            let name = handle.name();
            if name == META_GENERATION_TABLE_NAME {
                has_meta_gen = true;
            } else if name == META_CROSS_STORE_INTENT_TABLE_NAME {
                has_intent_table = true;
            }
        }
        if !has_meta_gen {
            if has_intent_table {
                return Err(StorageBackendError::BackendInternal {
                    detail: "git redb database has meta_cross_store_intent without meta_generation"
                        .to_string(),
                });
            }
            return Ok(0);
        }
        let table = wtx
            .open_table(META_GENERATION_TABLE)
            .map_err(|err| map_redb_table_err(err, "open git meta_generation for write"))?;
        match table
            .get(META_GENERATION_SINGLETON_KEY)
            .map_err(|err| map_redb_storage_err(err, "read git meta_generation in write tx"))?
        {
            Some(guard) => {
                let decoded: GitMetaGenerationValue =
                    decode_versioned(guard.value(), GIT_META_GENERATION_CODEC_VERSION)?;
                Ok(decoded.cross_store_gen)
            }
            None => Err(StorageBackendError::BackendInternal {
                detail: "git redb database is missing required meta_generation singleton"
                    .to_string(),
            }),
        }
    }

    /// Persist `META_GENERATION = generation` inside an active `git.redb` write transaction.
    pub(crate) fn write_cross_store_generation_in_wtx(
        wtx: &WriteTransaction,
        generation: u64,
    ) -> StorageResult<()> {
        let mut table = wtx
            .open_table(META_GENERATION_TABLE)
            .map_err(|err| map_redb_table_err(err, "open git meta_generation for write"))?;
        let encoded = encode_versioned(
            GIT_META_GENERATION_CODEC_VERSION,
            &GitMetaGenerationValue {
                cross_store_gen: generation,
            },
        )?;
        table
            .insert(META_GENERATION_SINGLETON_KEY, encoded.as_slice())
            .map_err(|err| map_redb_storage_err(err, "write git meta_generation"))?;
        Ok(())
    }

    /// Read all pending cross-store intent entries strictly greater than
    /// `after_gen` in strictly ascending generation order inside a write transaction.
    pub(crate) fn read_pending_cross_store_intents_in_wtx(
        wtx: &WriteTransaction,
        after_gen: u64,
    ) -> StorageResult<Vec<(u64, Vec<u8>)>> {
        let Some(start) = after_gen.checked_add(1) else {
            return Ok(Vec::new());
        };
        let has_intent_table = wtx
            .list_tables()
            .map_err(|err| {
                map_redb_storage_err(err, "list git tables for meta_cross_store_intent check")
            })?
            .any(|h| h.name() == META_CROSS_STORE_INTENT_TABLE_NAME);
        if !has_intent_table {
            return Ok(Vec::new());
        }
        let table = wtx
            .open_table(META_CROSS_STORE_INTENT_TABLE)
            .map_err(|err| map_redb_table_err(err, "open meta_cross_store_intent for read"))?;
        let mut out = Vec::new();
        for entry in table
            .range(start..)
            .map_err(|err| map_redb_storage_err(err, "range meta_cross_store_intent"))?
        {
            let (k, v) =
                entry.map_err(|err| map_redb_storage_err(err, "iter meta_cross_store_intent"))?;
            out.push((k.value(), v.value().to_vec()));
        }
        Ok(out)
    }

    /// Verify that `META_CROSS_STORE_INTENT` contains no orphan entries with
    /// generation strictly greater than `git_gen`.
    pub(crate) fn ensure_no_orphan_cross_store_intents_in_wtx(
        wtx: &WriteTransaction,
        git_gen: u64,
    ) -> StorageResult<()> {
        let Some(start) = git_gen.checked_add(1) else {
            return Ok(());
        };
        let has_intent_table = wtx
            .list_tables()
            .map_err(|err| {
                map_redb_storage_err(err, "list git tables for orphan cross-store intent check")
            })?
            .any(|h| h.name() == META_CROSS_STORE_INTENT_TABLE_NAME);
        if !has_intent_table {
            return Ok(());
        }
        let table = wtx
            .open_table(META_CROSS_STORE_INTENT_TABLE)
            .map_err(|err| {
                map_redb_table_err(err, "open meta_cross_store_intent for orphan check")
            })?;
        if let Some(entry) = table
            .range(start..)
            .map_err(|err| map_redb_storage_err(err, "range orphan meta_cross_store_intent"))?
            .next()
        {
            let (k, _) = entry
                .map_err(|err| map_redb_storage_err(err, "read orphan meta_cross_store_intent"))?;
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "cross-store outbox contains orphan intent generation {} beyond git generation {git_gen}",
                    k.value()
                ),
            });
        }
        Ok(())
    }

    /// Prune all already-reconciled cross-store intent entries with generation
    /// `<= up_to_gen` inside an active `git.redb` write transaction (`ADR-RB-06`,
    /// `RB-06e`).
    ///
    /// Returns the number of entries removed without creating
    /// `META_CROSS_STORE_INTENT` on stores that have never written intents.
    pub(crate) fn prune_reconciled_cross_store_intents_in_wtx(
        wtx: &WriteTransaction,
        up_to_gen: u64,
    ) -> StorageResult<usize> {
        let has_intent_table = wtx
            .list_tables()
            .map_err(|err| {
                map_redb_storage_err(err, "list git tables for cross-store intent pruning")
            })?
            .any(|h| h.name() == META_CROSS_STORE_INTENT_TABLE_NAME);
        if !has_intent_table {
            return Ok(0);
        }
        let mut table = wtx
            .open_table(META_CROSS_STORE_INTENT_TABLE)
            .map_err(|err| map_redb_table_err(err, "open meta_cross_store_intent for prune"))?;
        let mut keys_to_prune = Vec::new();
        {
            let range = table.range(..=up_to_gen).map_err(|err| {
                map_redb_storage_err(err, "range meta_cross_store_intent for prune")
            })?;
            for entry in range {
                let (k, _) = entry.map_err(|err| {
                    map_redb_storage_err(err, "read meta_cross_store_intent key for prune")
                })?;
                keys_to_prune.push(k.value());
            }
        }
        let mut pruned = 0usize;
        for key in keys_to_prune {
            if table
                .remove(key)
                .map_err(|err| map_redb_storage_err(err, "remove pruned meta_cross_store_intent"))?
                .is_some()
            {
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// Persist a serialized cross-store intent payload under `META_CROSS_STORE_INTENT[generation]`
    /// inside an active `git.redb` write transaction.
    pub(crate) fn write_cross_store_intent_in_wtx(
        wtx: &WriteTransaction,
        generation: u64,
        encoded_intent: &[u8],
    ) -> StorageResult<()> {
        let mut table = wtx
            .open_table(META_CROSS_STORE_INTENT_TABLE)
            .map_err(|err| map_redb_table_err(err, "open meta_cross_store_intent for write"))?;
        if table
            .get(generation)
            .map_err(|err| map_redb_storage_err(err, "check existing meta_cross_store_intent"))?
            .is_some()
        {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "cross-store intent generation {generation} already exists; refusing to overwrite outbox entry"
                ),
            });
        }
        table
            .insert(generation, encoded_intent)
            .map_err(|err| map_redb_storage_err(err, "insert meta_cross_store_intent"))?;
        Ok(())
    }

    /// Read the current `git.redb` cross-store generation synchronously.
    pub fn read_cross_store_generation(&self) -> StorageResult<u64> {
        let rtx = self.begin_checked_read_tx()?;
        Self::read_cross_store_generation_in_rtx(&rtx)
    }

    /// List all generation keys currently stored in `META_CROSS_STORE_INTENT`
    /// in ascending order.
    pub fn list_cross_store_intent_generations(&self) -> StorageResult<Vec<u64>> {
        let rtx = self.begin_checked_read_tx()?;
        let Some(table) = open_optional_read_table(
            &rtx,
            META_CROSS_STORE_INTENT_TABLE,
            "open meta_cross_store_intent for listing",
        )?
        else {
            return Ok(Vec::new());
        };
        let mut gens = Vec::new();
        for entry in table
            .iter()
            .map_err(|err| map_redb_storage_err(err, "iter meta_cross_store_intent"))?
        {
            let (k, _) = entry
                .map_err(|err| map_redb_storage_err(err, "read meta_cross_store_intent key"))?;
            gens.push(k.value());
        }
        Ok(gens)
    }

    /// Execute a reference CAS + optional reflog + optional `agent_bridge_publication`
    /// binding inside an existing `git.redb` [`WriteTransaction`].
    pub(crate) fn apply_ref_cas_in_wtx(
        wtx: &WriteTransaction,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
        publication: Option<&BridgePublicationRecord>,
    ) -> StorageResult<(bool, RefCasOutcome)> {
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

        let mut ref_table = wtx
            .open_table(REFERENCE_TABLE)
            .map_err(|err| map_redb_table_err(err, "open reference table for write"))?;
        let encoded_key = encode_ref_key(key);
        let actual = match ref_table
            .get(encoded_key)
            .map_err(|err| map_redb_storage_err(err, "read reference for CAS"))?
        {
            Some(guard) => Some(decode_ref_target(guard.value())?),
            None => None,
        };

        if actual.as_ref() != expected {
            return Ok((false, RefCasOutcome::CasConflict { actual }));
        }

        // Resolve symbolic `HEAD` when binding a bridge publication so switching
        // `Symbolic(branch -> X)` to `Direct(X)` cannot publish a record without
        // actually moving `HEAD` to a new commit OID.
        if let (Some(pub_rec), Some(RefTarget::Symbolic(sym_branch))) = (publication, &actual) {
            let branch_key = RefKey::LocalBranch {
                name: sym_branch.clone(),
            };
            if let Some(branch_guard) =
                ref_table.get(encode_ref_key(&branch_key)).map_err(|err| {
                    map_redb_storage_err(err, "resolve symbolic HEAD target for bridge publication")
                })?
                && let RefTarget::Direct(resolved_oid) = decode_ref_target(branch_guard.value())?
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
        if let RefKey::LocalBranch { name: branch_name } = key {
            for entry in reflogs {
                if entry.ref_name == REFLOG_HEAD_REF_NAME {
                    let head_key = RefKey::LocalHead {
                        worktree_id: entry.worktree_id.clone(),
                    };
                    let head_target =
                        match ref_table.get(encode_ref_key(&head_key)).map_err(|err| {
                            map_redb_storage_err(err, "verify LocalHead for branch HEAD reflog")
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
            if publication.is_some() {
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
        ) = (key, new_target)
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
                    detail: "branch is already checked out as symbolic HEAD in another worktree"
                        .to_string(),
                });
            }
        }

        // Apply reference mutation.
        match new_target {
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
        if !reflogs.is_empty() {
            let marker = encode_versioned(GIT_REFLOG_INDEX_CODEC_VERSION, &ReflogIndexMarker)?;
            let mut reflog_table = wtx
                .open_table(REFLOG_TABLE)
                .map_err(|err| map_redb_table_err(err, "open reflog table for write"))?;
            let mut idx_table = wtx
                .open_table(REFLOG_BY_REF_INDEX)
                .map_err(|err| map_redb_table_err(err, "open reflog_by_ref index"))?;
            for entry in reflogs {
                let id = next_reflog_id(wtx)?;
                let wt_scope = entry.worktree_id.clone().unwrap_or_default();
                let ref_name_idx = entry.ref_name.clone();
                let encoded = encode_versioned(
                    GIT_REFLOG_CODEC_VERSION,
                    &ReflogStoredRow {
                        ref_name: entry.ref_name.clone(),
                        old_oid: entry.old_oid.clone(),
                        new_oid: entry.new_oid.clone(),
                        timestamp: entry.timestamp,
                        committer_name: entry.committer_name.clone(),
                        committer_email: entry.committer_email.clone(),
                        action: entry.action.clone(),
                        message: entry.message.clone(),
                        worktree_id: entry.worktree_id.clone(),
                    },
                )?;
                reflog_table
                    .insert(id, encoded.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "insert reflog entry"))?;
                idx_table
                    .insert(
                        (ref_name_idx.as_str(), wt_scope.as_str(), id),
                        marker.as_slice(),
                    )
                    .map_err(|err| map_redb_storage_err(err, "insert reflog_by_ref index"))?;
            }
        }

        // Atomically bind `agent_bridge_publication` inside the same WriteTransaction.
        if let Some(pub_rec) = publication {
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
                    commit_oid: pub_rec.commit_oid.clone(),
                    phase: pub_rec.phase,
                },
            )?;
            pub_table
                .insert(pub_rec.operation_id.as_str(), encoded.as_slice())
                .map_err(|err| {
                    map_redb_storage_err(err, "insert agent_bridge_publication entry")
                })?;
        }

        Ok((
            true,
            RefCasOutcome::Applied {
                previous: actual,
                current: new_target.cloned(),
            },
        ))
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
            let _ = verify_required_tables_read(&rtx)?;
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
            let (applied, outcome) = Self::apply_ref_cas_in_wtx(
                wtx,
                &key_owned,
                expected_owned.as_ref(),
                new_target_owned.as_ref(),
                &reflogs_owned,
                publication_owned.as_ref(),
            )?;
            if !applied {
                return Ok((false, outcome));
            }
            if abort_before_commit_for_test {
                return Err(StorageBackendError::BackendInternal {
                    detail: "injected pre-commit abort for rollback verification".to_string(),
                });
            }
            Ok((true, outcome))
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

    /// Construct a genuine RB-04-format 5-table `git.redb` (pre-RB-04b schema)
    /// containing one seeded `main` branch and reflog row, or a corrupt 6-table
    /// partial-schema variant when `add_corrupt_partial_object_index` is `true`.
    #[cfg(all(test, feature = "storage-sqlite"))]
    pub(crate) fn create_legacy_rb04_five_table_store_for_test(
        path: impl AsRef<Path>,
        seed_oid: &str,
        add_corrupt_partial_object_index: bool,
    ) -> StorageResult<()> {
        let db = create_redb_writable(
            path.as_ref(),
            DatabaseRole::Repository,
            RedbRepairContext::WriteCommand,
        )?;
        let wtx =
            begin_write_bounded_blocking(&db, DatabaseRole::Repository, DEFAULT_GIT_WRITE_TIMEOUT)?;
        init_meta_schema(&wtx, DatabaseRole::Repository)?;

        let mut ref_table = wtx
            .open_table(REFERENCE_TABLE)
            .map_err(|err| map_redb_table_err(err, "seed legacy reference table"))?;
        let branch_key = RefKey::LocalBranch {
            name: "main".to_string(),
        };
        let target_bytes = encode_versioned(
            GIT_REF_CODEC_VERSION,
            &RefTargetStored {
                target: RefTarget::Direct(seed_oid.to_string()),
            },
        )?;
        ref_table
            .insert(encode_ref_key(&branch_key), target_bytes.as_slice())
            .map_err(|err| map_redb_storage_err(err, "insert legacy main branch"))?;
        drop(ref_table);

        let mut reflog_table = wtx
            .open_table(REFLOG_TABLE)
            .map_err(|err| map_redb_table_err(err, "seed legacy reflog table"))?;
        let reflog_bytes = encode_versioned(
            GIT_REFLOG_CODEC_VERSION,
            &ReflogStoredRow {
                ref_name: "refs/heads/main".to_string(),
                old_oid: "0".repeat(40),
                new_oid: seed_oid.to_string(),
                timestamp: 1_795_360_001,
                committer_name: "Legacy Author".to_string(),
                committer_email: "legacy@example.com".to_string(),
                action: "commit".to_string(),
                message: "rb04 legacy commit".to_string(),
                worktree_id: None,
            },
        )?;
        reflog_table
            .insert(1u64, reflog_bytes.as_slice())
            .map_err(|err| map_redb_storage_err(err, "insert legacy reflog"))?;
        drop(reflog_table);

        let mut idx_table = wtx
            .open_table(REFLOG_BY_REF_INDEX)
            .map_err(|err| map_redb_table_err(err, "seed legacy reflog index"))?;
        let marker = encode_versioned(GIT_REFLOG_INDEX_CODEC_VERSION, &ReflogIndexMarker)?;
        idx_table
            .insert(("refs/heads/main", "", 1u64), marker.as_slice())
            .map_err(|err| map_redb_storage_err(err, "insert legacy reflog index"))?;
        drop(idx_table);

        let _ = wtx
            .open_table(BRIDGE_PUBLICATION_TABLE)
            .map_err(|err| map_redb_table_err(err, "seed legacy bridge table"))?;
        let mut seq_table = wtx
            .open_table(GIT_SEQ_TABLE)
            .map_err(|err| map_redb_table_err(err, "seed legacy sequence table"))?;
        let one = encode_versioned(GIT_SEQ_CODEC_VERSION, &GitSequenceValue { last_id: 1 })?;
        seq_table
            .insert(SEQ_KEY_REFLOG, one.as_slice())
            .map_err(|err| map_redb_storage_err(err, "seed legacy reflog sequence"))?;
        drop(seq_table);

        if add_corrupt_partial_object_index {
            let _ = wtx
                .open_table(OBJECT_INDEX_TABLE)
                .map_err(|err| map_redb_table_err(err, "seed partial object_index table"))?;
        }

        wtx.commit()
            .map_err(|err| map_redb_commit_err(err, "commit legacy RB-04 fixture"))?;
        Ok(())
    }

    async fn upsert_object_index_batch_inner(
        &self,
        records: &[ObjectIndexRecord],
        pack_override: Option<Option<&str>>,
    ) -> StorageResult<()> {
        for rec in records {
            validate_object_index_record(rec)?;
        }
        if let Some(Some(pack_id)) = pack_override {
            validate_non_empty_identifier(pack_id, "object_by_pack pack_id")?;
        }
        if records.is_empty() {
            // Still enforce writable check even on empty batch.
            let _ = self.writable_db()?;
            return Ok(());
        }

        let records_owned = records.to_vec();
        let pack_override_owned = pack_override.map(|opt| opt.map(str::to_string));

        self.run_write_conditional(move |wtx| {
            // Step 1: enforce the `object_obliteration` write fence across the
            // entire batch BEFORE mutating any table so rejection is atomic.
            {
                let ob_table = wtx.open_table(OBJECT_OBLITERATION_TABLE).map_err(|err| {
                    map_redb_table_err(err, "open object_obliteration table for write fence")
                })?;
                let mut checked = HashSet::with_capacity(records_owned.len());
                for rec in &records_owned {
                    if checked.insert(rec.o_id.as_str())
                        && let Some(tomb) = lookup_obliteration_in_table(&ob_table, &rec.o_id)?
                    {
                        return Err(obliteration_fenced_write_error(tomb.state));
                    }
                }
            }

            let mut obj_table = wtx
                .open_table(OBJECT_INDEX_TABLE)
                .map_err(|err| map_redb_table_err(err, "open object_index table for write"))?;
            let mut pack_table = wtx
                .open_table(OBJECT_BY_PACK_TABLE)
                .map_err(|err| map_redb_table_err(err, "open object_by_pack index for write"))?;
            let pack_marker =
                encode_versioned(GIT_OBJECT_BY_PACK_CODEC_VERSION, &ObjectByPackMarker)?;

            for rec in records_owned {
                let existing = match obj_table
                    .get((rec.repo_id.as_str(), rec.o_id.as_str()))
                    .map_err(|err| map_redb_storage_err(err, "read object_index entry"))?
                {
                    Some(guard) => Some(decode_object_index_row(guard.value())?),
                    None => None,
                };

                let (old_pack_id, next_row) = match existing {
                    Some(prev) => {
                        let old_pack = prev.pack_id.clone();
                        let next_pack = match &pack_override_owned {
                            Some(override_val) => override_val.clone(),
                            None => prev.pack_id,
                        };
                        let (o_type, o_size, is_synced) =
                            if should_promote_object_index_type(&prev.o_type, &rec.o_type) {
                                (rec.o_type, rec.o_size, false)
                            } else {
                                (prev.o_type, prev.o_size, prev.is_synced)
                            };
                        (
                            old_pack,
                            ObjectIndexStoredRow {
                                o_type,
                                o_size,
                                created_at: prev.created_at,
                                is_synced,
                                pack_id: next_pack,
                            },
                        )
                    }
                    None => {
                        let next_pack = match &pack_override_owned {
                            Some(override_val) => override_val.clone(),
                            None => None,
                        };
                        (
                            None,
                            ObjectIndexStoredRow {
                                o_type: rec.o_type,
                                o_size: rec.o_size,
                                created_at: rec.created_at,
                                is_synced: rec.is_synced,
                                pack_id: next_pack,
                            },
                        )
                    }
                };

                if old_pack_id != next_row.pack_id {
                    if let Some(old_pack) = old_pack_id.as_deref() {
                        let _ = pack_table
                            .remove((old_pack, rec.repo_id.as_str(), rec.o_id.as_str()))
                            .map_err(|err| {
                                map_redb_storage_err(err, "remove stale object_by_pack entry")
                            })?;
                    }
                    if let Some(new_pack) = next_row.pack_id.as_deref() {
                        pack_table
                            .insert(
                                (new_pack, rec.repo_id.as_str(), rec.o_id.as_str()),
                                pack_marker.as_slice(),
                            )
                            .map_err(|err| {
                                map_redb_storage_err(err, "insert object_by_pack entry")
                            })?;
                    }
                }

                let encoded = encode_versioned(GIT_OBJECT_INDEX_CODEC_VERSION, &next_row)?;
                obj_table
                    .insert(
                        (rec.repo_id.as_str(), rec.o_id.as_str()),
                        encoded.as_slice(),
                    )
                    .map_err(|err| map_redb_storage_err(err, "upsert object_index entry"))?;
            }

            Ok((true, ()))
        })
        .await
    }

    /// Atomically upsert `object_index` records together with same-transaction
    /// `OBJECT_BY_PACK` secondary index maintenance (`RB-04b`).
    ///
    /// Pass `Some(pack_id)` to bind/rebind every object in `records` to `pack_id`,
    /// or `None` to clear their pack binding (loose objects), removing any stale
    /// `(old_pack_id, repo_id, o_id)` index rows inside the same `WriteTransaction`.
    pub async fn upsert_object_index_batch_with_pack(
        &self,
        records: &[ObjectIndexRecord],
        pack_id: Option<&str>,
    ) -> StorageResult<()> {
        self.upsert_object_index_batch_inner(records, Some(pack_id))
            .await
    }

    /// List live `object_index` records indexed under `pack_id` via the
    /// same-transaction `OBJECT_BY_PACK` secondary index (`RB-04b`), optionally
    /// scoped to `repo_id` and excluding any OID fenced by `object_obliteration`.
    pub async fn list_object_index_by_pack(
        &self,
        pack_id: &str,
        repo_id: Option<&str>,
    ) -> StorageResult<Vec<ObjectIndexRecord>> {
        validate_non_empty_identifier(pack_id, "object_by_pack pack_id")?;
        if let Some(rid) = repo_id {
            validate_non_empty_identifier(rid, "object_index repo_id")?;
        }
        let pack_owned = pack_id.to_string();
        let repo_owned = repo_id.map(str::to_string);

        self.run_read(move |rtx| {
            let Some(pack_table) =
                open_optional_read_table(rtx, OBJECT_BY_PACK_TABLE, "open object_by_pack index")?
            else {
                return Ok(Vec::new());
            };
            let Some(obj_table) =
                open_optional_read_table(rtx, OBJECT_INDEX_TABLE, "open object_index table")?
            else {
                return Ok(Vec::new());
            };
            let Some(ob_table) = open_optional_read_table(
                rtx,
                OBJECT_OBLITERATION_TABLE,
                "open object_obliteration table",
            )?
            else {
                return Ok(Vec::new());
            };

            let start_repo = repo_owned.as_deref().unwrap_or("");
            let range = pack_table
                .range((pack_owned.as_str(), start_repo, "")..)
                .map_err(|err| map_redb_storage_err(err, "range scan object_by_pack"))?;
            let mut out = Vec::new();
            for entry in range {
                let (k, _) =
                    entry.map_err(|err| map_redb_storage_err(err, "read object_by_pack entry"))?;
                let (found_pack, found_repo, found_oid) = k.value();
                if found_pack != pack_owned {
                    break;
                }
                if let Some(expected_repo) = repo_owned.as_deref()
                    && found_repo != expected_repo
                {
                    break;
                }
                if lookup_obliteration_in_table(&ob_table, found_oid)?.is_some() {
                    continue;
                }
                let row_guard = obj_table
                    .get((found_repo, found_oid))
                    .map_err(|err| {
                        map_redb_storage_err(err, "read object_index via object_by_pack")
                    })?
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "object_by_pack index references missing object_index primary row"
                            .to_string(),
                    })?;
                let stored = decode_object_index_row(row_guard.value())?;
                out.push(object_index_record_from_stored(
                    found_repo.to_string(),
                    found_oid.to_string(),
                    stored,
                ));
            }
            Ok(out)
        })
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

    async fn upsert_object_index_batch(&self, records: &[ObjectIndexRecord]) -> StorageResult<()> {
        self.upsert_object_index_batch_inner(records, None).await
    }

    async fn get_object_index(
        &self,
        repo_id: &str,
        o_id: &str,
    ) -> StorageResult<Option<ObjectIndexRecord>> {
        validate_non_empty_identifier(repo_id, "object_index repo_id")?;
        validate_hex_oid(o_id, "object_index o_id")?;
        let repo_owned = repo_id.to_string();
        let oid_owned = o_id.to_string();

        self.run_read(move |rtx| {
            let Some(ob_table) = open_optional_read_table(
                rtx,
                OBJECT_OBLITERATION_TABLE,
                "open object_obliteration table",
            )?
            else {
                return Ok(None);
            };
            if let Some(tomb) = lookup_obliteration_in_table(&ob_table, &oid_owned)? {
                return Err(obliteration_fenced_read_error(tomb.state));
            }
            let Some(obj_table) =
                open_optional_read_table(rtx, OBJECT_INDEX_TABLE, "open object_index table")?
            else {
                return Ok(None);
            };
            let Some(guard) = obj_table
                .get((repo_owned.as_str(), oid_owned.as_str()))
                .map_err(|err| map_redb_storage_err(err, "read object_index entry"))?
            else {
                return Ok(None);
            };
            let stored = decode_object_index_row(guard.value())?;
            Ok(Some(object_index_record_from_stored(
                repo_owned, oid_owned, stored,
            )))
        })
        .await
    }

    async fn get_object_index_preserved_address(
        &self,
        repo_id: &str,
        o_id: &str,
    ) -> StorageResult<Option<ObjectIndexRecord>> {
        validate_non_empty_identifier(repo_id, "object_index repo_id")?;
        validate_hex_oid(o_id, "object_index o_id")?;
        let repo_owned = repo_id.to_string();
        let oid_owned = o_id.to_string();

        self.run_read(move |rtx| {
            let Some(obj_table) =
                open_optional_read_table(rtx, OBJECT_INDEX_TABLE, "open object_index table")?
            else {
                return Ok(None);
            };
            let Some(guard) = obj_table
                .get((repo_owned.as_str(), oid_owned.as_str()))
                .map_err(|err| map_redb_storage_err(err, "read preserved object_index entry"))?
            else {
                return Ok(None);
            };
            let stored = decode_object_index_row(guard.value())?;
            Ok(Some(object_index_record_from_stored(
                repo_owned, oid_owned, stored,
            )))
        })
        .await
    }

    async fn list_object_index_by_repo(
        &self,
        repo_id: &str,
        synced_filter: Option<bool>,
    ) -> StorageResult<Vec<ObjectIndexRecord>> {
        validate_non_empty_identifier(repo_id, "object_index repo_id")?;
        let repo_owned = repo_id.to_string();

        self.run_read(move |rtx| {
            let Some(obj_table) =
                open_optional_read_table(rtx, OBJECT_INDEX_TABLE, "open object_index table")?
            else {
                return Ok(Vec::new());
            };
            let Some(ob_table) = open_optional_read_table(
                rtx,
                OBJECT_OBLITERATION_TABLE,
                "open object_obliteration table",
            )?
            else {
                return Ok(Vec::new());
            };

            let mut out = Vec::new();
            let range = obj_table
                .range((repo_owned.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "range scan object_index"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read object_index entry"))?;
                let (found_repo, found_oid) = k.value();
                if found_repo != repo_owned {
                    break;
                }
                if lookup_obliteration_in_table(&ob_table, found_oid)?.is_some() {
                    continue;
                }
                let stored = decode_object_index_row(v.value())?;
                if let Some(expected_synced) = synced_filter
                    && stored.is_synced != expected_synced
                {
                    continue;
                }
                out.push(object_index_record_from_stored(
                    found_repo.to_string(),
                    found_oid.to_string(),
                    stored,
                ));
            }
            Ok(out)
        })
        .await
    }

    async fn mark_object_index_synced(
        &self,
        repo_id: &str,
        o_ids: &[String],
    ) -> StorageResult<usize> {
        validate_non_empty_identifier(repo_id, "object_index repo_id")?;
        for oid in o_ids {
            validate_hex_oid(oid, "object_index o_id")?;
        }
        if o_ids.is_empty() {
            let _ = self.writable_db()?;
            return Ok(0);
        }
        let repo_owned = repo_id.to_string();
        let oids_owned = o_ids.to_vec();

        self.run_write_conditional(move |wtx| {
            let mut distinct = Vec::with_capacity(oids_owned.len());
            let mut seen = HashSet::with_capacity(oids_owned.len());
            for oid in oids_owned {
                if seen.insert(oid.clone()) {
                    distinct.push(oid);
                }
            }

            {
                let ob_table = wtx.open_table(OBJECT_OBLITERATION_TABLE).map_err(|err| {
                    map_redb_table_err(err, "open object_obliteration table for sync fence")
                })?;
                for oid in &distinct {
                    if let Some(tomb) = lookup_obliteration_in_table(&ob_table, oid)? {
                        return Err(obliteration_fenced_write_error(tomb.state));
                    }
                }
            }

            let mut obj_table = wtx
                .open_table(OBJECT_INDEX_TABLE)
                .map_err(|err| map_redb_table_err(err, "open object_index table for sync"))?;
            let mut updated = 0usize;
            for oid in &distinct {
                let existing = match obj_table
                    .get((repo_owned.as_str(), oid.as_str()))
                    .map_err(|err| map_redb_storage_err(err, "read object_index for sync"))?
                {
                    Some(guard) => Some(decode_object_index_row(guard.value())?),
                    None => None,
                };
                if let Some(mut row) = existing
                    && !row.is_synced
                {
                    row.is_synced = true;
                    let encoded = encode_versioned(GIT_OBJECT_INDEX_CODEC_VERSION, &row)?;
                    obj_table
                        .insert((repo_owned.as_str(), oid.as_str()), encoded.as_slice())
                        .map_err(|err| {
                            map_redb_storage_err(err, "update object_index is_synced")
                        })?;
                    updated += 1;
                }
            }
            Ok((updated > 0, updated))
        })
        .await
    }

    async fn begin_object_obliteration(
        &self,
        req: &ObliterationRequest,
    ) -> StorageResult<ObjectObliterationRecord> {
        validate_obliteration_request(req)?;
        let req_owned = req.clone();

        self.run_write_conditional(move |wtx| {
            let mut ob_table = wtx.open_table(OBJECT_OBLITERATION_TABLE).map_err(|err| {
                map_redb_table_err(err, "open object_obliteration table for begin")
            })?;
            let kind_str = req_owned.hash_kind.as_str();
            let existing = match ob_table
                .get((req_owned.oid.as_str(), kind_str))
                .map_err(|err| map_redb_storage_err(err, "read object_obliteration"))?
            {
                Some(guard) => Some(decode_obliteration_row(
                    req_owned.oid.clone(),
                    kind_str,
                    guard.value(),
                )?),
                None => None,
            };
            if let Some(record) = existing {
                return Ok((false, record));
            }
            let stored = ObjectObliterationStoredRow {
                state: ObliterationState::Obliterating,
                reason: req_owned.reason.clone(),
                actor: req_owned.actor.clone(),
                approval_source: req_owned.approval_source.clone(),
                requested_at: req_owned.recorded_at.clone(),
                tombstone_written_at: req_owned.recorded_at.clone(),
                payload_deleted_at: None,
                updated_at: req_owned.recorded_at.clone(),
            };
            let encoded = encode_versioned(GIT_OBJECT_OBLITERATION_CODEC_VERSION, &stored)?;
            ob_table
                .insert((req_owned.oid.as_str(), kind_str), encoded.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert object_obliteration tombstone"))?;
            Ok((
                true,
                ObjectObliterationRecord {
                    oid: req_owned.oid,
                    hash_kind: req_owned.hash_kind,
                    state: stored.state,
                    reason: stored.reason,
                    actor: stored.actor,
                    approval_source: stored.approval_source,
                    requested_at: stored.requested_at,
                    tombstone_written_at: stored.tombstone_written_at,
                    payload_deleted_at: stored.payload_deleted_at,
                    updated_at: stored.updated_at,
                },
            ))
        })
        .await
    }

    async fn mark_object_obliterated(
        &self,
        oid: &str,
        hash_kind: ObjectHashKind,
        deleted_at: &str,
    ) -> StorageResult<ObjectObliterationRecord> {
        validate_oid_for_hash_kind(oid, hash_kind)?;
        validate_non_empty_identifier(deleted_at, "obliteration deleted_at")?;
        let oid_owned = oid.to_string();
        let deleted_owned = deleted_at.to_string();

        self.run_write_conditional(move |wtx| {
            let mut ob_table = wtx.open_table(OBJECT_OBLITERATION_TABLE).map_err(|err| {
                map_redb_table_err(err, "open object_obliteration table for finalize")
            })?;
            let kind_str = hash_kind.as_str();
            let existing = match ob_table
                .get((oid_owned.as_str(), kind_str))
                .map_err(|err| map_redb_storage_err(err, "read object_obliteration for finalize"))?
            {
                Some(guard) => decode_obliteration_row(oid_owned.clone(), kind_str, guard.value())?,
                None => {
                    return Err(StorageBackendError::NotFound {
                        resource: "object_obliteration tombstone".to_string(),
                    });
                }
            };
            if existing.state == ObliterationState::Obliterated {
                return Ok((false, existing));
            }
            let updated_row = ObjectObliterationStoredRow {
                state: ObliterationState::Obliterated,
                reason: existing.reason.clone(),
                actor: existing.actor.clone(),
                approval_source: existing.approval_source.clone(),
                requested_at: existing.requested_at.clone(),
                tombstone_written_at: existing.tombstone_written_at.clone(),
                payload_deleted_at: Some(deleted_owned.clone()),
                updated_at: deleted_owned.clone(),
            };
            let encoded = encode_versioned(GIT_OBJECT_OBLITERATION_CODEC_VERSION, &updated_row)?;
            ob_table
                .insert((oid_owned.as_str(), kind_str), encoded.as_slice())
                .map_err(|err| map_redb_storage_err(err, "finalize object_obliteration"))?;
            Ok((
                true,
                ObjectObliterationRecord {
                    oid: oid_owned,
                    hash_kind,
                    state: ObliterationState::Obliterated,
                    reason: updated_row.reason,
                    actor: updated_row.actor,
                    approval_source: updated_row.approval_source,
                    requested_at: updated_row.requested_at,
                    tombstone_written_at: updated_row.tombstone_written_at,
                    payload_deleted_at: updated_row.payload_deleted_at,
                    updated_at: updated_row.updated_at,
                },
            ))
        })
        .await
    }

    async fn get_object_obliteration(
        &self,
        oid: &str,
    ) -> StorageResult<Option<ObjectObliterationRecord>> {
        validate_hex_oid(oid, "object_obliteration oid")?;
        let oid_owned = oid.to_string();
        self.run_read(move |rtx| {
            let Some(ob_table) = open_optional_read_table(
                rtx,
                OBJECT_OBLITERATION_TABLE,
                "open object_obliteration table",
            )?
            else {
                return Ok(None);
            };
            lookup_obliteration_in_table(&ob_table, &oid_owned)
        })
        .await
    }

    async fn is_object_obliterated(&self, oid: &str) -> StorageResult<bool> {
        Ok(self.get_object_obliteration(oid).await?.is_some())
    }

    async fn list_incomplete_obliterations(&self) -> StorageResult<Vec<ObjectObliterationRecord>> {
        self.run_read(move |rtx| {
            let Some(ob_table) = open_optional_read_table(
                rtx,
                OBJECT_OBLITERATION_TABLE,
                "open object_obliteration table",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let iter = ob_table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iterate object_obliteration"))?;
            for entry in iter {
                let (k, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read object_obliteration entry"))?;
                let (oid, kind_str) = k.value();
                let record = decode_obliteration_row(oid.to_string(), kind_str, v.value())?;
                if record.state == ObliterationState::Obliterating {
                    out.push(record);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn list_obliterated_oids(&self) -> StorageResult<Vec<String>> {
        self.run_read(move |rtx| {
            let Some(ob_table) = open_optional_read_table(
                rtx,
                OBJECT_OBLITERATION_TABLE,
                "open object_obliteration table",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out: Vec<String> = Vec::new();
            let iter = ob_table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iterate object_obliteration"))?;
            for entry in iter {
                let (k, _) = entry
                    .map_err(|err| map_redb_storage_err(err, "read object_obliteration key"))?;
                let oid = k.value().0;
                if out.last().map(String::as_str) != Some(oid) {
                    out.push(oid.to_string());
                }
            }
            Ok(out)
        })
        .await
    }

    async fn run_fenced_object_read(
        &self,
        oid: &str,
        read_payload: Box<dyn FnOnce() -> StorageResult<Vec<u8>> + Send>,
    ) -> StorageResult<Vec<u8>> {
        validate_hex_oid(oid, "object OID")?;
        if let Some(rec) = self.get_object_obliteration(oid).await? {
            return Err(obliteration_fenced_read_error(rec.state));
        }
        let payload_result = tokio::task::spawn_blocking(read_payload)
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("fenced object read task failed: {err}"),
            })?;
        // Post-read monotonic verification: if an obliteration tombstone
        // committed while `read_payload` ran, discard loaded bytes (or replace
        // a post-unlink error) and fail closed with the obliteration fence error.
        if let Some(rec) = self.get_object_obliteration(oid).await? {
            return Err(obliteration_fenced_read_error(rec.state));
        }
        payload_result
    }

    async fn run_fenced_object_write(
        &self,
        record: &ObjectIndexRecord,
        write_payload: Box<dyn FnOnce() -> StorageResult<()> + Send>,
    ) -> StorageResult<()> {
        validate_object_index_record(record)?;
        let rec = record.clone();

        self.run_write_conditional(move |wtx| {
            {
                let ob_table = wtx.open_table(OBJECT_OBLITERATION_TABLE).map_err(|err| {
                    map_redb_table_err(err, "open object_obliteration table for fenced write")
                })?;
                if let Some(tomb) = lookup_obliteration_in_table(&ob_table, &rec.o_id)? {
                    return Err(obliteration_fenced_write_error(tomb.state));
                }
            }

            // Execute payload materialization while holding `redb`'s cross-process
            // exclusive `WriteTransaction` so no concurrent `begin_object_obliteration`
            // can commit between the fence check and index registration.
            write_payload()?;

            let mut obj_table = wtx.open_table(OBJECT_INDEX_TABLE).map_err(|err| {
                map_redb_table_err(err, "open object_index table for fenced write")
            })?;
            let existing = match obj_table
                .get((rec.repo_id.as_str(), rec.o_id.as_str()))
                .map_err(|err| map_redb_storage_err(err, "read object_index entry"))?
            {
                Some(guard) => Some(decode_object_index_row(guard.value())?),
                None => None,
            };
            let next_row = match existing {
                Some(prev) => {
                    let (o_type, o_size, is_synced) =
                        if should_promote_object_index_type(&prev.o_type, &rec.o_type) {
                            (rec.o_type, rec.o_size, false)
                        } else {
                            (prev.o_type, prev.o_size, prev.is_synced)
                        };
                    ObjectIndexStoredRow {
                        o_type,
                        o_size,
                        created_at: prev.created_at,
                        is_synced,
                        pack_id: prev.pack_id,
                    }
                }
                None => ObjectIndexStoredRow {
                    o_type: rec.o_type,
                    o_size: rec.o_size,
                    created_at: rec.created_at,
                    is_synced: rec.is_synced,
                    pack_id: None,
                },
            };
            let encoded = encode_versioned(GIT_OBJECT_INDEX_CODEC_VERSION, &next_row)?;
            obj_table
                .insert(
                    (rec.repo_id.as_str(), rec.o_id.as_str()),
                    encoded.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert fenced object_index entry"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_note(&self, notes_ref: &str, object: &str) -> StorageResult<Option<NoteRecord>> {
        validate_notes_ref(notes_ref)?;
        validate_hex_oid(object, "note object OID")?;
        let ref_owned = notes_ref.to_string();
        let obj_owned = object.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_optional_read_table(rtx, NOTES_TABLE, "open notes table")?
            else {
                return Ok(None);
            };
            let Some(guard) = table
                .get((ref_owned.as_str(), obj_owned.as_str()))
                .map_err(|err| map_redb_storage_err(err, "get note entry"))?
            else {
                return Ok(None);
            };
            let blob = decode_note_row(guard.value())?;
            Ok(Some(NoteRecord {
                notes_ref: ref_owned,
                object: obj_owned,
                blob,
            }))
        })
        .await
    }

    async fn list_notes(&self, notes_ref: &str) -> StorageResult<Vec<NoteRecord>> {
        validate_notes_ref(notes_ref)?;
        let ref_owned = notes_ref.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_optional_read_table(rtx, NOTES_TABLE, "open notes table")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((ref_owned.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "range scan notes table"))?;
            for entry in range {
                let (k, v) = entry.map_err(|err| map_redb_storage_err(err, "read note entry"))?;
                let (found_ref, found_obj) = k.value();
                if found_ref != ref_owned {
                    break;
                }
                let blob = decode_note_row(v.value())?;
                out.push(NoteRecord {
                    notes_ref: found_ref.to_string(),
                    object: found_obj.to_string(),
                    blob,
                });
            }
            Ok(out)
        })
        .await
    }

    async fn put_note(
        &self,
        notes_ref: &str,
        object: &str,
        blob: &str,
        overwrite: bool,
    ) -> StorageResult<NoteWriteOutcome> {
        validate_notes_ref(notes_ref)?;
        validate_hex_oid(object, "note object OID")?;
        validate_hex_oid(blob, "note blob OID")?;
        let ref_owned = notes_ref.to_string();
        let obj_owned = object.to_string();
        let blob_owned = blob.to_string();

        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(NOTES_TABLE)
                .map_err(|err| map_redb_table_err(err, "open notes table for write"))?;
            let existing = match table
                .get((ref_owned.as_str(), obj_owned.as_str()))
                .map_err(|err| map_redb_storage_err(err, "read note entry"))?
            {
                Some(guard) => Some(decode_note_row(guard.value())?),
                None => None,
            };
            match existing {
                None => {
                    let encoded = encode_versioned(
                        GIT_NOTES_CODEC_VERSION,
                        &NoteStoredRow { blob: blob_owned },
                    )?;
                    table
                        .insert((ref_owned.as_str(), obj_owned.as_str()), encoded.as_slice())
                        .map_err(|err| map_redb_storage_err(err, "insert note entry"))?;
                    Ok((true, NoteWriteOutcome::Inserted))
                }
                Some(existing_blob) if !overwrite => {
                    Ok((false, NoteWriteOutcome::AlreadyExists { existing_blob }))
                }
                Some(previous_blob) => {
                    let encoded = encode_versioned(
                        GIT_NOTES_CODEC_VERSION,
                        &NoteStoredRow { blob: blob_owned },
                    )?;
                    table
                        .insert((ref_owned.as_str(), obj_owned.as_str()), encoded.as_slice())
                        .map_err(|err| map_redb_storage_err(err, "update note entry"))?;
                    Ok((true, NoteWriteOutcome::Replaced { previous_blob }))
                }
            }
        })
        .await
    }

    async fn remove_note_cas(
        &self,
        notes_ref: &str,
        object: &str,
        expected_blob: Option<&str>,
    ) -> StorageResult<bool> {
        validate_notes_ref(notes_ref)?;
        validate_hex_oid(object, "note object OID")?;
        if let Some(exp) = expected_blob {
            validate_hex_oid(exp, "note expected_blob OID")?;
        }
        let ref_owned = notes_ref.to_string();
        let obj_owned = object.to_string();
        let expected_owned = expected_blob.map(str::to_string);

        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(NOTES_TABLE)
                .map_err(|err| map_redb_table_err(err, "open notes table for delete"))?;
            let existing = match table
                .get((ref_owned.as_str(), obj_owned.as_str()))
                .map_err(|err| map_redb_storage_err(err, "read note entry for delete"))?
            {
                Some(guard) => Some(decode_note_row(guard.value())?),
                None => None,
            };
            let Some(actual) = existing else {
                return Ok((false, false));
            };
            if let Some(exp) = expected_owned.as_deref()
                && actual != exp
            {
                return Ok((false, false));
            }
            let _ = table
                .remove((ref_owned.as_str(), obj_owned.as_str()))
                .map_err(|err| map_redb_storage_err(err, "delete note entry"))?;
            Ok((true, true))
        })
        .await
    }

    async fn apply_notes_cas_batch(
        &self,
        notes_ref: &str,
        ops: &[NoteCasOp],
    ) -> StorageResult<NoteBatchOutcome> {
        validate_notes_ref(notes_ref)?;
        for op in ops {
            validate_note_cas_op(op)?;
        }
        if ops.is_empty() {
            let _ = self.writable_db()?;
            return Ok(NoteBatchOutcome::Applied {
                inserted: 0,
                updated: 0,
                deleted: 0,
            });
        }
        let ref_owned = notes_ref.to_string();
        let ops_owned = ops.to_vec();

        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(NOTES_TABLE)
                .map_err(|err| map_redb_table_err(err, "open notes table for batch"))?;
            let mut inserted = 0usize;
            let mut updated = 0usize;
            let mut deleted = 0usize;

            for op in ops_owned {
                match op {
                    NoteCasOp::InsertIfAbsent { object, blob } => {
                        let actual = match table
                            .get((ref_owned.as_str(), object.as_str()))
                            .map_err(|err| map_redb_storage_err(err, "read note for insert CAS"))?
                        {
                            Some(guard) => Some(decode_note_row(guard.value())?),
                            None => None,
                        };
                        if let Some(actual_blob) = actual {
                            return Ok((
                                false,
                                NoteBatchOutcome::CasConflict {
                                    object,
                                    actual_blob: Some(actual_blob),
                                },
                            ));
                        }
                        let encoded =
                            encode_versioned(GIT_NOTES_CODEC_VERSION, &NoteStoredRow { blob })?;
                        table
                            .insert((ref_owned.as_str(), object.as_str()), encoded.as_slice())
                            .map_err(|err| map_redb_storage_err(err, "batch insert note"))?;
                        inserted += 1;
                    }
                    NoteCasOp::UpdateIfMatches {
                        object,
                        expected_blob,
                        new_blob,
                    } => {
                        let actual = match table
                            .get((ref_owned.as_str(), object.as_str()))
                            .map_err(|err| map_redb_storage_err(err, "read note for update CAS"))?
                        {
                            Some(guard) => Some(decode_note_row(guard.value())?),
                            None => None,
                        };
                        if actual.as_deref() != Some(expected_blob.as_str()) {
                            return Ok((
                                false,
                                NoteBatchOutcome::CasConflict {
                                    object,
                                    actual_blob: actual,
                                },
                            ));
                        }
                        let encoded = encode_versioned(
                            GIT_NOTES_CODEC_VERSION,
                            &NoteStoredRow { blob: new_blob },
                        )?;
                        table
                            .insert((ref_owned.as_str(), object.as_str()), encoded.as_slice())
                            .map_err(|err| map_redb_storage_err(err, "batch update note"))?;
                        updated += 1;
                    }
                    NoteCasOp::DeleteIfMatches {
                        object,
                        expected_blob,
                    } => {
                        let actual = match table
                            .get((ref_owned.as_str(), object.as_str()))
                            .map_err(|err| map_redb_storage_err(err, "read note for delete CAS"))?
                        {
                            Some(guard) => Some(decode_note_row(guard.value())?),
                            None => None,
                        };
                        if actual.as_deref() != Some(expected_blob.as_str()) {
                            return Ok((
                                false,
                                NoteBatchOutcome::CasConflict {
                                    object,
                                    actual_blob: actual,
                                },
                            ));
                        }
                        let _ = table
                            .remove((ref_owned.as_str(), object.as_str()))
                            .map_err(|err| map_redb_storage_err(err, "batch delete note"))?;
                        deleted += 1;
                    }
                }
            }

            Ok((
                true,
                NoteBatchOutcome::Applied {
                    inserted,
                    updated,
                    deleted,
                },
            ))
        })
        .await
    }

    async fn get_revision_ordinal_meta(
        &self,
        ref_name: &str,
    ) -> StorageResult<Option<RevisionOrdinalMeta>> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        let ref_owned = ref_name.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_optional_read_table(
                rtx,
                REVISION_ORDINAL_META_TABLE,
                "open revision_ordinal_meta table",
            )?
            else {
                return Ok(None);
            };
            let Some(guard) = table
                .get(ref_owned.as_str())
                .map_err(|err| map_redb_storage_err(err, "get revision_ordinal_meta"))?
            else {
                return Ok(None);
            };
            decode_revision_ordinal_meta(ref_owned, guard.value()).map(Some)
        })
        .await
    }

    async fn list_revision_ordinal_metas(&self) -> StorageResult<Vec<RevisionOrdinalMeta>> {
        self.run_read(move |rtx| {
            let Some(table) = open_optional_read_table(
                rtx,
                REVISION_ORDINAL_META_TABLE,
                "open revision_ordinal_meta table",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let iter = table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iterate revision_ordinal_meta"))?;
            for entry in iter {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read revision_ordinal_meta"))?;
                out.push(decode_revision_ordinal_meta(
                    k.value().to_string(),
                    v.value(),
                )?);
            }
            Ok(out)
        })
        .await
    }

    async fn replace_revision_ordinals(
        &self,
        meta: &RevisionOrdinalMeta,
        entries: &[(i64, String)],
    ) -> StorageResult<()> {
        validate_revision_ordinal_entries(meta, entries)?;
        let meta_owned = meta.clone();
        let entries_owned = entries.to_vec();

        self.run_write_conditional(move |wtx| {
            let mut ord_table = wtx
                .open_table(REVISION_ORDINAL_TABLE)
                .map_err(|err| map_redb_table_err(err, "open revision_ordinal table"))?;
            let mut by_oid_table = wtx
                .open_table(REVISION_ORDINAL_BY_OID_TABLE)
                .map_err(|err| map_redb_table_err(err, "open revision_ordinal_by_oid index"))?;
            let mut meta_table = wtx
                .open_table(REVISION_ORDINAL_META_TABLE)
                .map_err(|err| map_redb_table_err(err, "open revision_ordinal_meta table"))?;

            clear_revision_ordinals_for_ref(
                &mut ord_table,
                &mut by_oid_table,
                &meta_owned.ref_name,
            )?;

            for (ordinal, oid) in &entries_owned {
                let encoded_oid = encode_versioned(
                    GIT_REVISION_ORDINAL_CODEC_VERSION,
                    &RevisionOrdinalStoredRow { oid: oid.clone() },
                )?;
                ord_table
                    .insert(
                        (meta_owned.ref_name.as_str(), *ordinal),
                        encoded_oid.as_slice(),
                    )
                    .map_err(|err| map_redb_storage_err(err, "insert revision_ordinal row"))?;
                let encoded_ord = encode_versioned(
                    GIT_REVISION_ORDINAL_BY_OID_CODEC_VERSION,
                    &RevisionOrdinalByOidStoredRow { ordinal: *ordinal },
                )?;
                by_oid_table
                    .insert(
                        (meta_owned.ref_name.as_str(), oid.as_str()),
                        encoded_ord.as_slice(),
                    )
                    .map_err(|err| {
                        map_redb_storage_err(err, "insert revision_ordinal_by_oid index")
                    })?;
            }

            let encoded_meta = encode_versioned(
                GIT_REVISION_ORDINAL_META_CODEC_VERSION,
                &RevisionOrdinalMetaStoredRow {
                    tip_oid: meta_owned.tip_oid,
                    replace_sig: meta_owned.replace_sig,
                    max_ordinal: meta_owned.max_ordinal,
                    built_at: meta_owned.built_at,
                },
            )?;
            meta_table
                .insert(meta_owned.ref_name.as_str(), encoded_meta.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert revision_ordinal_meta"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn append_revision_ordinals(
        &self,
        ref_name: &str,
        expected_tip_oid: &str,
        expected_replace_sig: &str,
        new_meta: &RevisionOrdinalMeta,
        suffix_oids: &[String],
    ) -> StorageResult<RevisionOrdinalAppendOutcome> {
        validate_revision_ordinal_append(
            ref_name,
            expected_tip_oid,
            expected_replace_sig,
            new_meta,
            suffix_oids,
        )?;
        let ref_owned = ref_name.to_string();
        let exp_tip = expected_tip_oid.to_string();
        let exp_sig = expected_replace_sig.to_string();
        let meta_owned = new_meta.clone();
        let suffix_owned = suffix_oids.to_vec();

        self.run_write_conditional(move |wtx| {
            let mut ord_table = wtx
                .open_table(REVISION_ORDINAL_TABLE)
                .map_err(|err| map_redb_table_err(err, "open revision_ordinal table"))?;
            let mut by_oid_table = wtx
                .open_table(REVISION_ORDINAL_BY_OID_TABLE)
                .map_err(|err| map_redb_table_err(err, "open revision_ordinal_by_oid index"))?;
            let mut meta_table = wtx
                .open_table(REVISION_ORDINAL_META_TABLE)
                .map_err(|err| map_redb_table_err(err, "open revision_ordinal_meta table"))?;

            let existing = match meta_table
                .get(ref_owned.as_str())
                .map_err(|err| map_redb_storage_err(err, "read revision_ordinal_meta"))?
            {
                Some(guard) => Some(decode_revision_ordinal_meta(
                    ref_owned.clone(),
                    guard.value(),
                )?),
                None => None,
            };
            let Some(existing) = existing else {
                return Ok((false, RevisionOrdinalAppendOutcome::Stale { actual: None }));
            };
            if existing.tip_oid != exp_tip || existing.replace_sig != exp_sig {
                return Ok((
                    false,
                    RevisionOrdinalAppendOutcome::Stale {
                        actual: Some(existing),
                    },
                ));
            }
            let suffix_len = i64::try_from(suffix_owned.len()).map_err(|_| {
                StorageBackendError::BackendInternal {
                    detail: "revision_ordinal suffix count overflowed i64".to_string(),
                }
            })?;
            let expected_new_max =
                existing.max_ordinal.checked_add(suffix_len).ok_or_else(|| {
                    StorageBackendError::BackendInternal {
                        detail: "revision_ordinal max_ordinal overflowed i64".to_string(),
                    }
                })?;
            if meta_owned.max_ordinal != expected_new_max {
                return Err(StorageBackendError::BackendInternal {
                    detail:
                        "revision_ordinal append new_meta.max_ordinal does not match existing + suffix"
                            .to_string(),
                });
            }

            // Enforce `UNIQUE(ref_name, oid)` before inserting any rows.
            for oid in &suffix_owned {
                if by_oid_table
                    .get((ref_owned.as_str(), oid.as_str()))
                    .map_err(|err| {
                        map_redb_storage_err(err, "check duplicate oid in revision_ordinal")
                    })?
                    .is_some()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "duplicate OID in revision_ordinal append for ref".to_string(),
                    });
                }
            }

            let mut ord = existing.max_ordinal;
            for oid in &suffix_owned {
                ord += 1;
                let encoded_oid = encode_versioned(
                    GIT_REVISION_ORDINAL_CODEC_VERSION,
                    &RevisionOrdinalStoredRow { oid: oid.clone() },
                )?;
                ord_table
                    .insert((ref_owned.as_str(), ord), encoded_oid.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "append revision_ordinal row"))?;
                let encoded_ord = encode_versioned(
                    GIT_REVISION_ORDINAL_BY_OID_CODEC_VERSION,
                    &RevisionOrdinalByOidStoredRow { ordinal: ord },
                )?;
                by_oid_table
                    .insert((ref_owned.as_str(), oid.as_str()), encoded_ord.as_slice())
                    .map_err(|err| {
                        map_redb_storage_err(err, "append revision_ordinal_by_oid index")
                    })?;
            }

            let encoded_meta = encode_versioned(
                GIT_REVISION_ORDINAL_META_CODEC_VERSION,
                &RevisionOrdinalMetaStoredRow {
                    tip_oid: meta_owned.tip_oid.clone(),
                    replace_sig: meta_owned.replace_sig.clone(),
                    max_ordinal: meta_owned.max_ordinal,
                    built_at: meta_owned.built_at.clone(),
                },
            )?;
            meta_table
                .insert(ref_owned.as_str(), encoded_meta.as_slice())
                .map_err(|err| map_redb_storage_err(err, "update revision_ordinal_meta"))?;

            Ok((true, RevisionOrdinalAppendOutcome::Applied { meta: meta_owned }))
        })
        .await
    }

    async fn find_revision_oid_by_ordinal(
        &self,
        ref_name: &str,
        ordinal: i64,
    ) -> StorageResult<Option<String>> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        if ordinal < 1 {
            return Ok(None);
        }
        let ref_owned = ref_name.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_optional_read_table(
                rtx,
                REVISION_ORDINAL_TABLE,
                "open revision_ordinal table",
            )?
            else {
                return Ok(None);
            };
            let Some(guard) = table
                .get((ref_owned.as_str(), ordinal))
                .map_err(|err| map_redb_storage_err(err, "get revision_ordinal by ordinal"))?
            else {
                return Ok(None);
            };
            decode_revision_ordinal_row(guard.value()).map(Some)
        })
        .await
    }

    async fn find_revision_ordinal_by_oid(
        &self,
        ref_name: &str,
        oid: &str,
    ) -> StorageResult<Option<i64>> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        validate_hex_oid(oid, "revision_ordinal oid")?;
        let ref_owned = ref_name.to_string();
        let oid_owned = oid.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_optional_read_table(
                rtx,
                REVISION_ORDINAL_BY_OID_TABLE,
                "open revision_ordinal_by_oid index",
            )?
            else {
                return Ok(None);
            };
            let Some(guard) = table
                .get((ref_owned.as_str(), oid_owned.as_str()))
                .map_err(|err| map_redb_storage_err(err, "get revision_ordinal by oid"))?
            else {
                return Ok(None);
            };
            decode_revision_ordinal_by_oid(guard.value()).map(Some)
        })
        .await
    }

    async fn list_revision_ordinals(&self, ref_name: &str) -> StorageResult<Vec<(i64, String)>> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        let ref_owned = ref_name.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_optional_read_table(
                rtx,
                REVISION_ORDINAL_TABLE,
                "open revision_ordinal table",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((ref_owned.as_str(), i64::MIN)..=(ref_owned.as_str(), i64::MAX))
                .map_err(|err| map_redb_storage_err(err, "range scan revision_ordinal"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read revision_ordinal"))?;
                out.push((k.value().1, decode_revision_ordinal_row(v.value())?));
            }
            Ok(out)
        })
        .await
    }

    async fn count_revision_ordinals(&self, ref_name: &str) -> StorageResult<u64> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        let ref_owned = ref_name.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_optional_read_table(
                rtx,
                REVISION_ORDINAL_TABLE,
                "open revision_ordinal table",
            )?
            else {
                return Ok(0);
            };
            let range = table
                .range((ref_owned.as_str(), i64::MIN)..=(ref_owned.as_str(), i64::MAX))
                .map_err(|err| map_redb_storage_err(err, "range scan revision_ordinal count"))?;
            let mut count = 0u64;
            for entry in range {
                let _ = entry
                    .map_err(|err| map_redb_storage_err(err, "read revision_ordinal count"))?;
                count += 1;
            }
            Ok(count)
        })
        .await
    }

    async fn prune_revision_ordinals_except(&self, live_refs: &[String]) -> StorageResult<usize> {
        for r in live_refs {
            validate_non_empty_identifier(r, "revision_ordinal live ref")?;
        }
        let live_set: HashSet<String> = live_refs.iter().cloned().collect();

        self.run_write_conditional(move |wtx| {
            let mut ord_table = wtx
                .open_table(REVISION_ORDINAL_TABLE)
                .map_err(|err| map_redb_table_err(err, "open revision_ordinal table for prune"))?;
            let mut by_oid_table =
                wtx.open_table(REVISION_ORDINAL_BY_OID_TABLE)
                    .map_err(|err| {
                        map_redb_table_err(err, "open revision_ordinal_by_oid index for prune")
                    })?;
            let mut meta_table = wtx.open_table(REVISION_ORDINAL_META_TABLE).map_err(|err| {
                map_redb_table_err(err, "open revision_ordinal_meta table for prune")
            })?;

            let mut to_prune = Vec::new();
            {
                let iter = meta_table.iter().map_err(|err| {
                    map_redb_storage_err(err, "iterate revision_ordinal_meta for prune")
                })?;
                for entry in iter {
                    let (k, _) = entry.map_err(|err| {
                        map_redb_storage_err(err, "read revision_ordinal_meta key for prune")
                    })?;
                    let ref_name = k.value().to_string();
                    if !live_set.contains(&ref_name) {
                        to_prune.push(ref_name);
                    }
                }
            }

            let pruned = to_prune.len();
            for ref_name in &to_prune {
                clear_revision_ordinals_for_ref(&mut ord_table, &mut by_oid_table, ref_name)?;
                let _ = meta_table.remove(ref_name.as_str()).map_err(|err| {
                    map_redb_storage_err(err, "delete pruned revision_ordinal_meta")
                })?;
            }
            Ok((pruned > 0, pruned))
        })
        .await
    }
}

fn clear_revision_ordinals_for_ref(
    ord_table: &mut redb::Table<'_, (&'static str, i64), &'static [u8]>,
    by_oid_table: &mut redb::Table<'_, (&'static str, &'static str), &'static [u8]>,
    ref_name: &str,
) -> StorageResult<()> {
    let mut ordinals = Vec::new();
    {
        let range = ord_table
            .range((ref_name, i64::MIN)..=(ref_name, i64::MAX))
            .map_err(|err| map_redb_storage_err(err, "scan revision_ordinal for clear"))?;
        for entry in range {
            let (k, _) = entry
                .map_err(|err| map_redb_storage_err(err, "read revision_ordinal for clear"))?;
            ordinals.push(k.value().1);
        }
    }
    for ordinal in ordinals {
        let _ = ord_table
            .remove((ref_name, ordinal))
            .map_err(|err| map_redb_storage_err(err, "remove old revision_ordinal row"))?;
    }

    let mut oids = Vec::new();
    {
        let range = by_oid_table
            .range((ref_name, "")..)
            .map_err(|err| map_redb_storage_err(err, "scan revision_ordinal_by_oid for clear"))?;
        for entry in range {
            let (k, _) = entry.map_err(|err| {
                map_redb_storage_err(err, "read revision_ordinal_by_oid for clear")
            })?;
            let (found_ref, found_oid) = k.value();
            if found_ref != ref_name {
                break;
            }
            oids.push(found_oid.to_string());
        }
    }
    for oid in oids {
        let _ = by_oid_table
            .remove((ref_name, oid.as_str()))
            .map_err(|err| map_redb_storage_err(err, "remove old revision_ordinal_by_oid row"))?;
    }
    Ok(())
}
