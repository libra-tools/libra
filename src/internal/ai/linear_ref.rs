//! Atomic compare-and-swap updates for Libra-owned linear refs.
//!
//! Object construction deliberately happens before this module is called.
//! This module owns the SQLite transaction that advances a named ref and
//! applies its companion projection/catalog writes. A stale expected head is
//! returned to the caller as data so the caller can rebuild its proposal; it
//! is never retried here.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use git_internal::hash::ObjectHash;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DatabaseTransaction, DbErr, EntityTrait,
    QueryFilter, Set, SqlErr, sea_query::Expr,
};
use tokio::time::sleep;

use crate::internal::{
    ai::history::AI_REF,
    branch::{INTENT_BRANCH, LEGACY_TRACES_BRANCH, TRACES_BRANCH},
    model::reference::{self, ConfigKind},
};

const SQLITE_BUSY_MAX_RETRIES: usize = 15;
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 100;

/// Transfer policy attached to a Libra-owned ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnedRefTransportPolicy {
    /// The ref participates in ordinary repository transport.
    Ordinary,
    /// The ref is transported only through its dedicated command path.
    DedicatedOnly,
    /// The ref must stay in the local repository.
    LocalOnly,
}

/// Behaviour attached to one canonical Libra-owned ref.
///
/// This record is deliberately value-only: command adapters can enforce the
/// same decision without querying SQLite or duplicating name checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnedRefPolicy {
    pub(crate) visible_to_branch: bool,
    pub(crate) mutable_by_user: bool,
    pub(crate) operation_snapshot: bool,
    pub(crate) gc_root: bool,
    pub(crate) transport: OwnedRefTransportPolicy,
}

/// Closed set of refs whose mutation policy is owned by Libra.
///
/// Callers select a variant instead of supplying a name, so a user-controlled
/// string cannot impersonate the local-only Memory ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnedRefSpec {
    AiHistory,
    Traces,
    LegacyTraces,
    MemoryRepo,
}

impl OwnedRefSpec {
    pub(crate) const fn kind(self) -> ConfigKind {
        ConfigKind::Branch
    }

    pub(crate) const fn storage_name(self) -> &'static str {
        match self {
            Self::AiHistory => AI_REF,
            Self::Traces => TRACES_BRANCH,
            Self::LegacyTraces => LEGACY_TRACES_BRANCH,
            Self::MemoryRepo => "libra/memory/repo",
        }
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "M2-03 freezes the full ref consumed by M2-04 and M2-05"
        )
    )]
    pub(crate) const fn full_ref(self) -> &'static str {
        match self {
            Self::AiHistory => "refs/heads/libra/intent",
            Self::Traces => "refs/libra/traces",
            Self::LegacyTraces => "refs/libra/agent-traces",
            Self::MemoryRepo => "refs/heads/libra/memory/repo",
        }
    }

    pub(crate) const fn transport_policy(self) -> OwnedRefTransportPolicy {
        match self {
            Self::AiHistory => OwnedRefTransportPolicy::Ordinary,
            Self::Traces | Self::LegacyTraces => OwnedRefTransportPolicy::DedicatedOnly,
            Self::MemoryRepo => OwnedRefTransportPolicy::LocalOnly,
        }
    }

    pub(crate) const fn policy(self) -> OwnedRefPolicy {
        match self {
            Self::MemoryRepo => OwnedRefPolicy {
                visible_to_branch: false,
                mutable_by_user: false,
                operation_snapshot: false,
                gc_root: true,
                transport: OwnedRefTransportPolicy::LocalOnly,
            },
            Self::AiHistory => OwnedRefPolicy {
                visible_to_branch: true,
                mutable_by_user: false,
                operation_snapshot: true,
                gc_root: true,
                transport: OwnedRefTransportPolicy::Ordinary,
            },
            Self::Traces | Self::LegacyTraces => OwnedRefPolicy {
                visible_to_branch: true,
                mutable_by_user: false,
                operation_snapshot: true,
                gc_root: true,
                transport: OwnedRefTransportPolicy::DedicatedOnly,
            },
        }
    }

    /// Classify an exact name as stored in the local `reference` table.
    pub(crate) fn for_storage_name(name: &str) -> Option<Self> {
        match name {
            AI_REF | INTENT_BRANCH => Some(Self::AiHistory),
            TRACES_BRANCH => Some(Self::Traces),
            LEGACY_TRACES_BRANCH => Some(Self::LegacyTraces),
            "libra/memory/repo" => Some(Self::MemoryRepo),
            _ => None,
        }
    }

    /// Classify an exact fully-qualified ref name.
    pub(crate) fn for_full_ref(name: &str) -> Option<Self> {
        match name {
            "refs/heads/libra/intent" | "refs/heads/intent" => Some(Self::AiHistory),
            "refs/libra/traces" => Some(Self::Traces),
            "refs/libra/agent-traces" => Some(Self::LegacyTraces),
            "refs/heads/libra/memory/repo" => Some(Self::MemoryRepo),
            _ => None,
        }
    }

    /// Classify a transport-visible ref name without broad prefix matching.
    ///
    /// Fetch stores remote-tracking rows in fully-qualified form, so transport
    /// boundaries must also recognize the exact branch suffix after the remote
    /// component. Lookalike branches remain ordinary.
    pub(crate) fn for_transport_ref(name: &str) -> Option<Self> {
        Self::for_storage_name(name)
            .or_else(|| Self::for_full_ref(name))
            .or_else(|| {
                name.strip_prefix("refs/remotes/")
                    .and_then(|rest| rest.split_once('/'))
                    .and_then(|(_, branch)| Self::for_storage_name(branch))
            })
    }

    /// Resolve the exact storage names accepted by `HistoryManager`.
    ///
    /// Full Memory ref classification belongs to M2-05. This conversion is
    /// intentionally narrower: it only admits the two histories that already
    /// use `HistoryManager` today.
    pub(crate) fn for_history_storage_name(name: &str) -> Option<Self> {
        Self::for_storage_name(name).filter(|spec| !matches!(spec, Self::MemoryRepo))
    }
}

/// Result of one conditional ref transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinearRefTransactionOutcome {
    Updated,
    HeadChanged,
}

#[derive(Debug, thiserror::Error)]
#[error("linear ref transaction exceeded its execution deadline")]
pub(crate) struct LinearRefDeadlineExceeded;

/// Borrowed proof that the enclosing owned-ref transaction acquired SQLite's
/// write lock before any companion reads or writes.
///
/// The constructor stays inside this module so companion implementations
/// cannot certify an arbitrary deferred transaction as write-locked.
pub(crate) struct LinearRefWriteTransaction<'a> {
    transaction: &'a DatabaseTransaction,
}

impl<'a> LinearRefWriteTransaction<'a> {
    pub(crate) const fn as_database_transaction(&self) -> &'a DatabaseTransaction {
        self.transaction
    }
}

/// Companion mutation applied after the ref CAS succeeds and before commit.
#[async_trait::async_trait]
pub(crate) trait LinearRefCompanion: Send + Sync {
    async fn apply(&self, txn: &LinearRefWriteTransaction<'_>) -> Result<()>;
}

/// Advance an owned ref and apply `companion` in the same SQLite transaction.
///
/// Transient SQLite lock failures are retried with a bounded delay. A stale
/// expected head is not retried: callers must rebuild any objects/proposal on
/// the winning head and explicitly invoke the primitive again.
pub(crate) async fn linear_ref_transaction(
    db_conn: &DatabaseConnection,
    spec: OwnedRefSpec,
    expected_head: Option<ObjectHash>,
    new_head: ObjectHash,
    deadline: Option<Instant>,
    companion: Option<&dyn LinearRefCompanion>,
) -> Result<LinearRefTransactionOutcome> {
    let expected_commit = expected_head.map(|hash| hash.to_string());
    let new_commit = new_head.to_string();

    for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
        ensure_before_deadline(deadline)?;
        let txn = match crate::internal::db::begin_write_transaction(db_conn).await {
            Ok(txn) => txn,
            Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                retry_after_busy(attempt).await;
                continue;
            }
            Err(err) => return Err(err).context("failed to begin linear ref transaction"),
        };

        let existing = match reference::Entity::find()
            .filter(reference::Column::Name.eq(spec.storage_name()))
            .filter(reference::Column::Kind.eq(spec.kind()))
            .one(&txn)
            .await
        {
            Ok(existing) => existing,
            Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                let _ = txn.rollback().await;
                retry_after_busy(attempt).await;
                continue;
            }
            Err(err) => return Err(err).context("failed to query owned reference"),
        };

        let write_result = match existing {
            Some(model) if model.commit != expected_commit => {
                let _ = txn.rollback().await;
                return Ok(LinearRefTransactionOutcome::HeadChanged);
            }
            Some(model) => {
                let mut update = reference::Entity::update_many()
                    .filter(reference::Column::Id.eq(model.id))
                    .filter(reference::Column::Name.eq(spec.storage_name()))
                    .filter(reference::Column::Kind.eq(spec.kind()));
                update = match expected_commit.as_ref() {
                    Some(commit) => update.filter(reference::Column::Commit.eq(commit.clone())),
                    None => update.filter(reference::Column::Commit.is_null()),
                };
                update
                    .col_expr(
                        reference::Column::Commit,
                        Expr::value(Some(new_commit.clone())),
                    )
                    .exec(&txn)
                    .await
                    .map(Some)
            }
            None if expected_commit.is_some() => {
                let _ = txn.rollback().await;
                return Ok(LinearRefTransactionOutcome::HeadChanged);
            }
            None => {
                let new_ref = reference::ActiveModel {
                    name: Set(Some(spec.storage_name().to_string())),
                    kind: Set(spec.kind()),
                    commit: Set(Some(new_commit.clone())),
                    remote: Set(None),
                    ..Default::default()
                };
                match new_ref.insert(&txn).await {
                    Ok(_) => Ok(None),
                    Err(err) if is_sqlite_unique_violation(&err) => {
                        let _ = txn.rollback().await;
                        return Ok(LinearRefTransactionOutcome::HeadChanged);
                    }
                    Err(err) => Err(err),
                }
            }
        };

        let rows_affected = match write_result {
            Ok(rows_affected) => rows_affected,
            Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                let _ = txn.rollback().await;
                retry_after_busy(attempt).await;
                continue;
            }
            Err(err) => return Err(err).context("failed to compare-and-swap owned reference"),
        };

        if rows_affected.is_some_and(|result| result.rows_affected != 1) {
            let _ = txn.rollback().await;
            return Ok(LinearRefTransactionOutcome::HeadChanged);
        }

        if let Some(companion) = companion
            && let Err(err) = companion
                .apply(&LinearRefWriteTransaction { transaction: &txn })
                .await
        {
            let _ = txn.rollback().await;
            return Err(err.context("companion mutation failed; owned ref update rolled back"));
        }

        if let Err(err) = ensure_before_deadline(deadline) {
            let _ = txn.rollback().await;
            return Err(err);
        }

        // COMMIT is deliberately not cancellable. Once it starts, wait for a
        // definitive SQLite result so callers never observe an ambiguous
        // "reported timeout but possibly committed" outcome.
        match txn.commit().await {
            Ok(()) => return Ok(LinearRefTransactionOutcome::Updated),
            Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                retry_after_busy(attempt).await;
            }
            Err(err) => return Err(err).context("failed to commit linear ref transaction"),
        }
    }

    Err(anyhow::anyhow!(
        "linear ref transaction exhausted its bounded SQLite retry budget"
    ))
}

fn ensure_before_deadline(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(LinearRefDeadlineExceeded.into());
    }
    Ok(())
}

async fn retry_after_busy(attempt: usize) {
    sleep(Duration::from_millis(
        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
    ))
    .await;
}

fn is_sqlite_busy(err: &DbErr) -> bool {
    let message = err.to_string();
    message.contains("database is locked") || message.contains("database schema is locked")
}

fn is_sqlite_unique_violation(err: &DbErr) -> bool {
    matches!(err.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use anyhow::bail;
    use git_internal::hash::ObjectHash;
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

    use super::*;

    struct InsertCompanion;

    #[async_trait::async_trait]
    impl LinearRefCompanion for InsertCompanion {
        async fn apply(&self, txn: &LinearRefWriteTransaction<'_>) -> Result<()> {
            let txn = txn.as_database_transaction();
            txn.execute_raw(Statement::from_string(
                txn.get_database_backend(),
                "INSERT INTO config_kv(key, value, encrypted) VALUES ('projection', '1', 0)"
                    .to_string(),
            ))
            .await?;
            Ok(())
        }
    }

    struct FailingCompanion;

    #[async_trait::async_trait]
    impl LinearRefCompanion for FailingCompanion {
        async fn apply(&self, txn: &LinearRefWriteTransaction<'_>) -> Result<()> {
            let txn = txn.as_database_transaction();
            txn.execute_raw(Statement::from_string(
                txn.get_database_backend(),
                "INSERT INTO config_kv(key, value, encrypted) VALUES ('rolled-back', '1', 0)"
                    .to_string(),
            ))
            .await?;
            bail!("simulated companion failure")
        }
    }

    struct DeadlineExpiringCompanion;

    #[async_trait::async_trait]
    impl LinearRefCompanion for DeadlineExpiringCompanion {
        async fn apply(&self, txn: &LinearRefWriteTransaction<'_>) -> Result<()> {
            let txn = txn.as_database_transaction();
            txn.execute_raw(Statement::from_string(
                txn.get_database_backend(),
                "INSERT INTO config_kv(key, value, encrypted) VALUES ('deadline', '1', 0)"
                    .to_string(),
            ))
            .await?;
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(())
        }
    }

    async fn test_database() -> DatabaseConnection {
        let database = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory database");
        database
            .execute_raw(Statement::from_string(
                database.get_database_backend(),
                "CREATE TABLE reference(\
                    id INTEGER PRIMARY KEY AUTOINCREMENT,\
                    name TEXT, kind TEXT NOT NULL, \"commit\" TEXT, remote TEXT, worktree_id TEXT);\
                 CREATE UNIQUE INDEX idx_name_kind ON reference(name, kind);\
                 CREATE TABLE config_kv(\
                    id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL,\
                    value TEXT NOT NULL, encrypted INTEGER NOT NULL DEFAULT 0);"
                    .to_string(),
            ))
            .await
            .expect("create ref transaction fixture");
        database
    }

    fn oid(value: &str) -> ObjectHash {
        ObjectHash::from_str(value).expect("valid test oid")
    }

    #[test]
    fn linear_ref_transaction_memory_spec_is_closed_and_local_only() {
        let expected = [
            (
                OwnedRefSpec::AiHistory,
                "libra/intent",
                "refs/heads/libra/intent",
                OwnedRefTransportPolicy::Ordinary,
            ),
            (
                OwnedRefSpec::Traces,
                "traces",
                "refs/libra/traces",
                OwnedRefTransportPolicy::DedicatedOnly,
            ),
            (
                OwnedRefSpec::LegacyTraces,
                "agent-traces",
                "refs/libra/agent-traces",
                OwnedRefTransportPolicy::DedicatedOnly,
            ),
            (
                OwnedRefSpec::MemoryRepo,
                "libra/memory/repo",
                "refs/heads/libra/memory/repo",
                OwnedRefTransportPolicy::LocalOnly,
            ),
        ];
        for (spec, storage_name, full_ref, transport_policy) in expected {
            assert_eq!(spec.kind(), ConfigKind::Branch);
            assert_eq!(spec.storage_name(), storage_name);
            assert_eq!(spec.full_ref(), full_ref);
            assert_eq!(spec.transport_policy(), transport_policy);
        }
        assert_eq!(
            OwnedRefSpec::for_history_storage_name("libra/memory/repo"),
            None
        );
        assert_eq!(
            OwnedRefSpec::for_history_storage_name("libra/memory/repo-user"),
            None
        );
        for name in [
            INTENT_BRANCH,
            "refs/heads/intent",
            AI_REF,
            "refs/heads/libra/intent",
        ] {
            let spec =
                OwnedRefSpec::for_storage_name(name).or_else(|| OwnedRefSpec::for_full_ref(name));
            assert_eq!(spec, Some(OwnedRefSpec::AiHistory));
        }
    }

    #[test]
    fn owned_ref_policy_classifies_only_exact_memory_names() {
        for name in ["libra/memory/repo", "refs/heads/libra/memory/repo"] {
            let spec = OwnedRefSpec::for_storage_name(name)
                .or_else(|| OwnedRefSpec::for_full_ref(name))
                .expect("canonical Memory ref must classify");
            assert_eq!(spec, OwnedRefSpec::MemoryRepo);
            assert_eq!(
                spec.policy(),
                OwnedRefPolicy {
                    visible_to_branch: false,
                    mutable_by_user: false,
                    operation_snapshot: false,
                    gc_root: true,
                    transport: OwnedRefTransportPolicy::LocalOnly,
                }
            );
        }

        for lookalike in [
            "libra/memory/repo-user",
            "libra/memory/repo/child",
            "refs/heads/libra/memory/repo-user",
            "refs/heads/libra/memory/repo/child",
            "refs/remotes/origin/libra/memory/repo",
        ] {
            assert_eq!(OwnedRefSpec::for_storage_name(lookalike), None);
            assert_eq!(OwnedRefSpec::for_full_ref(lookalike), None);
        }
        assert_eq!(
            OwnedRefSpec::for_transport_ref("refs/remotes/origin/libra/memory/repo"),
            Some(OwnedRefSpec::MemoryRepo)
        );
        assert_eq!(
            OwnedRefSpec::for_transport_ref("refs/remotes/origin/libra/memory/repo-user"),
            None
        );
    }

    #[tokio::test]
    async fn linear_ref_transaction_commits_ref_and_companion_atomically() {
        let database = test_database().await;
        let new_head = oid("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");

        let outcome = linear_ref_transaction(
            &database,
            OwnedRefSpec::MemoryRepo,
            None,
            new_head,
            None,
            Some(&InsertCompanion),
        )
        .await
        .expect("commit ref transaction");
        assert_eq!(outcome, LinearRefTransactionOutcome::Updated);

        let ref_count: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM reference WHERE name = 'libra/memory/repo'"
                    .to_string(),
            ))
            .await
            .expect("query ref")
            .expect("ref row")
            .try_get("", "count")
            .expect("ref count");
        let companion_count: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM config_kv WHERE key = 'projection'".to_string(),
            ))
            .await
            .expect("query companion")
            .expect("companion row")
            .try_get("", "count")
            .expect("companion count");
        assert_eq!((ref_count, companion_count), (1, 1));
    }

    #[tokio::test]
    async fn linear_ref_transaction_rolls_back_ref_when_companion_fails() {
        let database = test_database().await;
        let new_head = oid("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");

        let error = linear_ref_transaction(
            &database,
            OwnedRefSpec::MemoryRepo,
            None,
            new_head,
            None,
            Some(&FailingCompanion),
        )
        .await
        .expect_err("companion failure must roll back ref");
        assert!(format!("{error:#}").contains("simulated companion failure"));

        let ref_count: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM reference".to_string(),
            ))
            .await
            .expect("query refs")
            .expect("count row")
            .try_get("", "count")
            .expect("ref count");
        let companion_count: i64 = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM config_kv WHERE key = 'rolled-back'".to_string(),
            ))
            .await
            .expect("query rolled-back companion")
            .expect("count row")
            .try_get("", "count")
            .expect("companion count");
        assert_eq!((ref_count, companion_count), (0, 0));
    }

    #[tokio::test]
    async fn linear_ref_transaction_reports_stale_head_without_companion() {
        let database = test_database().await;
        let winner = oid("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        let stale = oid("f4e6d0434b8b29ae775ad8c2e48c5391e69de29b");
        let proposed = oid("a4e6d0434b8b29ae775ad8c2e48c5391e69de29b");
        linear_ref_transaction(
            &database,
            OwnedRefSpec::MemoryRepo,
            None,
            winner,
            None,
            None,
        )
        .await
        .expect("seed winner");

        let outcome = linear_ref_transaction(
            &database,
            OwnedRefSpec::MemoryRepo,
            Some(stale),
            proposed,
            None,
            Some(&FailingCompanion),
        )
        .await
        .expect("stale CAS is a typed outcome");
        assert_eq!(outcome, LinearRefTransactionOutcome::HeadChanged);
    }

    #[tokio::test]
    async fn linear_ref_transaction_deadline_rolls_back_before_commit() {
        let database = test_database().await;
        let new_head = oid("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        let deadline = Instant::now() + Duration::from_millis(5);

        let error = linear_ref_transaction(
            &database,
            OwnedRefSpec::MemoryRepo,
            None,
            new_head,
            Some(deadline),
            Some(&DeadlineExpiringCompanion),
        )
        .await
        .expect_err("deadline reached before commit must abort the transaction");
        assert!(error.downcast_ref::<LinearRefDeadlineExceeded>().is_some());

        for table in ["reference", "config_kv"] {
            let count: i64 = database
                .query_one_raw(Statement::from_string(
                    database.get_database_backend(),
                    format!("SELECT COUNT(*) AS count FROM {table}"),
                ))
                .await
                .expect("query table after deadline rollback")
                .expect("count row")
                .try_get("", "count")
                .expect("row count");
            assert_eq!(count, 0, "{table} mutation must roll back");
        }
    }
}
