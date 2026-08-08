//! Index-flagged obliteration (lore.md 2.5) — the "保留 ADDRESS 删 PAYLOAD"
//! compliance-deletion model (§19.6).
//!
//! An object's addressable REGISTRY entry (its `object_index` OID row and the
//! commit/tree bytes that reference it) is PRESERVED so referencing history
//! stays traversable; only the target OID's PAYLOAD bytes are physically
//! removed. The `object_obliteration` side-table (owned solely by
//! [`ObliterationStore`], §3.6) records an intentional-absence TOMBSTONE.
//!
//! STATE MACHINE (crash-safe by construction): a row's ABSENCE means Live, so
//! (no row) → INSERT `obliterating` (tombstone, fsynced BEFORE any payload
//! touch) → physical payload delete → UPDATE `obliterated`. A crash can only
//! ever leave `obliterating` with the payload possibly still present — NEVER
//! "payload deleted but marked Live". [`recover_incomplete`] re-runs the tail
//! idempotently.
//!
//! Every obliteration emits a MANDATORY durable audit record (§7.8): an
//! append-only, 0600 JSONL file, redaction-clean (no erased content, no
//! cleartext) — see [`audit`].

use std::{path::PathBuf, str::FromStr};

use git_internal::hash::{ObjectHash, get_hash_kind};
use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::{
    internal::db::get_db_conn_instance,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        util,
    },
};

pub mod audit;

/// A tombstone row (intentional-absence record).
#[derive(Debug, Clone)]
pub struct Tombstone {
    pub oid: String,
    pub hash_kind: String,
    pub state: String,
    pub reason: Option<String>,
}

impl Tombstone {
    pub fn is_obliterating(&self) -> bool {
        self.state == "obliterating"
    }
}

/// Single-owner store over `object_obliteration`.
pub struct ObliterationStore;

impl ObliterationStore {
    /// Whether an OID has a tombstone in ANY state (obliterating OR
    /// obliterated). This is the single predicate the fsck / heal / ingest /
    /// backup consult callers use — both states count as intentionally-absent
    /// so the mid-state is never mis-healed as Missing (closes the heal race).
    /// Absence-tolerant: a missing table (pre-migration / old binary) or a read
    /// error resolves to `false` for the READ-ONLY callers below; the mutating
    /// obliterate path uses [`Self::lookup`] which propagates errors.
    pub async fn is_tombstoned(hash: &ObjectHash) -> bool {
        Self::lookup(hash).await.ok().flatten().is_some()
    }

    /// Error-aware tombstone lookup (used by the mutating obliterate driver).
    pub async fn lookup(hash: &ObjectHash) -> Result<Option<Tombstone>, String> {
        let db = get_db_conn_instance().await;
        let oid = hash.to_string();
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT oid, hash_kind, state, reason FROM object_obliteration WHERE oid = ? LIMIT 1",
            [oid.into()],
        );
        let row = match db.query_one_raw(stmt).await {
            Ok(row) => row,
            Err(e) if e.to_string().contains("no such table") => return Ok(None),
            Err(e) => return Err(format!("failed to read obliteration tombstone: {e}")),
        };
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(Tombstone {
            oid: row.try_get_by_index(0).map_err(|e| e.to_string())?,
            hash_kind: row.try_get_by_index(1).map_err(|e| e.to_string())?,
            state: row.try_get_by_index(2).map_err(|e| e.to_string())?,
            reason: row.try_get_by_index(3).map_err(|e| e.to_string())?,
        }))
    }

    /// Live → Obliterating: write the tombstone row (the fsynced commit happens
    /// via the SQLite `synchronous = FULL` pin from lore.md 2.6). Idempotent on
    /// the `(oid, hash_kind)` UNIQUE key.
    pub async fn begin_obliterating(
        hash: &ObjectHash,
        reason: Option<&str>,
        actor: Option<&str>,
        approval_source: Option<&str>,
    ) -> Result<(), String> {
        let db = get_db_conn_instance().await;
        db.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT OR IGNORE INTO object_obliteration \
             (oid, hash_kind, state, reason, actor, approval_source) \
             VALUES (?, ?, 'obliterating', ?, ?, ?)",
            [
                hash.to_string().into(),
                hash_kind_str().into(),
                reason.into(),
                actor.into(),
                approval_source.into(),
            ],
        ))
        .await
        .map_err(|e| format!("failed to write obliteration tombstone: {e}"))?;
        // Verify a tombstone for THIS (oid, hash_kind) now exists (Codex P1):
        // `INSERT OR IGNORE` silently no-ops on a pre-existing row, so confirm
        // the caller may proceed (either freshly-inserted 'obliterating', or an
        // already-present tombstone in either state — both mean the object is
        // being / has been obliterated for this hash kind).
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT state FROM object_obliteration WHERE oid = ? AND hash_kind = ? LIMIT 1",
            [hash.to_string().into(), hash_kind_str().into()],
        );
        let row = db
            .query_one_raw(stmt)
            .await
            .map_err(|e| format!("failed to verify obliteration tombstone: {e}"))?;
        match row {
            Some(_) => Ok(()),
            None => Err(format!(
                "obliteration tombstone for {hash} ({}) was not written",
                hash_kind_str()
            )),
        }
    }

    /// Obliterating → Obliterated: record the payload as physically removed.
    pub async fn mark_obliterated(hash: &ObjectHash) -> Result<(), String> {
        let db = get_db_conn_instance().await;
        db.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE object_obliteration \
             SET state = 'obliterated', payload_deleted_at = CURRENT_TIMESTAMP, \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE oid = ?",
            [hash.to_string().into()],
        ))
        .await
        .map_err(|e| format!("failed to finalize obliteration: {e}"))?;
        Ok(())
    }

    /// All OIDs stuck in `obliterating` (crash recovery / diagnostics).
    pub async fn incomplete() -> Result<Vec<String>, String> {
        let db = get_db_conn_instance().await;
        let stmt = Statement::from_string(
            DbBackend::Sqlite,
            "SELECT oid FROM object_obliteration WHERE state = 'obliterating'".to_string(),
        );
        let rows = match db.query_all_raw(stmt).await {
            Ok(rows) => rows,
            Err(e) if e.to_string().contains("no such table") => return Ok(Vec::new()),
            Err(e) => return Err(format!("failed to scan incomplete obliterations: {e}")),
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row.try_get_by_index(0).map_err(|e| e.to_string())?);
        }
        Ok(out)
    }
}

/// Process-global snapshot of tombstoned OIDs, consulted SYNCHRONOUSLY by the
/// fsck connectivity seams (which cannot await a DB read). fsck refreshes it
/// once at the start of a run via [`refresh_snapshot`]; the default (no
/// tombstones) is an empty set → zero overhead and byte-identical
/// pre-feature fsck behavior.
static TOMBSTONE_SNAPSHOT: std::sync::RwLock<Option<std::collections::HashSet<String>>> =
    std::sync::RwLock::new(None);

/// Load the current tombstoned-OID set into the process snapshot (call at the
/// start of any read-only pass that must distinguish intentional absence from
/// corruption — e.g. fsck).
pub async fn refresh_snapshot() {
    let _ = refresh_snapshot_result().await;
}

/// [`refresh_snapshot`], but surfacing the load failure.
///
/// The infallible form treats an unreadable tombstone table as "nothing is
/// obliterated", which is the WRONG default for any caller that materializes
/// bytes: it would resurrect an obliterated object into the working tree and
/// call it success. Callers that write must use this and fail closed;
/// diagnostic readers (fsck) may keep the lenient form, where an empty
/// snapshot only costs a less precise message.
pub async fn refresh_snapshot_result() -> Result<(), sea_orm::DbErr> {
    let db = get_db_conn_instance().await;
    let stmt = Statement::from_string(
        DbBackend::Sqlite,
        "SELECT oid FROM object_obliteration".to_string(),
    );
    let set: std::collections::HashSet<String> = db
        .query_all_raw(stmt)
        .await?
        .into_iter()
        .filter_map(|row| row.try_get_by_index::<String>(0).ok())
        .collect();
    let mut guard = TOMBSTONE_SNAPSHOT
        .write()
        .unwrap_or_else(|poison| poison.into_inner());
    *guard = Some(set);
    Ok(())
}

/// SYNC: is `hash` intentionally absent (a tombstone exists in either state)?
/// Returns `false` when the snapshot is empty/unloaded.
pub fn is_tombstoned_cached(hash: &ObjectHash) -> bool {
    TOMBSTONE_SNAPSHOT
        .read()
        .unwrap_or_else(|poison| poison.into_inner())
        .as_ref()
        .is_some_and(|set| set.contains(&hash.to_string()))
}

fn hash_kind_str() -> &'static str {
    match get_hash_kind() {
        git_internal::hash::HashKind::Sha1 => "sha1",
        git_internal::hash::HashKind::Sha256 => "sha256",
    }
}

/// Loose-object payload path for `hash` (`.libra/objects/ab/cdef…`).
fn loose_payload_path(hash: &ObjectHash) -> Option<PathBuf> {
    let oid = hash.to_string();
    if oid.len() < 3 {
        return None;
    }
    Some(
        crate::utils::path::objects()
            .join(&oid[..2])
            .join(&oid[2..]),
    )
}

/// Classification of an OID at obliterate time.
#[derive(Debug, PartialEq)]
pub enum ObjectPresence {
    LooseOnly,
    PackedOnly,
    Absent,
}

/// Classify where `hash`'s payload physically lives (loose vs packed vs
/// absent). v1 refuses packed-only objects (no pack surgery).
pub fn classify_presence(hash: &ObjectHash) -> ObjectPresence {
    let loose = loose_payload_path(hash).is_some_and(|p| p.exists());
    if loose {
        return ObjectPresence::LooseOnly;
    }
    // Packed presence: check the LOCAL store ONLY (no alternates — lore.md
    // 2.3). Obliteration must never reach into a parent's borrowed store: an
    // object resolvable only via an alternate is NOT ours to obliterate, so it
    // classifies as Absent here and the driver refuses it. A local pack
    // (present locally, not loose) is PackedOnly.
    let storage =
        crate::utils::client_storage::ClientStorage::init_local(crate::utils::path::objects());
    if storage.exist(hash) {
        ObjectPresence::PackedOnly
    } else {
        ObjectPresence::Absent
    }
}

/// Physically delete the loose payload (idempotent) and purge the durable tier
/// + in-memory cache via the storage layer. Never touches packs.
pub async fn delete_payload(hash: &ObjectHash) -> CliResult<()> {
    // W0 §C.11 release gate: the borrower gate belongs HERE, at the unlink,
    // not only in `run_obliterate`. Crash recovery (`--recover`, and the
    // recovery pass `run_obliterate` itself performs BEFORE its own check)
    // reaches this function without passing that check, so a borrower that
    // registered after an interrupted obliteration could have an object it
    // still needs deleted out from under it. Re-checking here is cheap and
    // makes the gate impossible to route around.
    // §C.4.3 writer-vs-deleter, same reasoning and the same lock as `gc`:
    // the unlink must not overlap a publisher. Obliteration REFUSES rather
    // than defers — reporting an erasure the user asked for as done while
    // skipping it is the one outcome worse than failing — and the guard is
    // held until this function returns, covering the durable-tier purge too.
    //
    // Taken BEFORE the borrower gate, not after: registering a borrower is a
    // publication, so a gate evaluated outside the hold can be overtaken by
    // an `alternates add` that lands before the unlink.
    let _deletion_lock = crate::internal::maintenance_lock::MaintenanceLock::exclusive_or_refuse(
        &crate::utils::util::storage_path(),
        "delete an obliterated object's payload",
    )?;
    crate::internal::alternates::ensure_no_live_borrowers(
        "delete an obliterated object's payload",
        // A borrowed store is a conflict, not a missing confirmation.
        StableErrorCode::ConflictOperationBlocked,
    )?;
    // Local loose file.
    if let Some(path) = loose_payload_path(hash)
        && let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(
            CliError::fatal(format!("failed to delete loose payload for {hash}: {e}"))
                .with_stable_code(StableErrorCode::IoWriteFailed),
        );
    }
    // Durable tier + in-memory LRU (no-op for a local-only store).
    let storage = util::objects_storage();
    storage.delete_payload(hash).await.map_err(|e| {
        CliError::fatal(format!(
            "failed to purge durable-tier payload for {hash}: {e}"
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    Ok(())
}

/// Re-run the payload-delete + finalize tail for any `obliterating` rows left
/// by a crash (idempotent). Returns how many were completed.
pub async fn recover_incomplete() -> CliResult<usize> {
    let incomplete = ObliterationStore::incomplete()
        .await
        .map_err(|e| CliError::fatal(format!("obliteration recovery scan failed: {e}")))?;
    let mut completed = 0usize;
    for oid in incomplete {
        let Ok(hash) = ObjectHash::from_str(&oid) else {
            continue;
        };
        // Mandatory durable audit BEFORE the destructive delete (Codex P1): a
        // recovery that deletes a payload must be recorded, and a failure to
        // audit aborts before touching the payload.
        let now = chrono::Utc::now().to_rfc3339();
        audit::append(&audit::AuditRecord {
            at: now,
            operation: "obliterate-recover".to_string(),
            oid: hash.to_string(),
            approval_source: "recovery".to_string(),
            actor: "recover".to_string(),
            reason: None,
            outcome: "payload_deleted".to_string(),
        })?;
        delete_payload(&hash).await?;
        ObliterationStore::mark_obliterated(&hash)
            .await
            .map_err(|e| CliError::fatal(format!("obliteration recovery finalize failed: {e}")))?;
        completed += 1;
    }
    Ok(completed)
}

#[cfg(test)]
mod tests {
    use git_internal::internal::object::types::ObjectType;

    use super::*;
    use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

    fn blob_oid(bytes: &[u8]) -> ObjectHash {
        ObjectHash::from_type_and_data(ObjectType::Blob, bytes)
    }

    /// State machine: (no row)=Live -> obliterating -> obliterated; tombstone
    /// is permanent and both states count as intentionally-absent.
    #[tokio::test]
    #[serial_test::serial]
    async fn state_machine_and_tombstone_permanence() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;

        let oid = blob_oid(b"payload");
        // Live = no row.
        assert!(!ObliterationStore::is_tombstoned(&oid).await);
        assert!(
            ObliterationStore::lookup(&oid)
                .await
                .expect("lookup")
                .is_none()
        );

        // Live -> Obliterating.
        ObliterationStore::begin_obliterating(&oid, Some("gdpr"), Some("cli"), Some("human"))
            .await
            .expect("begin");
        let tomb = ObliterationStore::lookup(&oid)
            .await
            .expect("lookup")
            .expect("row");
        assert!(tomb.is_obliterating());
        assert!(ObliterationStore::is_tombstoned(&oid).await);
        assert_eq!(
            ObliterationStore::incomplete().await.expect("inc"),
            vec![oid.to_string()]
        );

        // Obliterating -> Obliterated.
        ObliterationStore::mark_obliterated(&oid)
            .await
            .expect("mark");
        let tomb = ObliterationStore::lookup(&oid)
            .await
            .expect("lookup")
            .expect("row");
        assert!(!tomb.is_obliterating());
        assert!(
            ObliterationStore::is_tombstoned(&oid).await,
            "obliterated still tombstoned"
        );
        assert!(
            ObliterationStore::incomplete()
                .await
                .expect("inc")
                .is_empty()
        );

        // begin is idempotent on the UNIQUE key (no duplicate row / no error).
        ObliterationStore::begin_obliterating(&oid, None, None, None)
            .await
            .expect("idempotent");
    }

    /// The sync snapshot reflects the store after a refresh.
    #[tokio::test]
    #[serial_test::serial]
    async fn snapshot_reflects_store() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;
        let oid = blob_oid(b"snap");
        refresh_snapshot().await;
        assert!(!is_tombstoned_cached(&oid));
        ObliterationStore::begin_obliterating(&oid, None, None, None)
            .await
            .expect("begin");
        refresh_snapshot().await;
        assert!(
            is_tombstoned_cached(&oid),
            "snapshot sees the new tombstone"
        );
    }
}
