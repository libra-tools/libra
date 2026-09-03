//! Read/maintenance diagnostics for repository Memory.
//!
//! The CLI adapter crosses this interface instead of reading refs, projection
//! tables or compile-job rows directly.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::{ConnectionTrait, Statement};
use serde::Serialize;

use super::{
    error::{MemoryWriterError, MemoryWriterErrorKind},
    policy::{AuthenticatedMemoryContext, REPO_EPISODE_POLICY_VERSION},
    projection::{MemoryProjection, MemoryProjectionStatus},
    reader::EpisodeReader,
    store::read_memory_ref_head,
};
use crate::internal::ai::{history::HistoryManager, keyed_digest::RepositoryKeyedDigest};

const MAX_STATUS_JOB_ROWS: usize = 4_096;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct MemoryJobStatus {
    pub(crate) total: u64,
    pub(crate) scan_limit: u64,
    pub(crate) truncated: bool,
    pub(crate) idle: u64,
    pub(crate) dirty: u64,
    pub(crate) inflight: u64,
    pub(crate) failed: u64,
    pub(crate) active_leases: u64,
    pub(crate) expired_leases: u64,
    pub(crate) pending_generations: u64,
    pub(crate) retry_count: u64,
    pub(crate) error_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct MemoryProjectionDiagnostic {
    pub(crate) state: &'static str,
    pub(crate) head: Option<String>,
    pub(crate) projected: Option<String>,
    pub(crate) last_event_seq: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct MemoryStatusReport {
    pub(crate) memory_ref: Option<String>,
    pub(crate) projection: MemoryProjectionDiagnostic,
    pub(crate) jobs: MemoryJobStatus,
    pub(crate) fts5_enabled: bool,
    pub(crate) view_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct MemoryRebuildReport {
    pub(crate) dry_run: bool,
    pub(crate) changed: bool,
    pub(crate) head: Option<String>,
    pub(crate) event_count: usize,
    pub(crate) note_count: usize,
    pub(crate) revision_count: usize,
    pub(crate) last_event_seq: u64,
}

pub(crate) struct MemoryDiagnostics<'a> {
    history: &'a HistoryManager,
    digest: Option<&'a RepositoryKeyedDigest>,
}

impl<'a> MemoryDiagnostics<'a> {
    pub(crate) const fn new(
        history: &'a HistoryManager,
        digest: Option<&'a RepositoryKeyedDigest>,
    ) -> Self {
        Self { history, digest }
    }

    pub(crate) async fn status(
        &self,
        context: &AuthenticatedMemoryContext,
    ) -> Result<MemoryStatusReport, MemoryWriterError> {
        let database = self.history.database_connection();
        let (head, projection) = MemoryProjection::new(
            Arc::new(database.clone()),
            self.history.repository_path().to_path_buf(),
            REPO_EPISODE_POLICY_VERSION,
        )
        .status_consistent()
        .await?;
        let view_hash = match self.digest {
            Some(digest) => match EpisodeReader::new(self.history, digest) {
                Ok(reader) => reader
                    .freeze_view(context)
                    .await
                    .ok()
                    .map(|view| view.view_hash().to_string()),
                Err(_) => None,
            },
            None => None,
        };
        Ok(MemoryStatusReport {
            memory_ref: head.map(|value| value.to_string()),
            projection: projection_diagnostic(projection),
            jobs: read_job_status(&database).await?,
            fts5_enabled: read_fts5_capability(&database).await?,
            view_hash,
        })
    }

    pub(crate) async fn rebuild(
        &self,
        dry_run: bool,
    ) -> Result<MemoryRebuildReport, MemoryWriterError> {
        let database = self.history.database_connection();
        let Some(head) = read_memory_ref_head(&database).await? else {
            return Ok(MemoryRebuildReport {
                dry_run,
                changed: false,
                head: None,
                event_count: 0,
                note_count: 0,
                revision_count: 0,
                last_event_seq: 0,
            });
        };
        let projection = MemoryProjection::new(
            Arc::new(database),
            self.history.repository_path().to_path_buf(),
            REPO_EPISODE_POLICY_VERSION,
        );
        let plan = projection.plan_rebuild(head).await?;
        if !dry_run {
            let rebuilt_at_ms = Utc::now().timestamp_millis();
            if rebuilt_at_ms < 0 {
                return Err(storage_error("system clock is before the Unix epoch"));
            }
            projection.rebuild(head, rebuilt_at_ms).await?;
        }
        Ok(MemoryRebuildReport {
            dry_run,
            changed: !dry_run,
            head: Some(plan.head.to_string()),
            event_count: plan.event_count,
            note_count: plan.note_count,
            revision_count: plan.revision_count,
            last_event_seq: plan.last_event_seq,
        })
    }
}

fn projection_diagnostic(status: MemoryProjectionStatus) -> MemoryProjectionDiagnostic {
    match status {
        MemoryProjectionStatus::Empty => MemoryProjectionDiagnostic {
            state: "empty",
            head: None,
            projected: None,
            last_event_seq: Some(0),
        },
        MemoryProjectionStatus::Current {
            head,
            last_event_seq,
        } => MemoryProjectionDiagnostic {
            state: "current",
            head: Some(head.to_string()),
            projected: Some(head.to_string()),
            last_event_seq: Some(last_event_seq),
        },
        MemoryProjectionStatus::Stale {
            head,
            projected,
            last_event_seq,
        } => MemoryProjectionDiagnostic {
            state: "stale",
            head: Some(head.to_string()),
            projected: projected.map(|value| value.to_string()),
            last_event_seq: Some(last_event_seq),
        },
        MemoryProjectionStatus::Corrupt {
            head,
            projected,
            last_event_seq,
        } => MemoryProjectionDiagnostic {
            state: "corrupt",
            head: head.map(|value| value.to_string()),
            projected,
            last_event_seq: last_event_seq.and_then(|value| u64::try_from(value).ok()),
        },
    }
}

async fn read_job_status<C: ConnectionTrait>(
    database: &C,
) -> Result<MemoryJobStatus, MemoryWriterError> {
    read_job_status_bounded(database, MAX_STATUS_JOB_ROWS).await
}

async fn read_job_status_bounded<C: ConnectionTrait>(
    database: &C,
    limit: usize,
) -> Result<MemoryJobStatus, MemoryWriterError> {
    let now_ms = Utc::now().timestamp_millis();
    if now_ms < 0 {
        return Err(storage_error("system clock is before the Unix epoch"));
    }
    let fetch_limit = limit
        .checked_add(1)
        .ok_or_else(|| storage_error("Memory job status scan limit overflowed"))?;
    let fetch_limit = i64::try_from(fetch_limit)
        .map_err(|_| storage_error("Memory job status scan limit is too large"))?;
    let mut rows = database
        .query_all_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT state, lease_expires_at, observed_generation,
                    processed_generation, retry_count, last_error_code
             FROM memory_compile_job
             WHERE scope_key = 'repo'
             ORDER BY scope_key, root_kind, root_id
             LIMIT ?",
            [fetch_limit.into()],
        ))
        .await
        .map_err(|_| storage_error("query bounded Memory compile-job status"))?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    let scan_limit = u64::try_from(limit)
        .map_err(|_| storage_error("Memory job status scan limit is too large"))?;
    let mut status = MemoryJobStatus {
        total: u64::try_from(rows.len())
            .map_err(|_| storage_error("Memory job status row count overflowed"))?,
        scan_limit,
        truncated,
        ..MemoryJobStatus::default()
    };
    for row in rows {
        let state = row
            .try_get::<String>("", "state")
            .map_err(|_| storage_error("decode Memory compile-job state"))?;
        match state.as_str() {
            "idle" => increment(&mut status.idle, 1)?,
            "dirty" => increment(&mut status.dirty, 1)?,
            "inflight" => {
                increment(&mut status.inflight, 1)?;
                let lease_expires_at = row
                    .try_get::<Option<i64>>("", "lease_expires_at")
                    .map_err(|_| storage_error("decode Memory compile-job lease"))?
                    .ok_or_else(|| {
                        MemoryWriterError::new(
                            MemoryWriterErrorKind::CorruptProjection,
                            "inflight Memory compile job has no lease expiry",
                        )
                    })?;
                if lease_expires_at > now_ms {
                    increment(&mut status.active_leases, 1)?;
                } else {
                    increment(&mut status.expired_leases, 1)?;
                }
            }
            "failed" => increment(&mut status.failed, 1)?,
            _ => {
                return Err(MemoryWriterError::new(
                    MemoryWriterErrorKind::CorruptProjection,
                    "Memory compile job has an invalid state",
                ));
            }
        }
        let observed = non_negative(&row, "observed_generation")?;
        let processed = non_negative(&row, "processed_generation")?;
        let pending = observed.checked_sub(processed).ok_or_else(|| {
            MemoryWriterError::new(
                MemoryWriterErrorKind::CorruptProjection,
                "Memory compile job generation moved backwards",
            )
        })?;
        increment(&mut status.pending_generations, pending)?;
        increment(&mut status.retry_count, non_negative(&row, "retry_count")?)?;
        if row
            .try_get::<Option<String>>("", "last_error_code")
            .map_err(|_| storage_error("decode Memory compile-job error status"))?
            .is_some()
        {
            increment(&mut status.error_count, 1)?;
        }
    }
    Ok(status)
}

fn increment(value: &mut u64, amount: u64) -> Result<(), MemoryWriterError> {
    *value = value
        .checked_add(amount)
        .ok_or_else(|| storage_error("Memory compile-job aggregate overflowed"))?;
    Ok(())
}

async fn read_fts5_capability<C: ConnectionTrait>(database: &C) -> Result<bool, MemoryWriterError> {
    let row = database
        .query_one_raw(Statement::from_string(
            database.get_database_backend(),
            "SELECT sqlite_compileoption_used('ENABLE_FTS5') AS enabled".to_string(),
        ))
        .await
        .map_err(|_| storage_error("query SQLite FTS5 capability"))?
        .ok_or_else(|| storage_error("SQLite FTS5 capability returned no row"))?;
    match row.try_get::<i64>("", "enabled") {
        Ok(0) => Ok(false),
        Ok(1) => Ok(true),
        _ => Err(MemoryWriterError::new(
            MemoryWriterErrorKind::CorruptProjection,
            "SQLite returned an invalid FTS5 capability value",
        )),
    }
}

fn non_negative(row: &sea_orm::QueryResult, column: &str) -> Result<u64, MemoryWriterError> {
    let value = row
        .try_get::<i64>("", column)
        .map_err(|_| storage_error("decode Memory compile-job status"))?;
    u64::try_from(value).map_err(|_| {
        MemoryWriterError::new(
            MemoryWriterErrorKind::CorruptProjection,
            "Memory compile-job status contains a negative aggregate",
        )
    })
}

fn storage_error(summary: &'static str) -> MemoryWriterError {
    MemoryWriterError::new(MemoryWriterErrorKind::StorageFailure, summary)
}

#[cfg(test)]
mod tests {
    use sea_orm::ConnectionTrait;

    use super::*;
    use crate::internal::ai::memory::memory_test_fixture;

    #[tokio::test]
    async fn job_status_distinguishes_active_and_expired_leases() {
        let fixture = memory_test_fixture().await;
        let now_ms = Utc::now().timestamp_millis();
        let created_at = now_ms.saturating_sub(60_000);
        for (root_id, owner, lease_expires_at) in [
            ("task-active-lease", "runner-active", now_ms + 60_000),
            ("task-expired-lease", "runner-expired", now_ms - 1),
        ] {
            fixture
                .database
                .execute_raw(Statement::from_sql_and_values(
                    fixture.database.get_database_backend(),
                    "INSERT INTO memory_compile_job (
                        scope_key, root_kind, root_id, terminal_source_oid,
                        input_fingerprint_version, input_fingerprint_key_id,
                        input_fingerprint_digest, observed_generation,
                        processed_generation, state, lease_owner, lease_fence,
                        lease_expires_at, retry_count, next_retry_at,
                        last_error_code, last_error_summary, created_at, updated_at
                     ) VALUES (
                        'repo', 'task', ?, ?, 1, ?, ?, 1, 0, 'inflight', ?, 1,
                        ?, 0, NULL, NULL, NULL, ?, ?
                     )",
                    [
                        root_id.into(),
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                        "550e8400-e29b-41d4-a716-446655440000".into(),
                        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                        owner.into(),
                        lease_expires_at.into(),
                        created_at.into(),
                        created_at.into(),
                    ],
                ))
                .await
                .expect("seed compile job lease");
        }

        let status = read_job_status(fixture.database.as_ref())
            .await
            .expect("read compile job status");
        assert_eq!(status.total, 2);
        assert_eq!(status.scan_limit, MAX_STATUS_JOB_ROWS as u64);
        assert!(!status.truncated);
        assert_eq!(status.inflight, 2);
        assert_eq!(status.active_leases, 1);
        assert_eq!(status.expired_leases, 1);
        assert_eq!(status.pending_generations, 2);

        let bounded = read_job_status_bounded(fixture.database.as_ref(), 1)
            .await
            .expect("read bounded compile job status");
        assert_eq!(bounded.total, 1);
        assert_eq!(bounded.scan_limit, 1);
        assert!(bounded.truncated);
    }
}
