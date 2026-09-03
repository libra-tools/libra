//! Deterministic projection replay and rebuild for repository Memory history.

use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use anyhow::Result;
use git_internal::hash::ObjectHash;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement, TransactionTrait,
};
use uuid::Uuid;

use super::{
    domain::{EpisodeRoot, MemoryEventAction, MemoryNoteV1},
    error::{MemoryDamagePoint, MemoryWriterError, MemoryWriterErrorKind},
    fts_sql::{
        EpisodeSearchDocument, EpisodeSearchText, MemoryWriteTransaction, delete_scope_documents,
        rebuild_index, upsert_document, upsert_document_in_linear_transaction,
    },
    replay::{ProjectedNote, ProjectedReviewState, ReducedProjection, ReplayRecord},
    store::{
        enum_label, execute, insert_note, insert_revision, read_memory_ref_head,
        replace_episode_paths, replace_links, update_note,
    },
    tree::{MemoryHistoryDelta, load_head_manifest, load_history_delta, parse_oid},
};
use crate::internal::ai::linear_ref::LinearRefWriteTransaction;

const PROJECTION_SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MemoryProjectionStatus {
    Empty,
    Current {
        head: ObjectHash,
        last_event_seq: u64,
    },
    Stale {
        head: ObjectHash,
        projected: Option<ObjectHash>,
        last_event_seq: u64,
    },
    Corrupt {
        head: Option<ObjectHash>,
        projected: Option<String>,
        last_event_seq: Option<i64>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MemoryRebuildPlan {
    pub(crate) head: ObjectHash,
    pub(crate) event_count: usize,
    pub(crate) note_count: usize,
    pub(crate) revision_count: usize,
    pub(crate) last_event_seq: u64,
}

pub(crate) struct MemoryProjection {
    database: Arc<DatabaseConnection>,
    storage_path: PathBuf,
    policy_version: String,
    #[cfg(test)]
    status_snapshot_hook: Option<StatusSnapshotHook>,
}

#[cfg(test)]
#[derive(Clone)]
struct StatusSnapshotHook {
    after_head_read: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}

impl MemoryProjection {
    pub(crate) fn new(
        database: Arc<DatabaseConnection>,
        storage_path: PathBuf,
        policy_version: impl Into<String>,
    ) -> Self {
        Self {
            database,
            storage_path,
            policy_version: policy_version.into(),
            #[cfg(test)]
            status_snapshot_hook: None,
        }
    }

    #[cfg(test)]
    fn with_status_snapshot_hook(mut self, hook: StatusSnapshotHook) -> Self {
        self.status_snapshot_hook = Some(hook);
        self
    }

    pub(crate) async fn status(
        &self,
        pinned_head: Option<ObjectHash>,
    ) -> Result<MemoryProjectionStatus, MemoryWriterError> {
        let row = projection_watermark(self.database.as_ref()).await?;
        Ok(self.classify_status(pinned_head, row))
    }

    /// Read the authoritative ref and projection watermark from one SQLite
    /// snapshot so diagnostics cannot combine two different writer commits.
    pub(crate) async fn status_consistent(
        &self,
    ) -> Result<(Option<ObjectHash>, MemoryProjectionStatus), MemoryWriterError> {
        let transaction = self
            .database
            .begin()
            .await
            .map_err(|error| projection_storage("begin Memory status snapshot", error))?;
        let head = read_memory_ref_head(&transaction).await?;
        #[cfg(test)]
        if let Some(hook) = &self.status_snapshot_hook {
            hook.after_head_read.notify_one();
            hook.resume.notified().await;
        }
        let row = projection_watermark(&transaction).await?;
        let status = self.classify_status(head, row);
        transaction
            .commit()
            .await
            .map_err(|error| projection_storage("commit Memory status snapshot", error))?;
        Ok((head, status))
    }

    fn classify_status(
        &self,
        pinned_head: Option<ObjectHash>,
        row: Option<ProjectionWatermark>,
    ) -> MemoryProjectionStatus {
        match (pinned_head, row) {
            (None, None) => MemoryProjectionStatus::Empty,
            (Some(head), Some(row)) => {
                let projected = parse_oid(&row.projected_ref_oid).ok();
                let last_event_seq = u64::try_from(row.last_event_seq).ok();
                if row.schema_version != PROJECTION_SCHEMA_VERSION
                    || projected.is_none()
                    || last_event_seq.is_none()
                {
                    return MemoryProjectionStatus::Corrupt {
                        head: Some(head),
                        projected: Some(row.projected_ref_oid),
                        last_event_seq: Some(row.last_event_seq),
                    };
                }
                if projected == Some(head) {
                    let last_event_seq = last_event_seq.unwrap_or_default();
                    match load_head_manifest(&self.storage_path, head, &self.policy_version) {
                        Ok(manifest) if manifest.last_event_seq == last_event_seq => {
                            MemoryProjectionStatus::Current {
                                head,
                                last_event_seq,
                            }
                        }
                        _ => MemoryProjectionStatus::Corrupt {
                            head: Some(head),
                            projected: Some(head.to_string()),
                            last_event_seq: i64::try_from(last_event_seq).ok(),
                        },
                    }
                } else {
                    MemoryProjectionStatus::Stale {
                        head,
                        projected,
                        last_event_seq: last_event_seq.unwrap_or_default(),
                    }
                }
            }
            (Some(head), None) => MemoryProjectionStatus::Stale {
                head,
                projected: None,
                last_event_seq: 0,
            },
            (None, Some(row)) => MemoryProjectionStatus::Corrupt {
                head: None,
                projected: Some(row.projected_ref_oid),
                last_event_seq: Some(row.last_event_seq),
            },
        }
    }

    pub(crate) async fn advance(
        &self,
        pinned_head: ObjectHash,
        rebuilt_at_ms: i64,
    ) -> Result<(), MemoryWriterError> {
        ensure_pinned_ref(self.database.as_ref(), pinned_head).await?;
        let watermark = projection_watermark(self.database.as_ref()).await?;
        let (after, last_event_seq) = match watermark {
            Some(row) => {
                if row.schema_version != PROJECTION_SCHEMA_VERSION || row.last_event_seq < 0 {
                    return Err(corrupt_projection("Memory projection watermark is invalid"));
                }
                (
                    Some(parse_oid(&row.projected_ref_oid)?),
                    u64::try_from(row.last_event_seq)
                        .map_err(|_| corrupt_projection("Memory projection sequence is invalid"))?,
                )
            }
            None => (None, 0),
        };
        let history =
            load_history_delta(&self.storage_path, pinned_head, after, &self.policy_version)?;
        if history.records.is_empty() {
            if history.manifest.last_event_seq != last_event_seq {
                return Err(corrupt_projection(
                    "Memory projection sequence does not match the pinned manifest",
                ));
            }
            return Ok(());
        }
        let note_ids = history
            .records
            .iter()
            .filter_map(|record| record.event.note_id)
            .collect::<BTreeSet<_>>();
        let mut reduced = load_projection_seed(self.database.as_ref(), &note_ids).await?;
        reduced.last_event_seq = last_event_seq;
        apply_history(&mut reduced, history)?;

        let transaction = MemoryWriteTransaction::begin(self.database.as_ref())
            .await
            .map_err(fts_error)?;
        ensure_transaction_snapshot(
            transaction.as_database_transaction(),
            pinned_head,
            after,
            last_event_seq,
        )
        .await?;
        materialize(
            ProjectionTransaction::Standalone(&transaction),
            &reduced,
            pinned_head,
            &self.policy_version,
            rebuilt_at_ms,
        )
        .await?;
        transaction.commit().await.map_err(fts_error)
    }

    pub(crate) async fn rebuild(
        &self,
        pinned_head: ObjectHash,
        rebuilt_at_ms: i64,
    ) -> Result<(), MemoryWriterError> {
        ensure_pinned_ref(self.database.as_ref(), pinned_head).await?;
        let (reduced, _) = self.reduce_full_history(pinned_head)?;

        let transaction = MemoryWriteTransaction::begin(self.database.as_ref())
            .await
            .map_err(fts_error)?;
        ensure_pinned_ref_in_transaction(transaction.as_database_transaction(), pinned_head)
            .await?;
        clear_rebuildable_projection(&transaction).await?;
        materialize(
            ProjectionTransaction::Standalone(&transaction),
            &reduced,
            pinned_head,
            &self.policy_version,
            rebuilt_at_ms,
        )
        .await?;
        rebuild_index(&transaction).await.map_err(fts_error)?;
        transaction.commit().await.map_err(fts_error)
    }

    /// Validate and reduce the complete authoritative history without writing
    /// projection tables. The command adapter uses this for `--dry-run`.
    pub(crate) async fn plan_rebuild(
        &self,
        pinned_head: ObjectHash,
    ) -> Result<MemoryRebuildPlan, MemoryWriterError> {
        ensure_pinned_ref(self.database.as_ref(), pinned_head).await?;
        self.reduce_full_history(pinned_head).map(|(_, plan)| plan)
    }

    fn reduce_full_history(
        &self,
        pinned_head: ObjectHash,
    ) -> Result<(ReducedProjection, MemoryRebuildPlan), MemoryWriterError> {
        let history =
            load_history_delta(&self.storage_path, pinned_head, None, &self.policy_version)
                .map_err(|error| {
                    error.with_damage_point(MemoryDamagePoint::MemoryHead { oid: pinned_head })
                })?;
        let event_count = history.records.len();
        let mut reduced = ReducedProjection::default();
        apply_history(&mut reduced, history)?;
        let plan = MemoryRebuildPlan {
            head: pinned_head,
            event_count,
            note_count: reduced.notes.len(),
            revision_count: reduced.new_revisions.len(),
            last_event_seq: reduced.last_event_seq,
        };
        Ok((reduced, plan))
    }
}

fn apply_history(
    reduced: &mut ReducedProjection,
    history: MemoryHistoryDelta,
) -> Result<(), MemoryWriterError> {
    for record in history.records {
        let damage_point = MemoryDamagePoint::EventIdentity {
            event_seq: record.event.event_seq,
            event_id: record.event.event_id,
        };
        reduced
            .apply(ReplayRecord {
                event: record.event,
                revision_oid: record.revision_oid,
                note: record.note,
            })
            .map_err(|error| error.with_damage_point(damage_point))?;
    }
    if reduced.last_event_seq != history.manifest.last_event_seq {
        return Err(MemoryWriterError::new(
            MemoryWriterErrorKind::CorruptHistory,
            "Memory replay sequence does not match the pinned manifest",
        )
        .with_damage_point(MemoryDamagePoint::EventSequence {
            event_seq: history.manifest.last_event_seq,
        }));
    }
    Ok(())
}

enum ProjectionTransaction<'a> {
    Standalone(&'a MemoryWriteTransaction),
    Linear(&'a LinearRefWriteTransaction<'a>),
}

impl ProjectionTransaction<'_> {
    fn as_database_transaction(&self) -> &DatabaseTransaction {
        match self {
            Self::Standalone(transaction) => transaction.as_database_transaction(),
            Self::Linear(transaction) => transaction.as_database_transaction(),
        }
    }

    async fn upsert_search(
        &self,
        document: &EpisodeSearchDocument,
    ) -> Result<(), MemoryWriterError> {
        match self {
            Self::Standalone(transaction) => {
                upsert_document(transaction, document)
                    .await
                    .map_err(fts_error)?;
            }
            Self::Linear(transaction) => {
                upsert_document_in_linear_transaction(transaction, document)
                    .await
                    .map_err(fts_error)?;
            }
        }
        Ok(())
    }
}

pub(super) async fn materialize_linear(
    transaction: &LinearRefWriteTransaction<'_>,
    reduced: &ReducedProjection,
    pinned_head: ObjectHash,
    policy_version: &str,
    rebuilt_at_ms: i64,
) -> Result<(), MemoryWriterError> {
    materialize(
        ProjectionTransaction::Linear(transaction),
        reduced,
        pinned_head,
        policy_version,
        rebuilt_at_ms,
    )
    .await
}

async fn materialize(
    write: ProjectionTransaction<'_>,
    reduced: &ReducedProjection,
    pinned_head: ObjectHash,
    policy_version: &str,
    rebuilt_at_ms: i64,
) -> Result<(), MemoryWriterError> {
    let txn = write.as_database_transaction();
    let mut inserted_notes = BTreeSet::new();
    for revision_oid in &reduced.new_revision_order {
        let note = reduced.new_revisions.get(revision_oid).ok_or_else(|| {
            corrupt_projection("ordered Memory revision is absent from reducer state")
        })?;
        if reduced.created_notes.contains(&note.note_id) && inserted_notes.insert(note.note_id) {
            insert_note(txn, note).await.map_err(storage_error)?;
        } else {
            update_note(txn, note).await.map_err(storage_error)?;
        }
        insert_revision(txn, note, parse_oid(revision_oid)?)
            .await
            .map_err(storage_error)?;
    }
    for revision_oid in &reduced.new_revision_order {
        let note = reduced.new_revisions.get(revision_oid).ok_or_else(|| {
            corrupt_projection("ordered Memory revision is absent from reducer state")
        })?;
        let revision_oid = parse_oid(revision_oid)?;
        replace_links(txn, note, revision_oid)
            .await
            .map_err(storage_error)?;
        replace_episode_paths(txn, note, revision_oid)
            .await
            .map_err(storage_error)?;
        if let Some(document) = episode_search_document(note, revision_oid)? {
            write.upsert_search(&document).await?;
        }
    }
    for note_id in &reduced.changed_notes {
        let projected = reduced.notes.get(note_id).ok_or_else(|| {
            corrupt_projection("changed Memory note is absent from reducer state")
        })?;
        if let Some(note) = reduced
            .new_revisions
            .get(&projected.latest_revision_oid.to_string())
        {
            upsert_head(txn, note, projected).await?;
        } else {
            update_head_state(txn, *note_id, projected).await?;
        }
        update_note_review_state(txn, *note_id, projected.review_state).await?;
    }
    refresh_path_summaries(txn, &reduced.changed_notes).await?;
    upsert_watermark(
        txn,
        pinned_head,
        reduced.last_event_seq,
        policy_version,
        rebuilt_at_ms,
    )
    .await
}

async fn update_note_review_state(
    txn: &DatabaseTransaction,
    note_id: Uuid,
    review_state: ProjectedReviewState,
) -> Result<(), MemoryWriterError> {
    let result = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE memory_note_index SET review_state = ?
             WHERE scope_key = 'repo' AND note_id = ?",
            [
                review_state_label(review_state).into(),
                note_id.to_string().into(),
            ],
        ))
        .await
        .map_err(|error| projection_storage("update Memory note review state", error))?;
    if result.rows_affected() != 1 {
        return Err(corrupt_projection(
            "Memory note disappeared while updating review state",
        ));
    }
    Ok(())
}

async fn load_projection_seed(
    database: &DatabaseConnection,
    note_ids: &BTreeSet<Uuid>,
) -> Result<ReducedProjection, MemoryWriterError> {
    let mut reduced = ReducedProjection::default();
    for note_id in note_ids {
        let row = database
            .query_one_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "SELECT latest_revision_oid, live_revision_oid, latest_action,
                        latest_review_state, last_event_seq, updated_at
                 FROM memory_head WHERE scope_key = 'repo' AND note_id = ?",
                [note_id.to_string().into()],
            ))
            .await
            .map_err(|error| projection_storage("read Memory projection seed", error))?;
        let Some(row) = row else {
            continue;
        };
        let latest: String = row
            .try_get("", "latest_revision_oid")
            .map_err(storage_error)?;
        let live: Option<String> = row
            .try_get("", "live_revision_oid")
            .map_err(storage_error)?;
        let latest_action: String = row.try_get("", "latest_action").map_err(storage_error)?;
        let review_state: String = row
            .try_get("", "latest_review_state")
            .map_err(storage_error)?;
        let last_event_seq: i64 = row.try_get("", "last_event_seq").map_err(storage_error)?;
        let updated_at: String = row.try_get("", "updated_at").map_err(storage_error)?;
        let revisions = database
            .query_all_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "SELECT revision_oid FROM memory_revision_index
                 WHERE scope_key = 'repo' AND note_id = ? ORDER BY revision_oid",
                [note_id.to_string().into()],
            ))
            .await
            .map_err(|error| projection_storage("read Memory revision seed", error))?
            .into_iter()
            .map(|row| row.try_get("", "revision_oid").map_err(storage_error))
            .collect::<Result<BTreeSet<String>, _>>()?;
        reduced.notes.insert(
            *note_id,
            ProjectedNote {
                latest_revision_oid: parse_oid(&latest)?,
                live_revision_oid: live.as_deref().map(parse_oid).transpose()?,
                latest_action: parse_action(&latest_action)?,
                review_state: parse_review_state(&review_state)?,
                last_event_seq: u64::try_from(last_event_seq)
                    .map_err(|_| corrupt_projection("Memory head sequence is invalid"))?,
                updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at)
                    .map_err(|_| corrupt_projection("Memory head timestamp is invalid"))?
                    .with_timezone(&chrono::Utc),
                revisions,
            },
        );
    }
    Ok(reduced)
}

struct ProjectionWatermark {
    projected_ref_oid: String,
    last_event_seq: i64,
    schema_version: i64,
}

async fn projection_watermark(
    database: &impl ConnectionTrait,
) -> Result<Option<ProjectionWatermark>, MemoryWriterError> {
    database
        .query_one_raw(Statement::from_string(
            database.get_database_backend(),
            "SELECT projected_ref_oid, last_event_seq, schema_version
             FROM memory_projection_state WHERE scope_key = 'repo'"
                .to_string(),
        ))
        .await
        .map_err(|error| projection_storage("read Memory projection watermark", error))?
        .map(|row| {
            Ok(ProjectionWatermark {
                projected_ref_oid: row
                    .try_get("", "projected_ref_oid")
                    .map_err(storage_error)?,
                last_event_seq: row.try_get("", "last_event_seq").map_err(storage_error)?,
                schema_version: row.try_get("", "schema_version").map_err(storage_error)?,
            })
        })
        .transpose()
}

async fn ensure_pinned_ref(
    database: &DatabaseConnection,
    pinned_head: ObjectHash,
) -> Result<(), MemoryWriterError> {
    if read_memory_ref_head(database).await? != Some(pinned_head) {
        return Err(MemoryWriterError::new(
            MemoryWriterErrorKind::ProjectionStale,
            "pinned Memory ref no longer matches the repository ref",
        ));
    }
    Ok(())
}

async fn ensure_pinned_ref_in_transaction(
    txn: &DatabaseTransaction,
    pinned_head: ObjectHash,
) -> Result<(), MemoryWriterError> {
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT `commit` FROM reference
             WHERE kind = 'Branch' AND remote IS NULL AND name = 'libra/memory/repo'",
            [],
        ))
        .await
        .map_err(|error| projection_storage("revalidate pinned Memory ref", error))?;
    let row = row.ok_or_else(|| corrupt_projection("repository Memory ref disappeared"))?;
    let value: String = row
        .try_get("", "commit")
        .map_err(|error| projection_storage("decode pinned Memory ref", error))?;
    if parse_oid(&value)? != pinned_head {
        return Err(MemoryWriterError::new(
            MemoryWriterErrorKind::ProjectionStale,
            "pinned Memory ref changed before projection commit",
        ));
    }
    Ok(())
}

async fn ensure_transaction_snapshot(
    txn: &DatabaseTransaction,
    pinned_head: ObjectHash,
    expected_projected: Option<ObjectHash>,
    expected_seq: u64,
) -> Result<(), MemoryWriterError> {
    ensure_pinned_ref_in_transaction(txn, pinned_head).await?;
    let current = projection_watermark(txn).await?;
    let expected_seq = i64::try_from(expected_seq)
        .map_err(|_| corrupt_projection("Memory projection sequence exceeds SQLite range"))?;
    match (expected_projected, current) {
        (None, None) => Ok(()),
        (Some(expected), Some(row))
            if row.projected_ref_oid == expected.to_string()
                && row.last_event_seq == expected_seq
                && row.schema_version == PROJECTION_SCHEMA_VERSION =>
        {
            Ok(())
        }
        _ => Err(MemoryWriterError::new(
            MemoryWriterErrorKind::ProjectionStale,
            "Memory projection changed before incremental replay committed",
        )),
    }
}

async fn clear_rebuildable_projection(
    transaction: &MemoryWriteTransaction,
) -> Result<(), MemoryWriterError> {
    let txn = transaction.as_database_transaction();
    let inbound = txn
        .query_one_raw(Statement::from_string(
            txn.get_database_backend(),
            "SELECT 1 AS present
             FROM memory_link_index AS link
             INNER JOIN memory_note_index AS target
                ON target.note_id = link.target_note_id
             WHERE link.source_scope_key <> 'repo' AND target.scope_key = 'repo'
             LIMIT 1"
                .to_string(),
        ))
        .await
        .map_err(|error| projection_storage("inspect cross-scope Memory links", error))?;
    if inbound.is_some() {
        return Err(MemoryWriterError::new(
            MemoryWriterErrorKind::PolicyRejected,
            "repository Memory projection has an inbound link from another scope; rebuild the dependent scopes together",
        ));
    }

    delete_scope_documents(transaction, "repo")
        .await
        .map_err(fts_error)?;
    for statement in [
        "DELETE FROM memory_link_index WHERE source_scope_key = 'repo'",
        "DELETE FROM memory_episode_path WHERE revision_oid IN (
            SELECT revision_oid FROM memory_revision_index WHERE scope_key = 'repo'
         )",
        "DELETE FROM memory_head WHERE scope_key = 'repo'",
        "DELETE FROM memory_path_summary WHERE scope_key = 'repo'",
        "DELETE FROM memory_revision_index WHERE scope_key = 'repo'",
        "DELETE FROM memory_note_index WHERE scope_key = 'repo'",
        "DELETE FROM memory_projection_state WHERE scope_key = 'repo'",
    ] {
        txn.execute_unprepared(statement).await.map_err(|error| {
            projection_storage("clear repository-scoped Memory projection", error)
        })?;
    }
    Ok(())
}

async fn upsert_head(
    txn: &DatabaseTransaction,
    note: &MemoryNoteV1,
    projected: &ProjectedNote,
) -> Result<(), MemoryWriterError> {
    execute(
        txn,
        "INSERT INTO memory_head (
            scope_key, namespace, path, note_id, latest_revision_oid,
            live_revision_oid, latest_action, latest_review_state, kind,
            lifecycle, confidence, trust, sensitivity, visibility, acl_policy_id,
            valid_from, valid_until, effective_from_commit, effective_until_commit,
            expires_at, rank_hint, last_event_seq, updated_at
         ) VALUES ('repo', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?)
         ON CONFLICT(scope_key, namespace, path, note_id) DO UPDATE SET
            latest_revision_oid = excluded.latest_revision_oid,
            live_revision_oid = excluded.live_revision_oid,
            latest_action = excluded.latest_action,
            latest_review_state = excluded.latest_review_state,
            confidence = excluded.confidence,
            trust = excluded.trust,
            sensitivity = excluded.sensitivity,
            visibility = excluded.visibility,
            acl_policy_id = excluded.acl_policy_id,
            valid_from = excluded.valid_from,
            valid_until = excluded.valid_until,
            effective_from_commit = excluded.effective_from_commit,
            effective_until_commit = excluded.effective_until_commit,
            expires_at = excluded.expires_at,
            last_event_seq = excluded.last_event_seq,
            updated_at = excluded.updated_at",
        vec![
            note.namespace.clone().into(),
            note.path.clone().into(),
            note.note_id.to_string().into(),
            projected.latest_revision_oid.to_string().into(),
            projected
                .live_revision_oid
                .map(|oid| oid.to_string())
                .into(),
            enum_label(&projected.latest_action)
                .map_err(storage_error)?
                .into(),
            review_state_label(projected.review_state).into(),
            enum_label(&note.kind).map_err(storage_error)?.into(),
            enum_label(&note.lifecycle).map_err(storage_error)?.into(),
            enum_label(&note.confidence).map_err(storage_error)?.into(),
            enum_label(&note.trust).map_err(storage_error)?.into(),
            enum_label(&note.sensitivity).map_err(storage_error)?.into(),
            enum_label(&note.visibility).map_err(storage_error)?.into(),
            note.acl_policy_id.clone().into(),
            note.valid_from.map(|value| value.to_rfc3339()).into(),
            note.valid_until.map(|value| value.to_rfc3339()).into(),
            note.effective_from_commit.clone().into(),
            note.effective_until_commit.clone().into(),
            note.expires_at.map(|value| value.to_rfc3339()).into(),
            i64::try_from(projected.last_event_seq)
                .map_err(|_| corrupt_projection("Memory head sequence exceeds SQLite range"))?
                .into(),
            projected.updated_at.to_rfc3339().into(),
        ],
    )
    .await
    .map_err(storage_error)
}

async fn update_head_state(
    txn: &DatabaseTransaction,
    note_id: Uuid,
    projected: &ProjectedNote,
) -> Result<(), MemoryWriterError> {
    let result = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE memory_head SET live_revision_oid = ?, latest_action = ?,
                    latest_review_state = ?, last_event_seq = ?, updated_at = ?
             WHERE scope_key = 'repo' AND note_id = ?",
            [
                projected
                    .live_revision_oid
                    .map(|oid| oid.to_string())
                    .into(),
                enum_label(&projected.latest_action)
                    .map_err(storage_error)?
                    .into(),
                review_state_label(projected.review_state).into(),
                i64::try_from(projected.last_event_seq)
                    .map_err(|_| corrupt_projection("Memory head sequence exceeds SQLite range"))?
                    .into(),
                projected.updated_at.to_rfc3339().into(),
                note_id.to_string().into(),
            ],
        ))
        .await
        .map_err(|error| projection_storage("update Memory head state", error))?;
    if result.rows_affected() != 1 {
        return Err(corrupt_projection("Memory head disappeared during replay"));
    }
    Ok(())
}

async fn refresh_path_summaries(
    txn: &DatabaseTransaction,
    changed_notes: &BTreeSet<Uuid>,
) -> Result<(), MemoryWriterError> {
    let mut affected = BTreeSet::new();
    for note_id in changed_notes {
        let row = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT namespace, path FROM memory_head
                 WHERE scope_key = 'repo' AND note_id = ?",
                [note_id.to_string().into()],
            ))
            .await
            .map_err(|error| projection_storage("read changed Memory path", error))?
            .ok_or_else(|| corrupt_projection("changed Memory head has no path"))?;
        let namespace: String = row.try_get("", "namespace").map_err(storage_error)?;
        let path: String = row.try_get("", "path").map_err(storage_error)?;
        let mut prefix = String::new();
        for segment in path.split('.') {
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(segment);
            affected.insert((namespace.clone(), prefix.clone()));
        }
    }

    for (namespace, path) in affected {
        let descendant_pattern = format!("{path}.%");
        let row = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT
                    SUM(CASE WHEN path = ? AND live_revision_oid IS NOT NULL THEN 1 ELSE 0 END)
                        AS confirmed_count,
                    SUM(CASE WHEN path = ? AND latest_review_state = 'quarantined'
                        THEN 1 ELSE 0 END) AS quarantined_count,
                    SUM(CASE WHEN live_revision_oid IS NOT NULL THEN 1 ELSE 0 END)
                        AS prefix_count,
                    MAX(updated_at) AS last_changed_at
                 FROM memory_head
                 WHERE scope_key = 'repo' AND namespace = ?
                   AND (path = ? OR path LIKE ? ESCAPE '\\')",
                [
                    path.clone().into(),
                    path.clone().into(),
                    namespace.clone().into(),
                    path.clone().into(),
                    descendant_pattern.clone().into(),
                ],
            ))
            .await
            .map_err(|error| projection_storage("aggregate Memory path summary", error))?
            .ok_or_else(|| corrupt_projection("Memory path aggregate disappeared"))?;
        let confirmed_count: i64 = row
            .try_get::<Option<i64>>("", "confirmed_count")
            .map_err(storage_error)?
            .unwrap_or_default();
        let quarantined_count: i64 = row
            .try_get::<Option<i64>>("", "quarantined_count")
            .map_err(storage_error)?
            .unwrap_or_default();
        let prefix_count: i64 = row
            .try_get::<Option<i64>>("", "prefix_count")
            .map_err(storage_error)?
            .unwrap_or_default();
        let last_changed_at: String = row
            .try_get::<Option<String>>("", "last_changed_at")
            .map_err(storage_error)?
            .ok_or_else(|| corrupt_projection("Memory path aggregate has no timestamp"))?;

        let paths = txn
            .query_all_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT DISTINCT path FROM memory_head
                 WHERE scope_key = 'repo' AND namespace = ? AND path LIKE ? ESCAPE '\\'",
                [namespace.clone().into(), descendant_pattern.into()],
            ))
            .await
            .map_err(|error| projection_storage("read Memory child paths", error))?;
        let child_prefix = format!("{path}.");
        let mut children = BTreeSet::new();
        for row in paths {
            let candidate: String = row.try_get("", "path").map_err(storage_error)?;
            if let Some(child) = candidate
                .strip_prefix(&child_prefix)
                .and_then(|rest| rest.split('.').next())
            {
                children.insert(child.to_owned());
            }
        }
        let child_count = children.len();
        let preview_row = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT document.summary FROM memory_head AS head
                 JOIN memory_episode_search_doc AS document
                   ON document.note_id = head.note_id
                  AND document.revision_oid = head.live_revision_oid
                 WHERE head.scope_key = 'repo' AND head.namespace = ? AND head.path = ?
                 ORDER BY head.note_id LIMIT 1",
                [namespace.clone().into(), path.clone().into()],
            ))
            .await
            .map_err(|error| projection_storage("read Memory path preview", error))?;
        let preview = match preview_row {
            Some(row) => row
                .try_get::<String>("", "summary")
                .map_err(storage_error)?
                .chars()
                .take(240)
                .collect(),
            None => String::new(),
        };

        execute(
            txn,
            "INSERT INTO memory_path_summary (
                scope_key, namespace, path, confirmed_count, quarantined_count,
                child_count, prefix_count, preview, last_changed_at
             ) VALUES ('repo', ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(scope_key, namespace, path) DO UPDATE SET
                confirmed_count = excluded.confirmed_count,
                quarantined_count = excluded.quarantined_count,
                child_count = excluded.child_count,
                prefix_count = excluded.prefix_count,
                preview = excluded.preview,
                last_changed_at = excluded.last_changed_at",
            vec![
                namespace.into(),
                path.into(),
                confirmed_count.into(),
                quarantined_count.into(),
                i64::try_from(child_count)
                    .map_err(|_| corrupt_projection("Memory child count exceeds SQLite range"))?
                    .into(),
                prefix_count.into(),
                preview.into(),
                last_changed_at.into(),
            ],
        )
        .await
        .map_err(storage_error)?;
    }
    Ok(())
}

async fn upsert_watermark(
    txn: &DatabaseTransaction,
    pinned_head: ObjectHash,
    last_event_seq: u64,
    policy_version: &str,
    rebuilt_at_ms: i64,
) -> Result<(), MemoryWriterError> {
    execute(
        txn,
        "INSERT INTO memory_projection_state (
            scope_key, projected_ref_oid, last_event_seq, schema_version,
            policy_version, rebuilt_at
         ) VALUES ('repo', ?, ?, 1, ?, ?)
         ON CONFLICT(scope_key) DO UPDATE SET
            projected_ref_oid = excluded.projected_ref_oid,
            last_event_seq = excluded.last_event_seq,
            schema_version = excluded.schema_version,
            policy_version = excluded.policy_version,
            rebuilt_at = excluded.rebuilt_at",
        vec![
            pinned_head.to_string().into(),
            i64::try_from(last_event_seq)
                .map_err(|_| corrupt_projection("Memory watermark exceeds SQLite range"))?
                .into(),
            policy_version.into(),
            rebuilt_at_ms.into(),
        ],
    )
    .await
    .map_err(storage_error)
}

fn episode_search_document(
    note: &MemoryNoteV1,
    revision_oid: ObjectHash,
) -> Result<Option<EpisodeSearchDocument>, MemoryWriterError> {
    let Some(episode) = &note.episode else {
        return Ok(None);
    };
    let root = match episode.root_kind {
        super::domain::EpisodeRootKind::Task => EpisodeRoot::task(episode.root_id.clone()),
        super::domain::EpisodeRootKind::Intent => EpisodeRoot::intent(episode.root_id.clone()),
    }
    .map_err(MemoryWriterError::from)?;
    let join_claims = |claims: &[super::domain::EpisodeClaimV1]| {
        claims
            .iter()
            .map(|claim| claim.claim.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let text = EpisodeSearchText::new(
        &episode.goal.claim,
        &episode.summary.claim,
        join_claims(&episode.decisions),
        join_claims(&episode.failed_attempts),
        join_claims(&episode.unresolved),
    )
    .map_err(fts_error)?;
    Ok(Some(EpisodeSearchDocument::new(
        root,
        revision_oid,
        episode.completion_status,
        episode.code_change_status,
        episode.ended_at,
        text,
    )))
}

fn parse_action(value: &str) -> Result<MemoryEventAction, MemoryWriterError> {
    serde_json::from_str(&format!("\"{value}\""))
        .map_err(|_| corrupt_projection("Memory head action is invalid"))
}

fn parse_review_state(value: &str) -> Result<ProjectedReviewState, MemoryWriterError> {
    match value {
        "draft" => Ok(ProjectedReviewState::Draft),
        "confirmed" => Ok(ProjectedReviewState::Confirmed),
        "quarantined" => Ok(ProjectedReviewState::Quarantined),
        "revoked" => Ok(ProjectedReviewState::Revoked),
        "superseded" => Ok(ProjectedReviewState::Superseded),
        "forgotten" => Ok(ProjectedReviewState::Forgotten),
        _ => Err(corrupt_projection("Memory head review state is invalid")),
    }
}

const fn review_state_label(value: ProjectedReviewState) -> &'static str {
    match value {
        ProjectedReviewState::Draft => "draft",
        ProjectedReviewState::Confirmed => "confirmed",
        ProjectedReviewState::Quarantined => "quarantined",
        ProjectedReviewState::Revoked => "revoked",
        ProjectedReviewState::Superseded => "superseded",
        ProjectedReviewState::Forgotten => "forgotten",
    }
}

fn projection_error(summary: &'static str) -> MemoryWriterError {
    MemoryWriterError::new(MemoryWriterErrorKind::CorruptProjection, summary)
}

fn corrupt_projection(summary: &'static str) -> MemoryWriterError {
    projection_error(summary)
}

fn projection_storage(action: &'static str, error: impl std::fmt::Display) -> MemoryWriterError {
    MemoryWriterError::new(
        MemoryWriterErrorKind::StorageFailure,
        format!("{action} failed: {error}"),
    )
}

fn storage_error(error: impl std::fmt::Display) -> MemoryWriterError {
    projection_storage("materialize Memory projection", error)
}

fn fts_error(error: impl std::fmt::Display) -> MemoryWriterError {
    projection_storage("maintain Memory FTS projection", error)
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use sea_orm::{ConnectionTrait, QueryResult};
    use tokio::sync::Notify;

    use super::*;
    use crate::internal::ai::memory::{
        domain::{MemoryLinkKind, MemoryLinkV1},
        policy::TrustedMemoryTarget,
        writer::tests::{file_backed_fixture, fixture, proposal},
    };

    fn projection_for(fixture: &super::super::writer::tests::Fixture) -> MemoryProjection {
        MemoryProjection::new(
            Arc::clone(&fixture.database),
            fixture._temp.path().to_path_buf(),
            "repo-policy-v1",
        )
    }

    async fn commit_generation(
        fixture: &super::super::writer::tests::Fixture,
        generation: u8,
        expected_head: Option<ObjectHash>,
    ) -> super::super::writer::CommittedMemoryEnvelope {
        fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, generation),
                expected_head,
            )
            .await
            .expect("commit Memory fixture generation")
    }

    async fn selected_lines(database: &DatabaseConnection, sql: &str) -> Vec<String> {
        database
            .query_all_raw(Statement::from_string(
                database.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("query semantic projection rows")
            .into_iter()
            .map(|row: QueryResult| row.try_get("", "value").expect("decode semantic row"))
            .collect()
    }

    async fn semantic_snapshot(database: &DatabaseConnection) -> Vec<String> {
        let queries = [
            "SELECT 'note|' || json_object(
                    'note_id', note_id, 'scope_key', scope_key, 'namespace', namespace,
                    'path', path, 'kind', kind, 'lifecycle', lifecycle,
                    'review_state', review_state, 'confidence', confidence, 'trust', trust,
                    'sensitivity', sensitivity, 'visibility', visibility,
                    'acl_policy_id', acl_policy_id, 'origin', origin,
                    'idempotency_key', idempotency_key,
                    'idempotency_scope', idempotency_scope, 'created_at', created_at) AS value
             FROM memory_note_index ORDER BY note_id",
            "SELECT 'revision|' || json_object(
                    'revision_oid', revision_oid, 'note_id', note_id, 'scope_key', scope_key,
                    'namespace', namespace, 'origin', origin, 'producer', producer,
                    'rules_version', rules_version, 'prompt_version', prompt_version,
                    'model_id', model_id, 'policy_version', policy_version,
                    'input_fingerprints_json', input_fingerprints_json, 'created_at', created_at)
                    AS value
             FROM memory_revision_index ORDER BY revision_oid",
            "SELECT 'head|' || json_object(
                    'scope_key', scope_key, 'namespace', namespace, 'path', path,
                    'note_id', note_id, 'latest_revision_oid', latest_revision_oid,
                    'live_revision_oid', live_revision_oid, 'latest_action', latest_action,
                    'latest_review_state', latest_review_state, 'kind', kind,
                    'lifecycle', lifecycle, 'confidence', confidence, 'trust', trust,
                    'sensitivity', sensitivity, 'visibility', visibility,
                    'acl_policy_id', acl_policy_id, 'valid_from', valid_from,
                    'valid_until', valid_until, 'effective_from_commit', effective_from_commit,
                    'effective_until_commit', effective_until_commit, 'expires_at', expires_at,
                    'rank_hint', rank_hint, 'last_event_seq', last_event_seq,
                    'updated_at', updated_at) AS value
             FROM memory_head ORDER BY note_id",
            "SELECT 'link|' || json_object(
                    'source_scope_key', source_scope_key,
                    'source_namespace', source_namespace, 'source_note_id', source_note_id,
                    'source_revision_oid', source_revision_oid,
                    'target_note_id', target_note_id,
                    'target_revision_oid', target_revision_oid, 'link_kind', link_kind,
                    'source_path', source_path, 'target_path', target_path,
                    'evidence_refs_json', evidence_refs_json,
                    'valid_from', valid_from, 'valid_until', valid_until) AS value
             FROM memory_link_index
             ORDER BY source_revision_oid, target_note_id, link_kind",
            "SELECT 'path|' || json_object(
                    'scope_key', scope_key, 'namespace', namespace, 'path', path,
                    'confirmed_count', confirmed_count,
                    'quarantined_count', quarantined_count, 'child_count', child_count,
                    'prefix_count', prefix_count, 'preview', preview,
                    'last_changed_at', last_changed_at) AS value
             FROM memory_path_summary ORDER BY namespace, path",
            "SELECT 'episode-path|' || json_object(
                    'note_id', note_id, 'revision_oid', revision_oid, 'code_path', code_path)
                    AS value
             FROM memory_episode_path ORDER BY note_id, revision_oid, code_path",
            "SELECT 'search|' || json_object(
                    'note_id', note_id, 'revision_oid', revision_oid,
                    'root_kind', root_kind, 'root_id', root_id,
                    'completion_status', completion_status,
                    'code_change_status', code_change_status, 'ended_at', ended_at,
                    'goal', goal, 'summary', summary, 'decisions', decisions,
                    'failed_attempts', failed_attempts, 'unresolved', unresolved) AS value
             FROM memory_episode_search_doc ORDER BY note_id, revision_oid",
            "SELECT 'watermark|' || json_object(
                    'scope_key', scope_key, 'projected_ref_oid', projected_ref_oid,
                    'last_event_seq', last_event_seq, 'schema_version', schema_version,
                    'policy_version', policy_version) AS value
             FROM memory_projection_state ORDER BY scope_key",
            "SELECT 'fts-retry|' || COUNT(*) AS value FROM memory_episode_fts
             WHERE memory_episode_fts MATCH 'retry'",
            "SELECT 'fts-generation|' || COUNT(*) AS value FROM memory_episode_fts
             WHERE memory_episode_fts MATCH 'generation'",
            "SELECT 'fts-timing|' || COUNT(*) AS value FROM memory_episode_fts
             WHERE memory_episode_fts MATCH 'timing'",
            "SELECT 'fts-other-scope|' || COUNT(*) AS value FROM memory_episode_fts
             WHERE memory_episode_fts MATCH 'other'",
        ];
        let mut snapshot = Vec::new();
        for query in queries {
            snapshot.extend(selected_lines(database, query).await);
        }
        snapshot
    }

    async fn set_memory_ref(database: &DatabaseConnection, head: ObjectHash) {
        let result = database
            .execute_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "UPDATE reference SET `commit` = ?
                 WHERE kind = 'Branch' AND remote IS NULL AND name = 'libra/memory/repo'",
                [head.to_string().into()],
            ))
            .await
            .expect("move test Memory ref");
        assert_eq!(result.rows_affected(), 1);
    }

    async fn seed_other_scope_projection(database: &DatabaseConnection) {
        for statement in [
            "INSERT INTO memory_note_index (
                note_id, scope_key, namespace, path, kind, lifecycle,
                review_state, confidence, trust, sensitivity, visibility,
                acl_policy_id, origin, idempotency_key, created_at
             ) VALUES (
                '11111111-1111-4111-8111-111111111111', 'repo-user', 'default',
                'episodic.tasks.other-scope', 'episodic', 'accretive', 'confirmed',
                'high', 'repo_evidence', 'internal', 'repo_local', 'repo-user-policy',
                'episode_compiler', 'other-scope-key', '2026-08-25T00:00:00Z'
             )",
            "INSERT INTO memory_revision_index (
                revision_oid, note_id, scope_key, namespace, origin, producer,
                rules_version, policy_version, input_fingerprints_json, created_at
             ) VALUES (
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                '11111111-1111-4111-8111-111111111111', 'repo-user', 'default',
                'episode_compiler', 'other-scope-test', 1, 'repo-policy-v1', '[]',
                '2026-08-25T00:00:00Z'
             )",
            "INSERT INTO memory_head (
                scope_key, namespace, path, note_id, latest_revision_oid,
                live_revision_oid, latest_action, latest_review_state, kind,
                lifecycle, confidence, trust, sensitivity, visibility,
                acl_policy_id, last_event_seq, updated_at
             ) VALUES (
                'repo-user', 'default', 'episodic.tasks.other-scope',
                '11111111-1111-4111-8111-111111111111',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'created', 'confirmed',
                'episodic', 'accretive', 'high', 'repo_evidence', 'internal',
                'repo_local', 'repo-user-policy', 1, '2026-08-25T00:00:00Z'
             )",
            "INSERT INTO memory_path_summary (
                scope_key, namespace, path, confirmed_count, quarantined_count,
                child_count, prefix_count, preview, last_changed_at
             ) VALUES (
                'repo-user', 'default', 'episodic.tasks.other-scope', 1, 0, 0, 1,
                'other scope', '2026-08-25T00:00:00Z'
             )",
            "INSERT INTO memory_episode_path (note_id, revision_oid, code_path)
             VALUES (
                '11111111-1111-4111-8111-111111111111',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'src/other_scope.rs'
             )",
            "INSERT INTO memory_episode_search_doc (
                rowid, note_id, revision_oid, root_kind, root_id,
                completion_status, code_change_status, ended_at, goal, summary,
                decisions, failed_attempts, unresolved
             ) VALUES (
                -1, '11111111-1111-4111-8111-111111111111',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'task', 'other-scope-task',
                'completed', 'unchanged', '2026-08-25T00:00:00Z',
                'other scope goal', 'other scope summary', '', '', ''
             )",
            "INSERT INTO memory_episode_fts (
                rowid, goal, summary, decisions, failed_attempts, unresolved
             ) VALUES (-1, 'other scope goal', 'other scope summary', '', '', '')",
            "INSERT INTO memory_projection_state (
                scope_key, projected_ref_oid, last_event_seq, schema_version,
                policy_version, rebuilt_at
             ) VALUES (
                'repo-user', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 1, 1,
                'repo-policy-v1', 1
             )",
        ] {
            database
                .execute_unprepared(statement)
                .await
                .expect("seed unrelated Memory scope projection");
        }
    }

    #[tokio::test]
    async fn projection_rebuild_equivalence() {
        let fixture = fixture().await;
        let first = commit_generation(&fixture, 1, None).await;
        let second = commit_generation(&fixture, 2, Some(first.commit_oid())).await;
        let linked_target = TrustedMemoryTarget::episode(
            EpisodeRoot::task("task-43").expect("construct linked task root"),
        );
        let mut linked_proposal = proposal(&linked_target, fixture.key_id, 1);
        linked_proposal.note_mut().links.push(MemoryLinkV1 {
            kind: MemoryLinkKind::Supports,
            target_note_id: fixture.target.root().note_id(),
            target_revision_oid: Some(second.revision_oid().to_string()),
            evidence_refs: Vec::new(),
            valid_from: None,
            valid_until: None,
        });
        let linked = fixture
            .writer
            .commit(
                &fixture.context,
                &linked_target,
                &linked_proposal,
                Some(second.commit_oid()),
            )
            .await
            .expect("commit linked Memory fixture");
        fixture
            .database
            .execute_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "INSERT INTO memory_compile_observer_state(
                    scope_key, source_ref_name, scanned_through_oid, updated_at
                 ) VALUES ('repo', 'libra/memory/repo', ?, 77)",
                [linked.commit_oid().to_string().into()],
            ))
            .await
            .expect("seed non-projection observer state");
        seed_other_scope_projection(&fixture.database).await;
        let before = semantic_snapshot(&fixture.database).await;
        let projection = projection_for(&fixture);

        projection
            .rebuild(linked.commit_oid(), 1234)
            .await
            .expect("rebuild Memory projection");

        assert_eq!(semantic_snapshot(&fixture.database).await, before);
        assert_eq!(
            selected_lines(
                &fixture.database,
                "SELECT source_ref_name || '|' || scanned_through_oid || '|' || updated_at
                 AS value FROM memory_compile_observer_state ORDER BY source_ref_name",
            )
            .await,
            vec![format!("libra/memory/repo|{}|77", linked.commit_oid())],
        );
        assert_eq!(
            read_memory_ref_head(fixture.database.as_ref())
                .await
                .expect("read Memory ref after rebuild"),
            Some(linked.commit_oid()),
        );
        assert_eq!(
            projection
                .status(Some(linked.commit_oid()))
                .await
                .expect("read projection status"),
            MemoryProjectionStatus::Current {
                head: linked.commit_oid(),
                last_event_seq: 6,
            },
        );
    }

    #[tokio::test]
    async fn projection_incremental_idempotent() {
        let fixture = fixture().await;
        let first = commit_generation(&fixture, 1, None).await;
        let second = commit_generation(&fixture, 2, Some(first.commit_oid())).await;
        let expected = semantic_snapshot(&fixture.database).await;
        let projection = projection_for(&fixture);

        set_memory_ref(&fixture.database, first.commit_oid()).await;
        projection
            .rebuild(first.commit_oid(), 100)
            .await
            .expect("rebuild first projection generation");
        set_memory_ref(&fixture.database, second.commit_oid()).await;
        projection
            .advance(second.commit_oid(), 200)
            .await
            .expect("advance one projection generation");
        assert_eq!(semantic_snapshot(&fixture.database).await, expected);

        projection
            .advance(second.commit_oid(), 300)
            .await
            .expect("repeated advance is a no-op");
        assert_eq!(semantic_snapshot(&fixture.database).await, expected);
    }

    #[tokio::test]
    async fn projection_corruption_stops_watermark() {
        let fixture = fixture().await;
        let committed = commit_generation(&fixture, 1, None).await;
        let before = semantic_snapshot(&fixture.database).await;
        let hash = committed.commit_oid().to_string();
        let object_path = fixture
            ._temp
            .path()
            .join("objects")
            .join(&hash[..2])
            .join(&hash[2..]);
        fs::remove_file(object_path).expect("remove temporary commit object");

        let error = projection_for(&fixture)
            .rebuild(committed.commit_oid(), 500)
            .await
            .expect_err("missing authority object stops rebuild");
        assert_eq!(error.kind(), MemoryWriterErrorKind::CorruptHistory);
        assert_eq!(
            error.damage_point(),
            Some(&MemoryDamagePoint::MemoryHead {
                oid: committed.commit_oid(),
            })
        );
        assert_eq!(semantic_snapshot(&fixture.database).await, before);
    }

    #[tokio::test]
    async fn projection_status_is_constant_work_and_dry_run_owns_full_validation() {
        let fixture = fixture().await;
        let committed = commit_generation(&fixture, 1, None).await;
        let projection = projection_for(&fixture);
        let expected = projection
            .status(Some(committed.commit_oid()))
            .await
            .expect("read current projection status");

        let revision = committed.revision_oid().to_string();
        let revision_path = fixture
            ._temp
            .path()
            .join("objects")
            .join(&revision[..2])
            .join(&revision[2..]);
        fs::remove_file(revision_path).expect("remove temporary revision object");

        assert_eq!(
            projection
                .status(Some(committed.commit_oid()))
                .await
                .expect("status reads only head manifest and watermark"),
            expected,
        );
        let error = projection
            .plan_rebuild(committed.commit_oid())
            .await
            .expect_err("dry-run validation still traverses the full authority");
        assert_eq!(error.kind(), MemoryWriterErrorKind::CorruptHistory);
        assert!(
            error
                .damage_point()
                .is_some_and(|point| point.to_string().contains("event_seq=")),
            "full validation should identify the damaged event"
        );
    }

    #[tokio::test]
    async fn projection_status_reads_ref_and_watermark_from_one_snapshot() {
        let fixture = file_backed_fixture().await;
        let first = commit_generation(&fixture, 1, None).await;
        let second = commit_generation(&fixture, 2, Some(first.commit_oid())).await;
        let first_seq =
            load_head_manifest(fixture._temp.path(), first.commit_oid(), "repo-policy-v1")
                .expect("load first Memory manifest")
                .last_event_seq;
        let second_seq =
            load_head_manifest(fixture._temp.path(), second.commit_oid(), "repo-policy-v1")
                .expect("load second Memory manifest")
                .last_event_seq;

        let reset = MemoryWriteTransaction::begin(fixture.database.as_ref())
            .await
            .expect("begin projection reset");
        reset
            .as_database_transaction()
            .execute_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "UPDATE reference SET `commit` = ? WHERE kind = 'Branch' AND remote IS NULL AND name = 'libra/memory/repo'",
                [first.commit_oid().to_string().into()],
            ))
            .await
            .expect("reset Memory ref to first generation");
        reset
            .as_database_transaction()
            .execute_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "UPDATE memory_projection_state SET projected_ref_oid = ?, last_event_seq = ? WHERE scope_key = 'repo'",
                [
                    first.commit_oid().to_string().into(),
                    i64::try_from(first_seq).expect("first sequence fits SQLite").into(),
                ],
            ))
            .await
            .expect("reset projection watermark to first generation");
        reset.commit().await.expect("commit projection reset");

        let hook = StatusSnapshotHook {
            after_head_read: Arc::new(Notify::new()),
            resume: Arc::new(Notify::new()),
        };
        let after_head_read_signal = Arc::clone(&hook.after_head_read);
        let after_head_read = after_head_read_signal.notified();
        let resume = Arc::clone(&hook.resume);
        let projection = projection_for(&fixture).with_status_snapshot_hook(hook);
        let status_task = tokio::spawn(async move { projection.status_consistent().await });
        tokio::time::timeout(Duration::from_secs(5), after_head_read)
            .await
            .expect("status read the first ref inside its snapshot");

        let writer_database = crate::internal::db::open_database_without_migrations(
            &fixture._temp.path().join("libra.db"),
        )
        .await
        .expect("open independent writer connection");
        let advance = crate::internal::db::begin_write_transaction(&writer_database)
            .await
            .expect("begin concurrent Memory advance");
        advance
            .execute_raw(Statement::from_sql_and_values(
                writer_database.get_database_backend(),
                "UPDATE reference SET `commit` = ? WHERE kind = 'Branch' AND remote IS NULL AND name = 'libra/memory/repo'",
                [second.commit_oid().to_string().into()],
            ))
            .await
            .expect("advance Memory ref");
        advance
            .execute_raw(Statement::from_sql_and_values(
                writer_database.get_database_backend(),
                "UPDATE memory_projection_state SET projected_ref_oid = ?, last_event_seq = ? WHERE scope_key = 'repo'",
                [
                    second.commit_oid().to_string().into(),
                    i64::try_from(second_seq)
                        .expect("second sequence fits SQLite")
                        .into(),
                ],
            ))
            .await
            .expect("advance projection watermark");
        advance.commit().await.expect("commit concurrent advance");
        resume.notify_one();

        let (head, status) = tokio::time::timeout(Duration::from_secs(5), status_task)
            .await
            .expect("status snapshot completed")
            .expect("status task joined")
            .expect("status read succeeded");
        assert_eq!(head, Some(first.commit_oid()));
        assert_eq!(
            status,
            MemoryProjectionStatus::Current {
                head: first.commit_oid(),
                last_event_seq: first_seq,
            },
            "status must not combine the old ref with the newly committed watermark"
        );
        writer_database
            .close()
            .await
            .expect("close independent writer connection");
    }

    #[tokio::test]
    async fn projection_stale_fails_closed() {
        let fixture = fixture().await;
        let first = commit_generation(&fixture, 1, None).await;
        let _second = commit_generation(&fixture, 2, Some(first.commit_oid())).await;

        let error = projection_for(&fixture)
            .advance(first.commit_oid(), 600)
            .await
            .expect_err("stale pinned head fails closed");
        assert_eq!(error.kind(), MemoryWriterErrorKind::ProjectionStale);
        assert_eq!(error.stable_code(), "LBR-MEMORY-PROJECTION-STALE");
        assert_eq!(
            error.to_string(),
            "LBR-MEMORY-PROJECTION-STALE: pinned Memory ref no longer matches the repository ref",
        );
    }

    #[tokio::test]
    async fn projection_rejects_same_head_with_wrong_sequence() {
        let fixture = fixture().await;
        let committed = commit_generation(&fixture, 1, None).await;
        fixture
            .database
            .execute_unprepared(
                "UPDATE memory_projection_state SET last_event_seq = 1
                 WHERE scope_key = 'repo'",
            )
            .await
            .expect("corrupt projection sequence");

        let error = projection_for(&fixture)
            .advance(committed.commit_oid(), 650)
            .await
            .expect_err("same-head sequence mismatch fails closed");
        assert_eq!(error.kind(), MemoryWriterErrorKind::CorruptProjection);
        assert_eq!(
            projection_for(&fixture)
                .status(Some(committed.commit_oid()))
                .await
                .expect("diagnose corrupt projection"),
            MemoryProjectionStatus::Corrupt {
                head: Some(committed.commit_oid()),
                projected: Some(committed.commit_oid().to_string()),
                last_event_seq: Some(1),
            },
        );
    }

    #[tokio::test]
    async fn projection_rebuild_transaction_failure_rolls_back() {
        let fixture = fixture().await;
        let committed = commit_generation(&fixture, 1, None).await;
        let before = semantic_snapshot(&fixture.database).await;
        fixture
            .database
            .execute_unprepared(
                "CREATE TRIGGER fail_memory_path_summary
                 BEFORE INSERT ON memory_path_summary
                 BEGIN SELECT RAISE(ABORT, 'simulated projection failure'); END;",
            )
            .await
            .expect("install projection failure trigger");

        projection_for(&fixture)
            .rebuild(committed.commit_oid(), 700)
            .await
            .expect_err("transaction failure rolls back rebuild");
        assert_eq!(semantic_snapshot(&fixture.database).await, before);
    }
}
