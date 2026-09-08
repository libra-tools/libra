//! SQLite owner for the shared context selection receipt ledger.

use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr, QueryResult, Statement,
};
use thiserror::Error;
use uuid::Uuid;

use super::receipt::{
    ContextSelectionReceiptDraftV1, ContextSelectionReceiptV1, PersistedReceiptFieldsV1,
    ReceiptReproducibilityState, ReceiptSourceKind,
};
use crate::internal::{ai::keyed_digest::RepositoryKeyedDigest, db, workspace::RepoIdentity};

const DEFAULT_RETENTION_DAYS: i64 = 30;
const MAX_RECEIPTS_PER_REPOSITORY: i64 = 10_000;

pub(crate) struct ReceiptStore<'database> {
    database: &'database DatabaseConnection,
    repository_id: String,
    digest_key_id: Uuid,
    _digest_provider: Arc<RepositoryKeyedDigest>,
    clock: fn() -> DateTime<Utc>,
}

impl<'database> ReceiptStore<'database> {
    pub(crate) async fn new(
        database: &'database DatabaseConnection,
        digest_provider: Arc<RepositoryKeyedDigest>,
    ) -> Result<Self, ReceiptStoreError> {
        Self::with_clock(database, digest_provider, Utc::now).await
    }

    async fn with_clock(
        database: &'database DatabaseConnection,
        digest_provider: Arc<RepositoryKeyedDigest>,
        clock: fn() -> DateTime<Utc>,
    ) -> Result<Self, ReceiptStoreError> {
        let repository_identity = RepoIdentity::resolve(database)
            .await
            .map_err(|_| ReceiptStoreError::identity())?;
        if repository_identity.as_str() != digest_provider.repository_id() {
            return Err(ReceiptStoreError::repository_mismatch());
        }
        digest_provider
            .validate_for_connection(database)
            .await
            .map_err(|_| ReceiptStoreError::digest_key_mismatch())?;
        let repository_id = repository_identity.as_str().to_string();
        let digest_key_id = digest_provider.key_id();
        Ok(Self {
            database,
            repository_id,
            digest_key_id,
            _digest_provider: digest_provider,
            clock,
        })
    }

    pub(crate) async fn append(
        &self,
        draft: ContextSelectionReceiptDraftV1,
    ) -> Result<ContextSelectionReceiptV1, ReceiptStoreError> {
        if draft.repository_id() != self.repository_id {
            return Err(ReceiptStoreError::repository_mismatch());
        }
        if draft.digest_key_id() != self.digest_key_id {
            return Err(ReceiptStoreError::digest_key_mismatch());
        }
        let now = (self.clock)();
        let receipt = ContextSelectionReceiptV1::from_draft(Uuid::now_v7(), now, draft);
        let transaction = db::begin_write_transaction(self.database)
            .await
            .map_err(|_| ReceiptStoreError::storage())?;
        let result = async {
            self.validate_transaction_binding(&transaction).await?;
            append_in_transaction(&transaction, &receipt).await
        }
        .await;
        match result {
            Ok(()) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| ReceiptStoreError::storage())?;
                Ok(receipt)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(_) => Err(ReceiptStoreError::storage()),
            },
        }
    }

    async fn validate_transaction_binding(
        &self,
        transaction: &DatabaseTransaction,
    ) -> Result<(), ReceiptStoreError> {
        let identity = RepoIdentity::resolve(transaction)
            .await
            .map_err(|_| ReceiptStoreError::identity())?;
        if identity.as_str() != self.repository_id
            || identity.as_str() != self._digest_provider.repository_id()
        {
            return Err(ReceiptStoreError::repository_mismatch());
        }
        self._digest_provider
            .validate_for_connection(transaction)
            .await
            .map_err(|_| ReceiptStoreError::digest_key_mismatch())
    }

    pub(crate) async fn lookup(
        &self,
        receipt_id: Uuid,
    ) -> Result<ReceiptLookup, ReceiptStoreError> {
        let row = self
            .database
            .query_one_raw(Statement::from_sql_and_values(
                self.database.get_database_backend(),
                "SELECT receipt_id, schema_version, source_kind, repository_id,
                        digest_key_id, principal_hmac, query_hmac, effective_at,
                        code_commit, full_branch_ref, source_heads_json,
                        projection_watermarks_json, policy_hash, selector_version,
                        token_budget, selected_json, omissions_json, bundle_hash,
                        reproducibility_state, frame_id, recorded_at
                   FROM context_selection_receipt
                  WHERE repository_id = ? AND receipt_id = ?",
                [
                    self.repository_id.as_str().into(),
                    receipt_id.to_string().into(),
                ],
            ))
            .await
            .map_err(|_| ReceiptStoreError::storage())?;
        if let Some(row) = row {
            return decode_receipt(row).map(|receipt| ReceiptLookup::Found(Box::new(receipt)));
        }

        let retention = self
            .database
            .query_one_raw(Statement::from_sql_and_values(
                self.database.get_database_backend(),
                "SELECT pruned_before
                   FROM context_selection_receipt_retention
                  WHERE repository_id = ?",
                [self.repository_id.as_str().into()],
            ))
            .await
            .map_err(|_| ReceiptStoreError::storage())?;
        let pruned_before = retention
            .map(|row| {
                row.try_get::<Option<String>>("", "pruned_before")
                    .map_err(|_| ReceiptStoreError::corrupt())
            })
            .transpose()?
            .flatten()
            .map(|value| parse_timestamp(&value))
            .transpose()?;
        if pruned_before.is_some_and(|watermark| {
            uuid_v7_timestamp(receipt_id).is_some_and(|timestamp| timestamp < watermark)
        }) {
            Ok(ReceiptLookup::Expired)
        } else {
            Ok(ReceiptLookup::NotFound)
        }
    }
}

async fn append_in_transaction(
    transaction: &DatabaseTransaction,
    receipt: &ContextSelectionReceiptV1,
) -> Result<(), ReceiptStoreError> {
    let source_heads = serde_json::to_string(&receipt.source_heads)
        .map_err(|_| ReceiptStoreError::serialization())?;
    let projection_watermarks = serde_json::to_string(&receipt.projection_watermarks)
        .map_err(|_| ReceiptStoreError::serialization())?;
    let selected =
        serde_json::to_string(&receipt.selected).map_err(|_| ReceiptStoreError::serialization())?;
    let omissions = serde_json::to_string(&receipt.omissions)
        .map_err(|_| ReceiptStoreError::serialization())?;
    let recorded_at = format_timestamp(receipt.recorded_at);
    let effective_at = format_timestamp(receipt.effective_at);
    let token_budget =
        i64::try_from(receipt.token_budget).map_err(|_| ReceiptStoreError::serialization())?;

    transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "INSERT INTO context_selection_receipt (
                receipt_id, schema_version, source_kind, repository_id,
                digest_key_id, principal_hmac, query_hmac, effective_at,
                code_commit, full_branch_ref, source_heads_json,
                projection_watermarks_json, policy_hash, selector_version,
                token_budget, selected_json, omissions_json, bundle_hash,
                reproducibility_state, frame_id, recorded_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                receipt.receipt_id.to_string().into(),
                i64::from(receipt.schema_version).into(),
                receipt.source_kind.as_str().into(),
                receipt.repository_id.as_str().into(),
                receipt.digest_key_id.to_string().into(),
                receipt.principal_hmac.as_str().into(),
                receipt.query_hmac.as_str().into(),
                effective_at.into(),
                receipt.code_commit.as_deref().into(),
                receipt.full_branch_ref.as_deref().into(),
                source_heads.into(),
                projection_watermarks.into(),
                receipt.policy_hash.as_str().into(),
                receipt.selector_version.as_str().into(),
                token_budget.into(),
                selected.into(),
                omissions.into(),
                receipt.bundle_hash.as_str().into(),
                receipt.reproducibility_state.as_str().into(),
                receipt.frame_id.map(|value| value.to_string()).into(),
                recorded_at.clone().into(),
            ],
        ))
        .await
        .map_err(|_| ReceiptStoreError::storage())?;

    prune_and_update_retention(
        transaction,
        &receipt.repository_id,
        receipt.recorded_at,
        &recorded_at,
    )
    .await
}

async fn prune_and_update_retention(
    transaction: &DatabaseTransaction,
    repository_id: &str,
    recorded_at: DateTime<Utc>,
    recorded_at_text: &str,
) -> Result<(), ReceiptStoreError> {
    let cutoff = format_timestamp(recorded_at - TimeDelta::days(DEFAULT_RETENTION_DAYS));
    let candidate = transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT COUNT(*) AS count, MAX(recorded_at) AS pruned_before
               FROM context_selection_receipt
              WHERE repository_id = ?
                AND receipt_id NOT IN (
                    SELECT receipt_id
                      FROM context_selection_receipt
                     WHERE repository_id = ? AND recorded_at >= ?
                     ORDER BY recorded_at DESC, receipt_id DESC
                     LIMIT 10000
                )",
            [
                repository_id.into(),
                repository_id.into(),
                cutoff.clone().into(),
            ],
        ))
        .await
        .map_err(|_| ReceiptStoreError::storage())?
        .ok_or_else(ReceiptStoreError::corrupt)?;
    let pruned_count: i64 = candidate
        .try_get("", "count")
        .map_err(|_| ReceiptStoreError::corrupt())?;
    let pruned_before: Option<String> = candidate
        .try_get("", "pruned_before")
        .map_err(|_| ReceiptStoreError::corrupt())?;
    if pruned_count > 0 {
        transaction
            .execute_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "DELETE FROM context_selection_receipt
                  WHERE repository_id = ?
                    AND receipt_id NOT IN (
                        SELECT receipt_id
                          FROM context_selection_receipt
                         WHERE repository_id = ? AND recorded_at >= ?
                         ORDER BY recorded_at DESC, receipt_id DESC
                         LIMIT 10000
                    )",
                [repository_id.into(), repository_id.into(), cutoff.into()],
            ))
            .await
            .map_err(|_| ReceiptStoreError::storage())?;
    }

    let retained_rows: i64 = transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT COUNT(*) AS count FROM context_selection_receipt
              WHERE repository_id = ?",
            [repository_id.into()],
        ))
        .await
        .map_err(|_| ReceiptStoreError::storage())?
        .ok_or_else(ReceiptStoreError::corrupt)?
        .try_get("", "count")
        .map_err(|_| ReceiptStoreError::corrupt())?;
    if retained_rows > MAX_RECEIPTS_PER_REPOSITORY {
        return Err(ReceiptStoreError::corrupt());
    }
    let last_pruned_at = pruned_before.as_ref().map(|_| recorded_at_text);
    transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "INSERT INTO context_selection_receipt_retention (
                repository_id, pruned_before, last_pruned_at, retained_rows
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(repository_id) DO UPDATE SET
                pruned_before = CASE
                    WHEN excluded.pruned_before IS NULL
                        THEN context_selection_receipt_retention.pruned_before
                    WHEN context_selection_receipt_retention.pruned_before IS NULL
                      OR context_selection_receipt_retention.pruned_before < excluded.pruned_before
                        THEN excluded.pruned_before
                    ELSE context_selection_receipt_retention.pruned_before
                END,
                last_pruned_at = CASE
                    WHEN excluded.pruned_before IS NULL
                        THEN context_selection_receipt_retention.last_pruned_at
                    ELSE excluded.last_pruned_at
                END,
                retained_rows = excluded.retained_rows",
            [
                repository_id.into(),
                pruned_before.into(),
                last_pruned_at.into(),
                retained_rows.into(),
            ],
        ))
        .await
        .map_err(|_| ReceiptStoreError::storage())?;
    Ok(())
}

fn decode_receipt(row: QueryResult) -> Result<ContextSelectionReceiptV1, ReceiptStoreError> {
    let receipt_id = parse_uuid(row.try_get("", "receipt_id")?)?;
    let schema_version = u32::try_from(row.try_get::<i64>("", "schema_version")?)
        .map_err(|_| ReceiptStoreError::corrupt())?;
    let source_kind = ReceiptSourceKind::parse(&row.try_get::<String>("", "source_kind")?)
        .ok_or_else(ReceiptStoreError::corrupt)?;
    let digest_key_id = parse_uuid(row.try_get("", "digest_key_id")?)?;
    let token_budget = u64::try_from(row.try_get::<i64>("", "token_budget")?)
        .map_err(|_| ReceiptStoreError::corrupt())?;
    let frame_id = row
        .try_get::<Option<String>>("", "frame_id")?
        .map(parse_uuid)
        .transpose()?;
    let reproducibility_state =
        ReceiptReproducibilityState::parse(&row.try_get::<String>("", "reproducibility_state")?)
            .ok_or_else(ReceiptStoreError::corrupt)?;

    ContextSelectionReceiptV1::from_persisted(PersistedReceiptFieldsV1 {
        receipt_id,
        schema_version,
        source_kind,
        repository_id: row.try_get("", "repository_id")?,
        digest_key_id,
        principal_hmac: row.try_get("", "principal_hmac")?,
        query_hmac: row.try_get("", "query_hmac")?,
        effective_at: parse_timestamp(&row.try_get::<String>("", "effective_at")?)?,
        code_commit: row.try_get("", "code_commit")?,
        full_branch_ref: row.try_get("", "full_branch_ref")?,
        source_heads: decode_json(&row.try_get::<String>("", "source_heads_json")?)?,
        projection_watermarks: decode_json(
            &row.try_get::<String>("", "projection_watermarks_json")?,
        )?,
        policy_hash: row.try_get("", "policy_hash")?,
        selector_version: row.try_get("", "selector_version")?,
        token_budget,
        selected: decode_json(&row.try_get::<String>("", "selected_json")?)?,
        omissions: decode_json(&row.try_get::<String>("", "omissions_json")?)?,
        bundle_hash: row.try_get("", "bundle_hash")?,
        reproducibility_state,
        frame_id,
        recorded_at: parse_timestamp(&row.try_get::<String>("", "recorded_at")?)?,
    })
    .map_err(|_| ReceiptStoreError::corrupt())
}

fn decode_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, ReceiptStoreError> {
    serde_json::from_str(value).map_err(|_| ReceiptStoreError::corrupt())
}

fn parse_uuid(value: String) -> Result<Uuid, ReceiptStoreError> {
    Uuid::parse_str(&value).map_err(|_| ReceiptStoreError::corrupt())
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, ReceiptStoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| ReceiptStoreError::corrupt())
}

fn format_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn uuid_v7_timestamp(value: Uuid) -> Option<DateTime<Utc>> {
    if value.get_version_num() != 7 {
        return None;
    }
    let bytes = value.as_bytes();
    let milliseconds = bytes[..6].iter().fold(0_u64, |accumulator, byte| {
        (accumulator << 8) | u64::from(*byte)
    });
    DateTime::from_timestamp_millis(i64::try_from(milliseconds).ok()?)
}

pub(crate) enum ReceiptLookup {
    Found(Box<ContextSelectionReceiptV1>),
    Expired,
    NotFound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiptStoreErrorKind {
    Identity,
    RepositoryMismatch,
    DigestKeyMismatch,
    Serialization,
    Storage,
    Corrupt,
}

#[derive(Debug, Error)]
#[error("context selection receipt storage failed ({kind:?})")]
pub(crate) struct ReceiptStoreError {
    kind: ReceiptStoreErrorKind,
}

impl ReceiptStoreError {
    const fn new(kind: ReceiptStoreErrorKind) -> Self {
        Self { kind }
    }

    const fn identity() -> Self {
        Self::new(ReceiptStoreErrorKind::Identity)
    }

    const fn repository_mismatch() -> Self {
        Self::new(ReceiptStoreErrorKind::RepositoryMismatch)
    }

    const fn digest_key_mismatch() -> Self {
        Self::new(ReceiptStoreErrorKind::DigestKeyMismatch)
    }

    const fn serialization() -> Self {
        Self::new(ReceiptStoreErrorKind::Serialization)
    }

    const fn storage() -> Self {
        Self::new(ReceiptStoreErrorKind::Storage)
    }

    const fn corrupt() -> Self {
        Self::new(ReceiptStoreErrorKind::Corrupt)
    }

    pub(crate) const fn kind(&self) -> ReceiptStoreErrorKind {
        self.kind
    }
}

impl From<DbErr> for ReceiptStoreError {
    fn from(_: DbErr) -> Self {
        Self::corrupt()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Utc};
    use sea_orm::{ConnectionTrait, Database, Statement};
    use uuid::Uuid;

    use super::*;
    use crate::internal::{
        ai::{
            context_budget::receipt::{
                ContextSelectionReceiptDraftV1, ReceiptDraftFieldsV1, ReceiptOmissionV1,
                ReceiptReproducibilityState, ReceiptSelectionInputV1, ReceiptSensitivity,
                ReceiptSourceKind,
            },
            keyed_digest::RepositoryKeyedDigest,
        },
        config::{ConfigKv, MEMORY_KEYED_DIGEST_CONFIG_KEY},
        db::migration::run_builtin_migrations,
    };

    const TEST_CIPHERTEXT: &str = "receipt-store-test-ciphertext";

    async fn database(
        repository_id: &str,
    ) -> (sea_orm::DatabaseConnection, Arc<RepositoryKeyedDigest>) {
        let database = Database::connect("sqlite::memory:")
            .await
            .expect("connect receipt test database");
        run_builtin_migrations(&database)
            .await
            .expect("apply receipt migration");
        ConfigKv::set_with_conn(&database, "libra.repoid", repository_id, false)
            .await
            .expect("seed canonical repository identity");
        assert!(
            ConfigKv::insert_vault_internal_if_absent_with_conn(
                &database,
                MEMORY_KEYED_DIGEST_CONFIG_KEY,
                TEST_CIPHERTEXT,
            )
            .await
            .expect("seed repository digest config")
        );
        let provider = Arc::new(RepositoryKeyedDigest::for_receipt_tests(
            repository_id,
            digest_key_id(),
            [0x41; 32],
            TEST_CIPHERTEXT,
        ));
        (database, provider)
    }

    fn digest_key_id() -> Uuid {
        Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").expect("fixed UUIDv4")
    }

    fn fixed_now() -> DateTime<Utc> {
        "2026-08-24T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("fixed receipt clock")
    }

    async fn store<'database>(
        database: &'database sea_orm::DatabaseConnection,
        provider: Arc<RepositoryKeyedDigest>,
    ) -> ReceiptStore<'database> {
        ReceiptStore::with_clock(database, provider, fixed_now)
            .await
            .expect("valid receipt store identity")
    }

    fn draft(
        repository_id: &str,
        provider: &RepositoryKeyedDigest,
    ) -> ContextSelectionReceiptDraftV1 {
        let mut source_heads = BTreeMap::new();
        source_heads.insert("memory_repo".to_string(), "a".repeat(40));
        let mut projection_watermarks = BTreeMap::new();
        projection_watermarks.insert("memory_repo".to_string(), "a".repeat(40));

        ContextSelectionReceiptDraftV1::new(ReceiptDraftFieldsV1 {
            source_kind: ReceiptSourceKind::Memory,
            repository_id: repository_id.to_string(),
            principal_digest: provider
                .principal_digest(b"agent:alice")
                .expect("principal digest"),
            query_digest: provider
                .query_digest(b"normalized query")
                .expect("query digest"),
            effective_at: "2026-08-24T00:00:00Z"
                .parse::<DateTime<Utc>>()
                .expect("effective timestamp"),
            code_commit: Some("b".repeat(40)),
            full_branch_ref: Some("refs/heads/feature/memory".to_string()),
            source_heads,
            projection_watermarks,
            policy_hash: format!("sha256:{}", "c".repeat(64)),
            selector_version: "memory-v1".to_string(),
            selected: vec![ReceiptSelectionInputV1 {
                object_id: "episode:task-42".to_string(),
                revision_oid: "d".repeat(40),
                summary_key: "episodic/tasks/task-42".to_string(),
                order: 0,
                reason_codes: vec!["bm25_match".to_string()],
                score_components: BTreeMap::new(),
                sensitivity: ReceiptSensitivity::Allowed,
            }],
            omissions: vec![ReceiptOmissionV1 {
                reason_code: "budget".to_string(),
                count: 2,
            }],
            token_budget: 1_600,
            bundle_hash: format!("sha256:{}", "e".repeat(64)),
            reproducibility_state: ReceiptReproducibilityState::Reproducible,
            frame_id: None,
        })
        .expect("valid receipt draft")
    }

    #[tokio::test]
    async fn append_and_retention_metadata_commit_atomically() {
        let (database, provider) = database("repo-42").await;
        let store = store(&database, Arc::clone(&provider)).await;
        let receipt = store
            .append(draft("repo-42", &provider))
            .await
            .expect("append receipt");
        assert_eq!(receipt.receipt_id().get_version_num(), 7);
        assert_eq!(receipt.repository_id(), "repo-42");

        match store
            .lookup(receipt.receipt_id())
            .await
            .expect("lookup stored receipt")
        {
            ReceiptLookup::Found(found) => assert_eq!(found.receipt_id(), receipt.receipt_id()),
            ReceiptLookup::Expired | ReceiptLookup::NotFound => {
                panic!("stored receipt disappeared")
            }
        }

        let retained_rows: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT retained_rows FROM context_selection_receipt_retention
                 WHERE repository_id = 'repo-42'"
                    .to_string(),
            ))
            .await
            .expect("read retention metadata")
            .expect("retention row")
            .try_get("", "retained_rows")
            .expect("retained row count");
        assert_eq!(retained_rows, 1);

        database
            .execute_unprepared(
                "UPDATE context_selection_receipt
                    SET recorded_at = '2026-07-24T11:59:59.000000000Z'
                  WHERE repository_id = 'repo-42'",
            )
            .await
            .expect("make the existing receipt old enough to prune");

        database
            .execute_unprepared(
                "CREATE TRIGGER receipt_retention_test_abort
                 BEFORE INSERT ON context_selection_receipt_retention
                 BEGIN SELECT RAISE(ABORT, 'forced retention failure'); END",
            )
            .await
            .expect("install deterministic retention failure");
        let error = store
            .append(draft("repo-42", &provider))
            .await
            .err()
            .expect("retention failure must roll back the receipt insert");
        assert_eq!(error.kind(), ReceiptStoreErrorKind::Storage);
        let failed_rows: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM context_selection_receipt
                 WHERE repository_id = 'repo-42'"
                    .to_string(),
            ))
            .await
            .expect("count failed append rows")
            .expect("count row")
            .try_get("", "count")
            .expect("count value");
        assert_eq!(
            failed_rows, 1,
            "rollback must restore the old row deleted by retention and remove the new row"
        );
        assert!(matches!(
            store
                .lookup(receipt.receipt_id())
                .await
                .expect("old receipt remains readable after rollback"),
            ReceiptLookup::Found(_)
        ));
        let retention_after_failure = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT pruned_before, retained_rows
                   FROM context_selection_receipt_retention
                  WHERE repository_id = 'repo-42'"
                    .to_string(),
            ))
            .await
            .expect("read retention metadata after rollback")
            .expect("retention metadata row");
        let pruned_before: Option<String> = retention_after_failure
            .try_get("", "pruned_before")
            .expect("pruned-before watermark");
        let retained_rows: i64 = retention_after_failure
            .try_get("", "retained_rows")
            .expect("retained row count");
        assert_eq!((pruned_before, retained_rows), (None, 1));
    }

    #[tokio::test]
    async fn receipt_retention_20000_rows_and_lookup_outcomes() {
        let (database, provider) = database("repo-capacity").await;
        database
            .execute_unprepared(
                "WITH RECURSIVE sequence(value) AS (
                     SELECT 0
                     UNION ALL
                     SELECT value + 1 FROM sequence WHERE value < 19999
                 )
                 INSERT INTO context_selection_receipt (
                     receipt_id, schema_version, source_kind, repository_id,
                     digest_key_id, principal_hmac, query_hmac, effective_at,
                     source_heads_json, projection_watermarks_json, policy_hash,
                     selector_version, token_budget, selected_json, omissions_json,
                     bundle_hash, reproducibility_state, recorded_at
                 )
                 SELECT
                     printf('0198a7e0-%04x-7%03x-8000-%012x',
                            (value >> 12) & 65535, value & 4095, value),
                     1, 'memory', 'repo-capacity',
                     '123e4567-e89b-42d3-a456-426614174000',
                     'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                     '2026-08-24T00:00:00.000000000Z',
                     '{\"memory_repo\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}',
                     '{\"memory_repo\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}',
                     'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                     'memory-v1', 1600, '[]', '[]',
                     'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                     'reproducible', '2026-08-24T00:00:00.000000000Z'
                 FROM sequence",
            )
            .await
            .expect("seed the bounded capacity fixture");

        let store = store(&database, Arc::clone(&provider)).await;
        store
            .append(draft("repo-capacity", &provider))
            .await
            .expect("append triggers indexed retention pruning");

        let retained_rows: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM context_selection_receipt
                  WHERE repository_id = 'repo-capacity'"
                    .to_string(),
            ))
            .await
            .expect("count retained receipts")
            .expect("retained count row")
            .try_get("", "count")
            .expect("retained count");
        assert_eq!(retained_rows, MAX_RECEIPTS_PER_REPOSITORY);

        let retention_rows: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT retained_rows FROM context_selection_receipt_retention
                  WHERE repository_id = 'repo-capacity'"
                    .to_string(),
            ))
            .await
            .expect("read retention metadata")
            .expect("retention metadata row")
            .try_get("", "retained_rows")
            .expect("retention count");
        assert_eq!(retention_rows, MAX_RECEIPTS_PER_REPOSITORY);

        let pruned_receipt =
            Uuid::parse_str("0198a7e0-0000-7000-8000-000000000000").expect("fixed pruned UUIDv7");
        assert!(matches!(
            store
                .lookup(pruned_receipt)
                .await
                .expect("classify a pruned receipt"),
            ReceiptLookup::Expired
        ));
        assert!(matches!(
            store
                .lookup(Uuid::now_v7())
                .await
                .expect("classify an unknown current receipt"),
            ReceiptLookup::NotFound
        ));

        let memory_rows: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT
                     (SELECT COUNT(*) FROM memory_head)
                   + (SELECT COUNT(*) FROM memory_revision_index)
                   + (SELECT COUNT(*) FROM memory_projection_state) AS count"
                    .to_string(),
            ))
            .await
            .expect("count authoritative Memory state")
            .expect("Memory state count row")
            .try_get("", "count")
            .expect("Memory state count");
        assert_eq!(memory_rows, 0, "receipt pruning cannot mutate Memory state");
    }

    #[tokio::test]
    async fn store_rejects_cross_repository_and_digest_key_before_writing() {
        let (database, provider) = database("repo-42").await;
        let store = store(&database, Arc::clone(&provider)).await;

        let provider_for_other_repository = Arc::new(RepositoryKeyedDigest::for_receipt_tests(
            "repo-other",
            digest_key_id(),
            [0x41; 32],
            TEST_CIPHERTEXT,
        ));
        let store_binding_error = ReceiptStore::new(&database, provider_for_other_repository)
            .await
            .err()
            .expect("a provider resolved for another repository cannot bind this database");
        assert_eq!(
            store_binding_error.kind(),
            ReceiptStoreErrorKind::RepositoryMismatch
        );

        let repository_error = store
            .append(draft("repo-other", &provider))
            .await
            .err()
            .expect("store cannot accept a draft from another repository");
        assert_eq!(
            repository_error.kind(),
            ReceiptStoreErrorKind::RepositoryMismatch
        );

        let other_key =
            Uuid::parse_str("223e4567-e89b-42d3-a456-426614174000").expect("second UUIDv4");
        let other_provider = Arc::new(RepositoryKeyedDigest::for_receipt_tests(
            "repo-42",
            other_key,
            [0x42; 32],
            "different-repository-digest-config",
        ));
        let other_key_draft = draft("repo-42", &other_provider);
        let key_binding_error = ReceiptStore::new(&database, Arc::clone(&other_provider))
            .await
            .err()
            .expect("an unknown digest generation cannot bind this database");
        assert_eq!(
            key_binding_error.kind(),
            ReceiptStoreErrorKind::DigestKeyMismatch
        );
        let key_error = store
            .append(other_key_draft)
            .await
            .err()
            .expect("store cannot accept a draft using another repository key");
        assert_eq!(key_error.kind(), ReceiptStoreErrorKind::DigestKeyMismatch);

        let removed_generation_draft = draft("repo-42", &provider);
        assert_eq!(
            ConfigKv::unset_all_with_conn(&database, MEMORY_KEYED_DIGEST_CONFIG_KEY)
                .await
                .expect("remove the persisted digest generation after store construction"),
            1
        );
        let removed_generation_error = store
            .append(removed_generation_draft)
            .await
            .err()
            .expect("append must revalidate the persisted digest generation");
        assert_eq!(
            removed_generation_error.kind(),
            ReceiptStoreErrorKind::DigestKeyMismatch
        );

        let rows: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM context_selection_receipt".to_string(),
            ))
            .await
            .expect("count rejected writes")
            .expect("count row")
            .try_get("", "count")
            .expect("count value");
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn retention_uses_injected_clock_at_29_30_and_31_day_boundaries() {
        let (database, provider) = database("repo-age").await;
        database
            .execute_unprepared(
                "INSERT INTO context_selection_receipt (
                     receipt_id, schema_version, source_kind, repository_id,
                     digest_key_id, principal_hmac, query_hmac, effective_at,
                     source_heads_json, projection_watermarks_json, policy_hash,
                     selector_version, token_budget, selected_json, omissions_json,
                     bundle_hash, reproducibility_state, recorded_at
                 ) VALUES
                 ('0198a7e0-0000-7000-8000-000000000001', 1, 'memory', 'repo-age',
                  '123e4567-e89b-42d3-a456-426614174000',
                  'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                  'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                  '2026-07-24T12:00:00.000000000Z', '{}', '{}',
                  'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                  'memory-v1', 1, '[]', '[]',
                  'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                  'reproducible', '2026-07-24T12:00:00.000000000Z'),
                 ('0198a7e0-0000-7000-8000-000000000002', 1, 'memory', 'repo-age',
                  '123e4567-e89b-42d3-a456-426614174000',
                  'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                  'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                  '2026-07-25T12:00:00.000000000Z', '{}', '{}',
                  'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                  'memory-v1', 1, '[]', '[]',
                  'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                  'reproducible', '2026-07-25T12:00:00.000000000Z'),
                 ('0198a7e0-0000-7000-8000-000000000003', 1, 'memory', 'repo-age',
                  '123e4567-e89b-42d3-a456-426614174000',
                  'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                  'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                  '2026-07-26T12:00:00.000000000Z', '{}', '{}',
                  'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                  'memory-v1', 1, '[]', '[]',
                  'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                  'reproducible', '2026-07-26T12:00:00.000000000Z')",
            )
            .await
            .expect("seed age boundary receipts");

        store(&database, Arc::clone(&provider))
            .await
            .append(draft("repo-age", &provider))
            .await
            .expect("apply deterministic age retention");
        let retained: Vec<String> = database
            .query_all_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT receipt_id FROM context_selection_receipt
                  WHERE repository_id = 'repo-age'
                  ORDER BY recorded_at"
                    .to_string(),
            ))
            .await
            .expect("read age boundary survivors")
            .into_iter()
            .map(|row| row.try_get("", "receipt_id").expect("receipt id"))
            .collect();
        assert_eq!(
            retained.len(),
            3,
            "31-day row pruned; 30-day and 29-day rows retained"
        );
        assert!(!retained.iter().any(|id| id.ends_with("0001")));
        assert!(retained.iter().any(|id| id.ends_with("0002")));
        assert!(retained.iter().any(|id| id.ends_with("0003")));
    }
}
