use std::{
    collections::HashSet,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use git_internal::hash::ObjectHash;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement};
use thiserror::Error;
use uuid::Uuid;

use super::{
    domain::{EpisodeRoot, EpisodeRootKind},
    job_state::{
        COMPILE_JOB_LEASE_MS, COMPILE_JOB_MAX_RETRIES, CompileFailureClass,
        CompileJobCompletionOutcome, CompileJobKey, CompileJobLease, CompileJobMutationOutcome,
        CompileJobStateError, CompileJobStateErrorKind, StableJobFailure, retry_delay_ms,
        validate_lease_owner,
    },
};
use crate::internal::{ai::keyed_digest::SourceInputFingerprint, db};

const INTENT_SOURCE_REF: &str = "libra/intent";
const MEMORY_SOURCE_REF: &str = "libra/memory/repo";

pub(crate) struct ObservedRoot {
    root: EpisodeRoot,
    terminal_source_oid: ObjectHash,
    input_fingerprint: SourceInputFingerprint,
}

impl ObservedRoot {
    pub(crate) const fn new(
        root: EpisodeRoot,
        terminal_source_oid: ObjectHash,
        input_fingerprint: SourceInputFingerprint,
    ) -> Self {
        Self {
            root,
            terminal_source_oid,
            input_fingerprint,
        }
    }

    fn kind_label(&self) -> &'static str {
        match self.root.kind() {
            EpisodeRootKind::Task => "task",
            EpisodeRootKind::Intent => "intent",
        }
    }
}

pub(crate) struct ObservationBatch {
    scope_key: String,
    source_ref_name: String,
    expected_cursor: Option<ObjectHash>,
    scanned_through_oid: ObjectHash,
    roots: Vec<ObservedRoot>,
}

impl ObservationBatch {
    pub(crate) fn new(
        scope_key: impl Into<String>,
        source_ref_name: impl Into<String>,
        expected_cursor: Option<ObjectHash>,
        scanned_through_oid: ObjectHash,
        roots: Vec<ObservedRoot>,
    ) -> Result<Self, RecordObservationError> {
        let scope_key = scope_key.into();
        if !valid_scope_key(&scope_key) {
            return Err(RecordObservationError::new(
                RecordObservationErrorKind::InvalidScope,
            ));
        }

        let source_ref_name = source_ref_name.into();
        if !matches!(
            source_ref_name.as_str(),
            INTENT_SOURCE_REF | MEMORY_SOURCE_REF
        ) {
            return Err(RecordObservationError::new(
                RecordObservationErrorKind::InvalidSourceRef,
            ));
        }

        if expected_cursor == Some(scanned_through_oid) && !roots.is_empty() {
            return Err(RecordObservationError::new(
                RecordObservationErrorKind::InvalidCursorRange,
            ));
        }

        let mut root_keys = HashSet::with_capacity(roots.len());
        for root in &roots {
            if !root_keys.insert((root.kind_label(), root.root.id())) {
                return Err(RecordObservationError::new(
                    RecordObservationErrorKind::DuplicateRoot,
                ));
            }
        }

        Ok(Self {
            scope_key,
            source_ref_name,
            expected_cursor,
            scanned_through_oid,
            roots,
        })
    }
}

fn valid_scope_key(scope_key: &str) -> bool {
    !scope_key.is_empty()
        && scope_key.len() <= 512
        && scope_key.trim() == scope_key
        && !scope_key.chars().any(char::is_control)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObservationBatchOutcome {
    Recorded { observed_roots: usize },
    AlreadyRecorded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecordObservationErrorKind {
    InvalidScope,
    InvalidSourceRef,
    InvalidCursorRange,
    DuplicateRoot,
    CursorConflict,
    SourceMismatch,
    Storage,
}

#[derive(Debug, Error)]
#[error("Memory compiler observation failed ({kind:?})")]
pub(crate) struct RecordObservationError {
    kind: RecordObservationErrorKind,
}

impl RecordObservationError {
    const fn new(kind: RecordObservationErrorKind) -> Self {
        Self { kind }
    }

    fn storage() -> Self {
        Self::new(RecordObservationErrorKind::Storage)
    }

    pub(crate) const fn kind(&self) -> RecordObservationErrorKind {
        self.kind
    }
}

pub(crate) async fn record_observation_batch(
    database: &DatabaseConnection,
    batch: ObservationBatch,
) -> Result<ObservationBatchOutcome, RecordObservationError> {
    let transaction = db::begin_write_transaction(database)
        .await
        .map_err(|_| RecordObservationError::storage())?;
    let observed_at = epoch_millis()?;
    match record_observation_batch_in_transaction(&transaction, batch, observed_at).await {
        Ok(outcome) => {
            transaction
                .commit()
                .await
                .map_err(|_| RecordObservationError::storage())?;
            Ok(outcome)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

async fn record_observation_batch_in_transaction(
    transaction: &DatabaseTransaction,
    batch: ObservationBatch,
    observed_at: i64,
) -> Result<ObservationBatchOutcome, RecordObservationError> {
    let scanned_through_oid = batch.scanned_through_oid.to_string();
    let expected_cursor = batch.expected_cursor.map(|cursor| cursor.to_string());
    let current_cursor =
        read_observer_cursor(transaction, &batch.scope_key, &batch.source_ref_name).await?;

    if current_cursor.as_deref() == Some(scanned_through_oid.as_str()) {
        return Ok(ObservationBatchOutcome::AlreadyRecorded);
    }

    let cursor_matches = match (current_cursor.as_deref(), expected_cursor.as_deref()) {
        (None, None) => true,
        (Some(current), Some(expected)) => current == expected,
        _ => false,
    };
    if !cursor_matches {
        return Err(RecordObservationError::new(
            RecordObservationErrorKind::CursorConflict,
        ));
    }

    let observed_roots = batch.roots.len();
    for root in batch.roots {
        observe_root(transaction, &batch.scope_key, root, observed_at).await?;
    }

    match current_cursor {
        None => {
            insert_observer_cursor(
                transaction,
                &batch.scope_key,
                &batch.source_ref_name,
                &scanned_through_oid,
                observed_at,
            )
            .await?
        }
        Some(current) => {
            advance_observer_cursor(
                transaction,
                &batch.scope_key,
                &batch.source_ref_name,
                &current,
                &scanned_through_oid,
                observed_at,
            )
            .await?
        }
    }

    Ok(ObservationBatchOutcome::Recorded { observed_roots })
}

#[cfg(test)]
async fn record_observation_batch_at(
    database: &DatabaseConnection,
    batch: ObservationBatch,
    observed_at: i64,
) -> Result<ObservationBatchOutcome, RecordObservationError> {
    let transaction = db::begin_write_transaction(database)
        .await
        .map_err(|_| RecordObservationError::storage())?;
    match record_observation_batch_in_transaction(&transaction, batch, observed_at).await {
        Ok(outcome) => {
            transaction
                .commit()
                .await
                .map_err(|_| RecordObservationError::storage())?;
            Ok(outcome)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

fn epoch_millis() -> Result<i64, RecordObservationError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RecordObservationError::storage())?;
    i64::try_from(elapsed.as_millis()).map_err(|_| RecordObservationError::storage())
}

pub(crate) async fn load_observer_cursor(
    database: &DatabaseConnection,
    scope_key: &str,
    source_ref_name: &str,
) -> Result<Option<ObjectHash>, RecordObservationError> {
    if !valid_scope_key(scope_key) {
        return Err(RecordObservationError::new(
            RecordObservationErrorKind::InvalidScope,
        ));
    }
    if !matches!(source_ref_name, INTENT_SOURCE_REF | MEMORY_SOURCE_REF) {
        return Err(RecordObservationError::new(
            RecordObservationErrorKind::InvalidSourceRef,
        ));
    }
    database
        .query_one_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT scanned_through_oid FROM memory_compile_observer_state
             WHERE scope_key = ? AND source_ref_name = ?",
            [scope_key.into(), source_ref_name.into()],
        ))
        .await
        .map_err(|_| RecordObservationError::storage())?
        .map(|row| {
            let value: String = row
                .try_get("", "scanned_through_oid")
                .map_err(|_| RecordObservationError::storage())?;
            ObjectHash::from_str(&value).map_err(|_| RecordObservationError::storage())
        })
        .transpose()
}

/// Prove that a compiler lease still owns its generation while an enclosing
/// SQLite write transaction is held. MemoryWriter uses this as a companion
/// precondition, so lease takeover and the authoritative ref CAS cannot race.
pub(super) async fn verify_job_lease(
    transaction: &DatabaseTransaction,
    lease: &CompileJobLease,
) -> Result<bool, CompileJobStateError> {
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT 1 AS valid
             FROM memory_compile_job
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?
               AND state = 'inflight' AND lease_owner = ? AND lease_fence = ?
               AND processed_generation < ? AND observed_generation >= ?
               AND lease_expires_at > CAST(strftime('%s', 'now') AS INTEGER) * 1000",
            [
                lease.key().scope_key().into(),
                lease.key().root_kind_label().into(),
                lease.key().root().id().into(),
                lease.owner().into(),
                lease.fence().into(),
                lease.target_generation().into(),
                lease.target_generation().into(),
            ],
        ))
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    Ok(row.is_some())
}

pub(crate) async fn load_terminal_job_source(
    database: &DatabaseConnection,
    key: &CompileJobKey,
) -> Result<Option<ObjectHash>, CompileJobStateError> {
    database
        .query_one_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT terminal_source_oid FROM memory_compile_job
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?",
            [
                key.scope_key().into(),
                key.root_kind_label().into(),
                key.root().id().into(),
            ],
        ))
        .await
        .map_err(|_| CompileJobStateError::storage())?
        .map(|row| {
            let value: String = row
                .try_get("", "terminal_source_oid")
                .map_err(|_| CompileJobStateError::storage())?;
            ObjectHash::from_str(&value)
                .map_err(|_| CompileJobStateError::new(CompileJobStateErrorKind::CorruptState))
        })
        .transpose()
}

async fn read_observer_cursor(
    transaction: &DatabaseTransaction,
    scope_key: &str,
    source_ref_name: &str,
) -> Result<Option<String>, RecordObservationError> {
    transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT scanned_through_oid FROM memory_compile_observer_state
             WHERE scope_key = ? AND source_ref_name = ?",
            [scope_key.into(), source_ref_name.into()],
        ))
        .await
        .map_err(|_| RecordObservationError::storage())?
        .map(|row| {
            row.try_get("", "scanned_through_oid")
                .map_err(|_| RecordObservationError::storage())
        })
        .transpose()
}

async fn insert_observer_cursor(
    transaction: &DatabaseTransaction,
    scope_key: &str,
    source_ref_name: &str,
    scanned_through_oid: &str,
    observed_at: i64,
) -> Result<(), RecordObservationError> {
    transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "INSERT INTO memory_compile_observer_state
             (scope_key, source_ref_name, scanned_through_oid, updated_at)
             VALUES (?, ?, ?, ?)",
            [
                scope_key.into(),
                source_ref_name.into(),
                scanned_through_oid.into(),
                observed_at.into(),
            ],
        ))
        .await
        .map_err(|_| RecordObservationError::storage())?;
    Ok(())
}

async fn advance_observer_cursor(
    transaction: &DatabaseTransaction,
    scope_key: &str,
    source_ref_name: &str,
    expected_cursor: &str,
    scanned_through_oid: &str,
    observed_at: i64,
) -> Result<(), RecordObservationError> {
    let result = transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "UPDATE memory_compile_observer_state
             SET scanned_through_oid = ?, updated_at = ?
             WHERE scope_key = ? AND source_ref_name = ? AND scanned_through_oid = ?",
            [
                scanned_through_oid.into(),
                observed_at.into(),
                scope_key.into(),
                source_ref_name.into(),
                expected_cursor.into(),
            ],
        ))
        .await
        .map_err(|_| RecordObservationError::storage())?;
    if result.rows_affected() != 1 {
        return Err(RecordObservationError::new(
            RecordObservationErrorKind::CursorConflict,
        ));
    }
    Ok(())
}

struct StoredJob {
    terminal_source_oid: String,
    input_fingerprint_version: i64,
    input_fingerprint_key_id: String,
    input_fingerprint_digest: String,
    observed_generation: i64,
    state: String,
}

async fn observe_root(
    transaction: &DatabaseTransaction,
    scope_key: &str,
    root: ObservedRoot,
    observed_at: i64,
) -> Result<(), RecordObservationError> {
    let root_kind = root.kind_label();
    let root_id = root.root.id();
    let existing = transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT terminal_source_oid, input_fingerprint_version,
                    input_fingerprint_key_id, input_fingerprint_digest,
                    observed_generation, state
             FROM memory_compile_job
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?",
            [scope_key.into(), root_kind.into(), root_id.into()],
        ))
        .await
        .map_err(|_| RecordObservationError::storage())?
        .map(|row| {
            Ok::<_, RecordObservationError>(StoredJob {
                terminal_source_oid: row
                    .try_get("", "terminal_source_oid")
                    .map_err(|_| RecordObservationError::storage())?,
                input_fingerprint_version: row
                    .try_get("", "input_fingerprint_version")
                    .map_err(|_| RecordObservationError::storage())?,
                input_fingerprint_key_id: row
                    .try_get("", "input_fingerprint_key_id")
                    .map_err(|_| RecordObservationError::storage())?,
                input_fingerprint_digest: row
                    .try_get("", "input_fingerprint_digest")
                    .map_err(|_| RecordObservationError::storage())?,
                observed_generation: row
                    .try_get("", "observed_generation")
                    .map_err(|_| RecordObservationError::storage())?,
                state: row
                    .try_get("", "state")
                    .map_err(|_| RecordObservationError::storage())?,
            })
        })
        .transpose()?;

    let terminal_source_oid = root.terminal_source_oid.to_string();
    let fingerprint_version = i64::from(root.input_fingerprint.version());
    let fingerprint_key_id = root.input_fingerprint.key_id().to_string();
    let fingerprint_digest = root.input_fingerprint.digest_hex();

    let Some(existing) = existing else {
        transaction
            .execute_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "INSERT INTO memory_compile_job (
                    scope_key, root_kind, root_id, terminal_source_oid,
                    input_fingerprint_version, input_fingerprint_key_id,
                    input_fingerprint_digest, created_at, updated_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                [
                    scope_key.into(),
                    root_kind.into(),
                    root_id.into(),
                    terminal_source_oid.into(),
                    fingerprint_version.into(),
                    fingerprint_key_id.into(),
                    fingerprint_digest.into(),
                    observed_at.into(),
                    observed_at.into(),
                ],
            ))
            .await
            .map_err(|_| RecordObservationError::storage())?;
        return Ok(());
    };

    let same_fingerprint = existing.input_fingerprint_version == fingerprint_version
        && existing.input_fingerprint_key_id == fingerprint_key_id
        && existing.input_fingerprint_digest == fingerprint_digest;
    if same_fingerprint {
        if existing.terminal_source_oid == terminal_source_oid {
            return Ok(());
        }
        return Err(RecordObservationError::new(
            RecordObservationErrorKind::SourceMismatch,
        ));
    }

    let next_generation = existing
        .observed_generation
        .checked_add(1)
        .ok_or_else(RecordObservationError::storage)?;
    let (sql, values) = if existing.state == "inflight" {
        (
            "UPDATE memory_compile_job
             SET terminal_source_oid = ?, input_fingerprint_version = ?,
                 input_fingerprint_key_id = ?, input_fingerprint_digest = ?,
                 observed_generation = ?, updated_at = ?
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?",
            vec![
                terminal_source_oid.into(),
                fingerprint_version.into(),
                fingerprint_key_id.into(),
                fingerprint_digest.into(),
                next_generation.into(),
                observed_at.into(),
                scope_key.into(),
                root_kind.into(),
                root_id.into(),
            ],
        )
    } else {
        (
            "UPDATE memory_compile_job
             SET terminal_source_oid = ?, input_fingerprint_version = ?,
                 input_fingerprint_key_id = ?, input_fingerprint_digest = ?,
                 observed_generation = ?, state = 'dirty',
                 lease_owner = NULL, lease_expires_at = NULL,
                 retry_count = 0, next_retry_at = NULL,
                 last_error_code = NULL, last_error_summary = NULL,
                 updated_at = ?
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?",
            vec![
                terminal_source_oid.into(),
                fingerprint_version.into(),
                fingerprint_key_id.into(),
                fingerprint_digest.into(),
                next_generation.into(),
                observed_at.into(),
                scope_key.into(),
                root_kind.into(),
                root_id.into(),
            ],
        )
    };
    let result = transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            sql,
            values,
        ))
        .await
        .map_err(|_| RecordObservationError::storage())?;
    if result.rows_affected() != 1 {
        return Err(RecordObservationError::storage());
    }
    Ok(())
}

struct ClaimableJob {
    key: CompileJobKey,
    terminal_source_oid: ObjectHash,
    input_fingerprint: SourceInputFingerprint,
    observed_generation: i64,
    next_fence: i64,
}

/// Claim at most one runnable compiler generation for a repository scope.
///
/// Expired leases are reclaimed by incrementing the persisted fence. Every
/// later mutation includes owner + fence, so an old process cannot complete,
/// fail, or release work after takeover.
pub(crate) async fn claim_next_job(
    database: &DatabaseConnection,
    scope_key: &str,
    owner: &str,
    now_ms: i64,
) -> Result<Option<CompileJobLease>, CompileJobStateError> {
    let scope_probe = EpisodeRoot::task("scope-validation")
        .map_err(|_| CompileJobStateError::new(CompileJobStateErrorKind::InvalidInput))?;
    CompileJobKey::new(scope_key, scope_probe)?;
    validate_lease_owner(owner)?;
    if now_ms < 0 {
        return Err(CompileJobStateError::new(
            CompileJobStateErrorKind::InvalidInput,
        ));
    }

    let transaction = db::begin_write_transaction(database)
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    let result = claim_next_job_in_transaction(&transaction, scope_key, owner, now_ms).await;
    match result {
        Ok(lease) => {
            transaction
                .commit()
                .await
                .map_err(|_| CompileJobStateError::storage())?;
            Ok(lease)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

async fn claim_next_job_in_transaction(
    transaction: &DatabaseTransaction,
    scope_key: &str,
    owner: &str,
    now_ms: i64,
) -> Result<Option<CompileJobLease>, CompileJobStateError> {
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT root_kind, root_id, terminal_source_oid,
                    input_fingerprint_version, input_fingerprint_key_id,
                    input_fingerprint_digest, observed_generation, lease_fence
             FROM memory_compile_job
             WHERE scope_key = ? AND processed_generation < observed_generation
               AND ((state = 'dirty' AND (next_retry_at IS NULL OR next_retry_at <= ?))
                    OR (state = 'inflight' AND lease_expires_at <= ?))
             ORDER BY updated_at ASC, root_kind ASC, root_id ASC
             LIMIT 1",
            [scope_key.into(), now_ms.into(), now_ms.into()],
        ))
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    let Some(row) = row else {
        return Ok(None);
    };

    let root_kind: String = row
        .try_get("", "root_kind")
        .map_err(|_| CompileJobStateError::storage())?;
    let root_id: String = row
        .try_get("", "root_id")
        .map_err(|_| CompileJobStateError::storage())?;
    let root = match root_kind.as_str() {
        "task" => EpisodeRoot::task(root_id),
        "intent" => EpisodeRoot::intent(root_id),
        _ => {
            return Err(CompileJobStateError::new(
                CompileJobStateErrorKind::CorruptState,
            ));
        }
    }
    .map_err(|_| CompileJobStateError::new(CompileJobStateErrorKind::CorruptState))?;
    let key = CompileJobKey::new(scope_key, root)
        .map_err(|_| CompileJobStateError::new(CompileJobStateErrorKind::CorruptState))?;
    let terminal_source_oid: String = row
        .try_get("", "terminal_source_oid")
        .map_err(|_| CompileJobStateError::storage())?;
    let terminal_source_oid = ObjectHash::from_str(&terminal_source_oid)
        .map_err(|_| CompileJobStateError::new(CompileJobStateErrorKind::CorruptState))?;
    let fingerprint_version: i64 = row
        .try_get("", "input_fingerprint_version")
        .map_err(|_| CompileJobStateError::storage())?;
    let fingerprint_key_id: String = row
        .try_get("", "input_fingerprint_key_id")
        .map_err(|_| CompileJobStateError::storage())?;
    let fingerprint_digest: String = row
        .try_get("", "input_fingerprint_digest")
        .map_err(|_| CompileJobStateError::storage())?;
    let input_fingerprint = SourceInputFingerprint::from_parts(
        u8::try_from(fingerprint_version)
            .map_err(|_| CompileJobStateError::new(CompileJobStateErrorKind::CorruptState))?,
        Uuid::parse_str(&fingerprint_key_id)
            .map_err(|_| CompileJobStateError::new(CompileJobStateErrorKind::CorruptState))?,
        fingerprint_digest,
    )
    .map_err(|_| CompileJobStateError::new(CompileJobStateErrorKind::CorruptState))?;
    let observed_generation: i64 = row
        .try_get("", "observed_generation")
        .map_err(|_| CompileJobStateError::storage())?;
    let current_fence: i64 = row
        .try_get("", "lease_fence")
        .map_err(|_| CompileJobStateError::storage())?;
    let next_fence = current_fence
        .checked_add(1)
        .ok_or_else(CompileJobStateError::storage)?;
    let claimable = ClaimableJob {
        key,
        terminal_source_oid,
        input_fingerprint,
        observed_generation,
        next_fence,
    };

    let lease_expires_at = now_ms.saturating_add(COMPILE_JOB_LEASE_MS);
    let result = transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "UPDATE memory_compile_job
             SET state = 'inflight', lease_owner = ?, lease_fence = ?,
                 lease_expires_at = ?, next_retry_at = NULL, updated_at = ?
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?
               AND processed_generation < observed_generation
               AND ((state = 'dirty' AND (next_retry_at IS NULL OR next_retry_at <= ?))
                    OR (state = 'inflight' AND lease_expires_at <= ?))",
            [
                owner.into(),
                claimable.next_fence.into(),
                lease_expires_at.into(),
                now_ms.into(),
                claimable.key.scope_key().into(),
                claimable.key.root_kind_label().into(),
                claimable.key.root().id().into(),
                now_ms.into(),
                now_ms.into(),
            ],
        ))
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    if result.rows_affected() != 1 {
        return Ok(None);
    }

    CompileJobLease::from_persisted(
        claimable.key,
        owner.to_owned(),
        claimable.next_fence,
        claimable.observed_generation,
        claimable.terminal_source_oid,
        claimable.input_fingerprint,
    )
    .map(Some)
}

pub(crate) async fn complete_job(
    database: &DatabaseConnection,
    lease: &CompileJobLease,
    now_ms: i64,
) -> Result<CompileJobCompletionOutcome, CompileJobStateError> {
    if now_ms < 0 {
        return Err(CompileJobStateError::new(
            CompileJobStateErrorKind::InvalidInput,
        ));
    }
    let transaction = db::begin_write_transaction(database)
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT observed_generation FROM memory_compile_job
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?
               AND state = 'inflight' AND lease_owner = ? AND lease_fence = ?",
            lease_identity_values(lease),
        ))
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    let Some(row) = row else {
        transaction.rollback().await.ok();
        return Ok(CompileJobCompletionOutcome::FencedOut);
    };
    let observed_generation: i64 = row
        .try_get("", "observed_generation")
        .map_err(|_| CompileJobStateError::storage())?;
    if observed_generation < lease.target_generation() {
        transaction.rollback().await.ok();
        return Err(CompileJobStateError::new(
            CompileJobStateErrorKind::CorruptState,
        ));
    }
    let clean = observed_generation == lease.target_generation();
    let state = if clean { "idle" } else { "dirty" };
    let result = transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "UPDATE memory_compile_job
             SET processed_generation = ?, state = ?, lease_owner = NULL,
                 lease_expires_at = NULL, retry_count = 0, next_retry_at = NULL,
                 last_error_code = NULL, last_error_summary = NULL, updated_at = ?
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?
               AND state = 'inflight' AND lease_owner = ? AND lease_fence = ?",
            [
                lease.target_generation().into(),
                state.into(),
                now_ms.into(),
                lease.key().scope_key().into(),
                lease.key().root_kind_label().into(),
                lease.key().root().id().into(),
                lease.owner().into(),
                lease.fence().into(),
            ],
        ))
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    if result.rows_affected() != 1 {
        transaction.rollback().await.ok();
        return Ok(CompileJobCompletionOutcome::FencedOut);
    }
    transaction
        .commit()
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    Ok(if clean {
        CompileJobCompletionOutcome::Clean
    } else {
        CompileJobCompletionOutcome::NewGenerationPending
    })
}

pub(crate) async fn record_job_failure(
    database: &DatabaseConnection,
    lease: &CompileJobLease,
    failure: &StableJobFailure,
    now_ms: i64,
) -> Result<CompileJobMutationOutcome, CompileJobStateError> {
    if now_ms < 0 {
        return Err(CompileJobStateError::new(
            CompileJobStateErrorKind::InvalidInput,
        ));
    }
    let transaction = db::begin_write_transaction(database)
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT retry_count FROM memory_compile_job
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?
               AND state = 'inflight' AND lease_owner = ? AND lease_fence = ?",
            lease_identity_values(lease),
        ))
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    let Some(row) = row else {
        transaction.rollback().await.ok();
        return Ok(CompileJobMutationOutcome::FencedOut);
    };
    let retry_count: i64 = row
        .try_get("", "retry_count")
        .map_err(|_| CompileJobStateError::storage())?;
    let retry_count = retry_count
        .checked_add(1)
        .ok_or_else(CompileJobStateError::storage)?;
    let retry_count_u32 = u32::try_from(retry_count).unwrap_or(u32::MAX);
    let retry = failure.class() == CompileFailureClass::Transient
        && retry_count_u32 < COMPILE_JOB_MAX_RETRIES;
    let (state, next_retry_at) = if retry {
        (
            "dirty",
            Some(now_ms.saturating_add(retry_delay_ms(retry_count_u32))),
        )
    } else {
        ("failed", None)
    };
    let result = transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "UPDATE memory_compile_job
             SET state = ?, lease_owner = NULL, lease_expires_at = NULL,
                 retry_count = ?, next_retry_at = ?, last_error_code = ?,
                 last_error_summary = ?, updated_at = ?
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?
               AND state = 'inflight' AND lease_owner = ? AND lease_fence = ?",
            [
                state.into(),
                retry_count.into(),
                next_retry_at.into(),
                failure.code().into(),
                failure.summary().into(),
                now_ms.into(),
                lease.key().scope_key().into(),
                lease.key().root_kind_label().into(),
                lease.key().root().id().into(),
                lease.owner().into(),
                lease.fence().into(),
            ],
        ))
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    if result.rows_affected() != 1 {
        transaction.rollback().await.ok();
        return Ok(CompileJobMutationOutcome::FencedOut);
    }
    transaction
        .commit()
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    Ok(CompileJobMutationOutcome::Applied)
}

pub(crate) async fn release_job_dirty(
    database: &DatabaseConnection,
    lease: &CompileJobLease,
    now_ms: i64,
) -> Result<CompileJobMutationOutcome, CompileJobStateError> {
    if now_ms < 0 {
        return Err(CompileJobStateError::new(
            CompileJobStateErrorKind::InvalidInput,
        ));
    }
    let result = database
        .execute_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "UPDATE memory_compile_job
             SET state = 'dirty', lease_owner = NULL, lease_expires_at = NULL,
                 next_retry_at = NULL, updated_at = ?
             WHERE scope_key = ? AND root_kind = ? AND root_id = ?
               AND state = 'inflight' AND lease_owner = ? AND lease_fence = ?",
            [
                now_ms.into(),
                lease.key().scope_key().into(),
                lease.key().root_kind_label().into(),
                lease.key().root().id().into(),
                lease.owner().into(),
                lease.fence().into(),
            ],
        ))
        .await
        .map_err(|_| CompileJobStateError::storage())?;
    Ok(if result.rows_affected() == 1 {
        CompileJobMutationOutcome::Applied
    } else {
        CompileJobMutationOutcome::FencedOut
    })
}

fn lease_identity_values(lease: &CompileJobLease) -> Vec<sea_orm::Value> {
    vec![
        lease.key().scope_key().into(),
        lease.key().root_kind_label().into(),
        lease.key().root().id().into(),
        lease.owner().into(),
        lease.fence().into(),
    ]
}

#[cfg(test)]
mod tests {
    use git_internal::hash::ObjectHash;
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::{
        super::domain::EpisodeRoot, ObservationBatch, ObservationBatchOutcome, ObservedRoot,
        RecordObservationErrorKind, claim_next_job, complete_job, epoch_millis, record_job_failure,
        record_observation_batch, record_observation_batch_at, release_job_dirty, verify_job_lease,
    };
    use crate::internal::{
        ai::{
            keyed_digest::SourceInputFingerprint,
            memory::job_state::{
                CompileFailureClass, CompileJobCompletionOutcome, CompileJobMutationOutcome,
                StableJobFailure,
            },
        },
        db,
    };

    struct JobSnapshot {
        terminal_source_oid: String,
        fingerprint_digest: String,
        observed_generation: i64,
        processed_generation: i64,
        state: String,
        lease_owner: Option<String>,
        lease_fence: i64,
        lease_expires_at: Option<i64>,
        retry_count: i64,
        next_retry_at: Option<i64>,
        last_error_code: Option<String>,
        last_error_summary: Option<String>,
        updated_at: i64,
    }

    async fn memory_database() -> (TempDir, DatabaseConnection) {
        let directory = tempfile::tempdir().expect("temporary repository database directory");
        let path = directory.path().join("libra.db");
        let connection = db::create_database(&path.to_string_lossy())
            .await
            .expect("current Libra schema must initialize");
        (directory, connection)
    }

    fn oid(label: &[u8]) -> ObjectHash {
        ObjectHash::new(label)
    }

    fn fingerprint(fill: char) -> SourceInputFingerprint {
        SourceInputFingerprint::from_parts(
            1,
            Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000")
                .expect("fixed UUIDv4 must parse"),
            fill.to_string().repeat(64),
        )
        .expect("synthetic lowercase source-input fingerprint must validate")
    }

    fn observed_task(
        id: &str,
        terminal_source_oid: ObjectHash,
        input_fingerprint: SourceInputFingerprint,
    ) -> ObservedRoot {
        ObservedRoot::new(
            EpisodeRoot::task(id).expect("synthetic task root must validate"),
            terminal_source_oid,
            input_fingerprint,
        )
    }

    fn observed_intent(
        id: &str,
        terminal_source_oid: ObjectHash,
        input_fingerprint: SourceInputFingerprint,
    ) -> ObservedRoot {
        ObservedRoot::new(
            EpisodeRoot::intent(id).expect("synthetic intent root must validate"),
            terminal_source_oid,
            input_fingerprint,
        )
    }

    async fn job_snapshot(
        connection: &DatabaseConnection,
        root_kind: &str,
        root_id: &str,
    ) -> Option<JobSnapshot> {
        connection
            .query_one_raw(Statement::from_sql_and_values(
                connection.get_database_backend(),
                "SELECT terminal_source_oid, input_fingerprint_digest,
                        observed_generation, processed_generation, state,
                        lease_owner, lease_fence, lease_expires_at, retry_count,
                        next_retry_at, last_error_code, last_error_summary, updated_at
                 FROM memory_compile_job
                 WHERE scope_key = ? AND root_kind = ? AND root_id = ?",
                ["repo".into(), root_kind.into(), root_id.into()],
            ))
            .await
            .expect("query compiler job")
            .map(|row| JobSnapshot {
                terminal_source_oid: row.try_get("", "terminal_source_oid").unwrap(),
                fingerprint_digest: row.try_get("", "input_fingerprint_digest").unwrap(),
                observed_generation: row.try_get("", "observed_generation").unwrap(),
                processed_generation: row.try_get("", "processed_generation").unwrap(),
                state: row.try_get("", "state").unwrap(),
                lease_owner: row.try_get("", "lease_owner").unwrap(),
                lease_fence: row.try_get("", "lease_fence").unwrap(),
                lease_expires_at: row.try_get("", "lease_expires_at").unwrap(),
                retry_count: row.try_get("", "retry_count").unwrap(),
                next_retry_at: row.try_get("", "next_retry_at").unwrap(),
                last_error_code: row.try_get("", "last_error_code").unwrap(),
                last_error_summary: row.try_get("", "last_error_summary").unwrap(),
                updated_at: row.try_get("", "updated_at").unwrap(),
            })
    }

    async fn observer_cursor(connection: &DatabaseConnection) -> Option<String> {
        connection
            .query_one_raw(Statement::from_sql_and_values(
                connection.get_database_backend(),
                "SELECT scanned_through_oid FROM memory_compile_observer_state
                 WHERE scope_key = ? AND source_ref_name = ?",
                ["repo".into(), "libra/intent".into()],
            ))
            .await
            .expect("query observer cursor")
            .map(|row| row.try_get("", "scanned_through_oid").unwrap())
    }

    #[tokio::test]
    async fn observer_job_schema_transaction() {
        let (_directory, connection) = memory_database().await;
        let cursor_one = oid(b"observer-cursor-one");
        let cursor_two = oid(b"observer-cursor-two");
        let cursor_three = oid(b"observer-cursor-three");
        let task_source_one = oid(b"task-source-one");
        let task_source_two = oid(b"task-source-two");
        let intent_source = oid(b"intent-source");

        let first = ObservationBatch::new(
            "repo",
            "libra/intent",
            None,
            cursor_one,
            vec![
                observed_task("task-1", task_source_one, fingerprint('a')),
                observed_intent("intent-1", intent_source, fingerprint('b')),
            ],
        )
        .expect("first observation batch must validate");
        assert_eq!(
            record_observation_batch(&connection, first)
                .await
                .expect("first batch must commit"),
            ObservationBatchOutcome::Recorded { observed_roots: 2 }
        );
        assert_eq!(
            observer_cursor(&connection).await,
            Some(cursor_one.to_string())
        );

        let initial_task = job_snapshot(&connection, "task", "task-1")
            .await
            .expect("task job must exist");
        assert_eq!(
            initial_task.terminal_source_oid,
            task_source_one.to_string()
        );
        assert_eq!(initial_task.fingerprint_digest, "a".repeat(64));
        assert_eq!(initial_task.observed_generation, 1);
        assert_eq!(initial_task.processed_generation, 0);
        assert_eq!(initial_task.state, "dirty");
        assert_eq!(initial_task.lease_fence, 0);

        let retry = ObservationBatch::new(
            "repo",
            "libra/intent",
            None,
            cursor_one,
            vec![observed_task("task-1", task_source_one, fingerprint('a'))],
        )
        .expect("commit-outcome retry batch must validate");
        assert_eq!(
            record_observation_batch(&connection, retry)
                .await
                .expect("commit-outcome retry must succeed"),
            ObservationBatchOutcome::AlreadyRecorded
        );
        assert_eq!(
            job_snapshot(&connection, "task", "task-1")
                .await
                .expect("task job remains")
                .updated_at,
            initial_task.updated_at,
            "AlreadyRecorded must not touch jobs"
        );

        connection
            .execute_unprepared(
                "UPDATE memory_compile_job
                 SET retry_count = 2, next_retry_at = 5,
                     last_error_code = 'LBR-MEMORY-101',
                     last_error_summary = 'old generation'
                 WHERE scope_key = 'repo' AND root_kind = 'task' AND root_id = 'task-1'",
            )
            .await
            .expect("seed retry diagnostics");

        let changed = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(cursor_one),
            cursor_two,
            vec![observed_task("task-1", task_source_two, fingerprint('c'))],
        )
        .expect("changed observation batch must validate");
        assert_eq!(
            record_observation_batch(&connection, changed)
                .await
                .expect("changed batch must commit"),
            ObservationBatchOutcome::Recorded { observed_roots: 1 }
        );
        let changed_task = job_snapshot(&connection, "task", "task-1")
            .await
            .expect("changed task job must exist");
        assert_eq!(
            changed_task.terminal_source_oid,
            task_source_two.to_string()
        );
        assert_eq!(changed_task.fingerprint_digest, "c".repeat(64));
        assert_eq!(changed_task.observed_generation, 2);
        assert_eq!(changed_task.processed_generation, 0);
        assert_eq!(changed_task.state, "dirty");
        assert_eq!(changed_task.retry_count, 0);
        assert_eq!(changed_task.next_retry_at, None);
        assert_eq!(changed_task.last_error_code, None);
        assert_eq!(changed_task.last_error_summary, None);

        let empty = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(cursor_two),
            cursor_three,
            vec![],
        )
        .expect("empty scan batch must validate");
        assert_eq!(
            record_observation_batch(&connection, empty)
                .await
                .expect("empty scan must advance"),
            ObservationBatchOutcome::Recorded { observed_roots: 0 }
        );
        assert_eq!(
            observer_cursor(&connection).await,
            Some(cursor_three.to_string())
        );
    }

    #[tokio::test]
    async fn observer_job_same_input_across_cursor_preserves_completed_progress() {
        let (_directory, connection) = memory_database().await;
        let cursor_one = oid(b"same-input-cursor-one");
        let cursor_two = oid(b"same-input-cursor-two");
        let cursor_three = oid(b"same-input-cursor-three");
        let source_one = oid(b"same-input-source-one");
        let source_two = oid(b"same-input-source-two");

        let initial = ObservationBatch::new(
            "repo",
            "libra/intent",
            None,
            cursor_one,
            vec![observed_task("task-progress", source_one, fingerprint('a'))],
        )
        .expect("initial progress batch validates");
        record_observation_batch(&connection, initial)
            .await
            .expect("initial progress batch commits");
        connection
            .execute_unprepared(
                "UPDATE memory_compile_job
                 SET processed_generation = observed_generation, state = 'idle'
                 WHERE scope_key = 'repo' AND root_kind = 'task'
                   AND root_id = 'task-progress'",
            )
            .await
            .expect("mark the first generation processed");
        let completed = job_snapshot(&connection, "task", "task-progress")
            .await
            .expect("completed job exists");
        assert_eq!(completed.observed_generation, 1);
        assert_eq!(completed.processed_generation, 1);
        assert_eq!(completed.state, "idle");

        let same_input = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(cursor_one),
            cursor_two,
            vec![observed_task("task-progress", source_one, fingerprint('a'))],
        )
        .expect("same-input next interval validates");
        assert_eq!(
            record_observation_batch(&connection, same_input)
                .await
                .expect("same-input next interval advances only the cursor"),
            ObservationBatchOutcome::Recorded { observed_roots: 1 }
        );
        let unchanged = job_snapshot(&connection, "task", "task-progress")
            .await
            .expect("same-input job remains");
        assert_eq!(unchanged.observed_generation, 1);
        assert_eq!(unchanged.processed_generation, 1);
        assert_eq!(unchanged.state, "idle");
        assert_eq!(unchanged.terminal_source_oid, source_one.to_string());
        assert_eq!(unchanged.fingerprint_digest, "a".repeat(64));
        assert_eq!(unchanged.updated_at, completed.updated_at);
        assert_eq!(
            observer_cursor(&connection).await,
            Some(cursor_two.to_string())
        );

        let changed_input = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(cursor_two),
            cursor_three,
            vec![observed_task("task-progress", source_two, fingerprint('b'))],
        )
        .expect("changed-input next interval validates");
        record_observation_batch(&connection, changed_input)
            .await
            .expect("changed input schedules the next generation");
        let changed = job_snapshot(&connection, "task", "task-progress")
            .await
            .expect("changed-input job remains");
        assert_eq!(changed.observed_generation, 2);
        assert_eq!(
            changed.processed_generation, 1,
            "observing generation two must retain completed generation one"
        );
        assert_eq!(changed.state, "dirty");
        assert_eq!(changed.terminal_source_oid, source_two.to_string());
        assert_eq!(changed.fingerprint_digest, "b".repeat(64));
    }

    #[tokio::test]
    async fn observer_job_rejects_conflicts_atomically() {
        let (_directory, connection) = memory_database().await;
        let cursor_one = oid(b"atomic-cursor-one");
        let cursor_two = oid(b"atomic-cursor-two");
        let other_cursor = oid(b"atomic-other-cursor");
        let source_one = oid(b"atomic-source-one");
        let source_two = oid(b"atomic-source-two");

        let initial = ObservationBatch::new(
            "repo",
            "libra/intent",
            None,
            cursor_one,
            vec![observed_task("task-1", source_one, fingerprint('a'))],
        )
        .expect("initial batch validates");
        record_observation_batch(&connection, initial)
            .await
            .expect("initial batch commits");

        let mismatch = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(cursor_one),
            cursor_two,
            vec![
                observed_task("task-2", source_two, fingerprint('b')),
                observed_task("task-1", source_two, fingerprint('a')),
            ],
        )
        .expect("mismatch batch validates structurally");
        let Err(error) = record_observation_batch(&connection, mismatch).await else {
            panic!("same fingerprint with another source OID must fail");
        };
        assert_eq!(error.kind(), RecordObservationErrorKind::SourceMismatch);
        assert!(
            job_snapshot(&connection, "task", "task-2").await.is_none(),
            "earlier writes in the failed batch must roll back"
        );
        assert_eq!(
            observer_cursor(&connection).await,
            Some(cursor_one.to_string())
        );

        let conflict = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(other_cursor),
            cursor_two,
            vec![observed_task("task-3", source_two, fingerprint('c'))],
        )
        .expect("cursor-conflict batch validates structurally");
        let Err(error) = record_observation_batch(&connection, conflict).await else {
            panic!("stale expected cursor must fail");
        };
        assert_eq!(error.kind(), RecordObservationErrorKind::CursorConflict);
        assert!(job_snapshot(&connection, "task", "task-3").await.is_none());

        let Err(error) = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(cursor_two),
            cursor_two,
            vec![observed_task("task-4", source_two, fingerprint('d'))],
        ) else {
            panic!("non-empty zero-width scan must be rejected before SQL");
        };
        assert_eq!(error.kind(), RecordObservationErrorKind::InvalidCursorRange);

        let Err(error) = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(cursor_one),
            cursor_two,
            vec![
                observed_task("task-5", source_one, fingerprint('e')),
                observed_task("task-5", source_one, fingerprint('e')),
            ],
        ) else {
            panic!("duplicate roots must be rejected before SQL");
        };
        assert_eq!(error.kind(), RecordObservationErrorKind::DuplicateRoot);

        let Err(error) = ObservationBatch::new(
            "repo",
            "refs/heads/main",
            Some(cursor_one),
            cursor_two,
            vec![],
        ) else {
            panic!("unsupported source refs must be rejected");
        };
        assert_eq!(error.kind(), RecordObservationErrorKind::InvalidSourceRef);
    }

    #[tokio::test]
    async fn observer_job_preserves_inflight_lease_on_new_generation() {
        let (_directory, connection) = memory_database().await;
        let cursor_one = oid(b"lease-cursor-one");
        let cursor_two = oid(b"lease-cursor-two");
        let source_one = oid(b"lease-source-one");
        let source_two = oid(b"lease-source-two");

        let initial = ObservationBatch::new(
            "repo",
            "libra/intent",
            None,
            cursor_one,
            vec![observed_task("task-lease", source_one, fingerprint('a'))],
        )
        .expect("initial lease fixture validates");
        record_observation_batch(&connection, initial)
            .await
            .expect("initial lease fixture commits");
        connection
            .execute_unprepared(
                "UPDATE memory_compile_job
                 SET state = 'inflight', lease_owner = 'runner-a', lease_fence = 7,
                     lease_expires_at = 0, retry_count = 1,
                     last_error_code = 'LBR-MEMORY-102',
                     last_error_summary = 'pinned generation diagnostic'
                 WHERE scope_key = 'repo' AND root_kind = 'task' AND root_id = 'task-lease'",
            )
            .await
            .expect("seed an inflight lease whose deadline is already past");

        let changed = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(cursor_one),
            cursor_two,
            vec![observed_task("task-lease", source_two, fingerprint('b'))],
        )
        .expect("new generation validates");
        record_observation_batch(&connection, changed)
            .await
            .expect("new generation commits without judging lease time");

        let job = job_snapshot(&connection, "task", "task-lease")
            .await
            .expect("lease job remains");
        assert_eq!(job.observed_generation, 2);
        assert_eq!(job.processed_generation, 0);
        assert_eq!(job.terminal_source_oid, source_two.to_string());
        assert_eq!(job.fingerprint_digest, "b".repeat(64));
        assert_eq!(job.state, "inflight");
        assert_eq!(job.lease_owner.as_deref(), Some("runner-a"));
        assert_eq!(job.lease_fence, 7);
        assert_eq!(job.lease_expires_at, Some(0));
        assert_eq!(job.retry_count, 1);
        assert_eq!(job.last_error_code.as_deref(), Some("LBR-MEMORY-102"));
        assert_eq!(
            job.last_error_summary.as_deref(),
            Some("pinned generation diagnostic")
        );
    }

    #[tokio::test]
    async fn compiler_job_lease_fences_expired_owner() {
        let (_directory, connection) = memory_database().await;
        let now = epoch_millis().expect("read current test time");
        let cursor = oid(b"fence-cursor");
        let source = oid(b"fence-source");
        let batch = ObservationBatch::new(
            "repo",
            "libra/intent",
            None,
            cursor,
            vec![observed_task("task-fence", source, fingerprint('a'))],
        )
        .expect("lease observation validates");
        record_observation_batch_at(&connection, batch, 100)
            .await
            .expect("lease observation commits");

        let first = claim_next_job(&connection, "repo", "runner-a", now)
            .await
            .expect("first claim succeeds")
            .expect("first runner receives the job");
        assert_eq!(first.fence(), 1);
        assert!(
            claim_next_job(&connection, "repo", "runner-b", now + 1)
                .await
                .expect("contended claim succeeds")
                .is_none(),
            "an unexpired lease must exclude another runner"
        );

        let second = claim_next_job(
            &connection,
            "repo",
            "runner-b",
            now + super::super::job_state::COMPILE_JOB_LEASE_MS,
        )
        .await
        .expect("expired lease takeover succeeds")
        .expect("second runner reclaims the expired job");
        assert_eq!(second.fence(), 2);
        let transaction = db::begin_write_transaction(&connection)
            .await
            .expect("begin lease proof transaction");
        assert!(
            !verify_job_lease(&transaction, &first)
                .await
                .expect("stale lease proof is readable")
        );
        assert!(
            verify_job_lease(&transaction, &second)
                .await
                .expect("current lease proof is readable")
        );
        transaction
            .rollback()
            .await
            .expect("rollback read-only lease proof");
        assert_eq!(
            complete_job(
                &connection,
                &first,
                now + super::super::job_state::COMPILE_JOB_LEASE_MS + 1,
            )
            .await
            .expect("stale completion is a typed no-op"),
            CompileJobCompletionOutcome::FencedOut
        );
        assert_eq!(
            release_job_dirty(
                &connection,
                &first,
                now + super::super::job_state::COMPILE_JOB_LEASE_MS + 2,
            )
            .await
            .expect("stale release is a typed no-op"),
            CompileJobMutationOutcome::FencedOut
        );
        assert_eq!(
            complete_job(
                &connection,
                &second,
                now + super::super::job_state::COMPILE_JOB_LEASE_MS + 3,
            )
            .await
            .expect("new owner completes"),
            CompileJobCompletionOutcome::Clean
        );
        let job = job_snapshot(&connection, "task", "task-fence")
            .await
            .expect("completed job remains");
        assert_eq!(job.state, "idle");
        assert_eq!(job.processed_generation, 1);
        assert_eq!(job.lease_owner, None);
        assert_eq!(job.lease_fence, 2);
    }

    #[tokio::test]
    async fn compiler_job_completion_preserves_new_generation() {
        let (_directory, connection) = memory_database().await;
        let cursor_one = oid(b"generation-cursor-one");
        let cursor_two = oid(b"generation-cursor-two");
        let source_one = oid(b"generation-source-one");
        let source_two = oid(b"generation-source-two");
        let first = ObservationBatch::new(
            "repo",
            "libra/intent",
            None,
            cursor_one,
            vec![observed_task(
                "task-generation",
                source_one,
                fingerprint('a'),
            )],
        )
        .expect("first generation validates");
        record_observation_batch_at(&connection, first, 100)
            .await
            .expect("first generation commits");
        let lease = claim_next_job(&connection, "repo", "runner-a", 200)
            .await
            .expect("claim succeeds")
            .expect("first generation is runnable");
        assert_eq!(lease.target_generation(), 1);
        assert_eq!(lease.terminal_source_oid(), source_one);
        assert_eq!(lease.input_fingerprint().digest_hex(), "a".repeat(64));

        let second = ObservationBatch::new(
            "repo",
            "libra/intent",
            Some(cursor_one),
            cursor_two,
            vec![observed_task(
                "task-generation",
                source_two,
                fingerprint('b'),
            )],
        )
        .expect("second generation validates");
        record_observation_batch_at(&connection, second, 300)
            .await
            .expect("second generation is observed while generation one runs");
        assert_eq!(
            complete_job(&connection, &lease, 400)
                .await
                .expect("generation one completion succeeds"),
            CompileJobCompletionOutcome::NewGenerationPending
        );
        let job = job_snapshot(&connection, "task", "task-generation")
            .await
            .expect("generation job remains");
        assert_eq!(job.processed_generation, 1);
        assert_eq!(job.observed_generation, 2);
        assert_eq!(job.state, "dirty");
        assert_eq!(job.lease_owner, None);
    }

    #[tokio::test]
    async fn compiler_job_failure_retries_then_stabilizes_redacted_diagnostic() {
        let (_directory, connection) = memory_database().await;
        let cursor = oid(b"retry-cursor");
        let source = oid(b"retry-source");
        let batch = ObservationBatch::new(
            "repo",
            "libra/intent",
            None,
            cursor,
            vec![observed_task("task-retry", source, fingerprint('a'))],
        )
        .expect("retry observation validates");
        record_observation_batch_at(&connection, batch, 100)
            .await
            .expect("retry observation commits");

        let lease = claim_next_job(&connection, "repo", "runner-a", 200)
            .await
            .expect("claim succeeds")
            .expect("job is runnable");
        let transient = StableJobFailure::new(
            CompileFailureClass::Transient,
            "LBR-MEMORY-101",
            format!("provider timeout token ghp_{}", "a".repeat(40)),
        )
        .expect("transient failure validates");
        assert_eq!(
            record_job_failure(&connection, &lease, &transient, 300)
                .await
                .expect("transient failure records"),
            CompileJobMutationOutcome::Applied
        );
        let retrying = job_snapshot(&connection, "task", "task-retry")
            .await
            .expect("retrying job remains");
        assert_eq!(retrying.state, "dirty");
        assert_eq!(retrying.retry_count, 1);
        assert_eq!(retrying.next_retry_at, Some(800));
        assert_eq!(retrying.last_error_code.as_deref(), Some("LBR-MEMORY-101"));
        assert!(!retrying.last_error_summary.unwrap().contains("ghp_"));

        let retry_lease = claim_next_job(&connection, "repo", "runner-b", 800)
            .await
            .expect("retry claim succeeds")
            .expect("retry deadline makes the job runnable");
        let stable = StableJobFailure::new(
            CompileFailureClass::Stable,
            "LBR-MEMORY-103",
            "schema rejected",
        )
        .expect("stable failure validates");
        record_job_failure(&connection, &retry_lease, &stable, 900)
            .await
            .expect("stable failure records");
        let failed = job_snapshot(&connection, "task", "task-retry")
            .await
            .expect("failed job remains");
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.retry_count, 2);
        assert_eq!(failed.next_retry_at, None);
        assert_eq!(failed.last_error_code.as_deref(), Some("LBR-MEMORY-103"));
        assert!(
            claim_next_job(&connection, "repo", "runner-c", 1_000)
                .await
                .expect("failed claim query succeeds")
                .is_none(),
            "stable failure waits for a new observed generation"
        );
    }
}
