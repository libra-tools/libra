//! Atomic owner for the Episode external-content FTS5 projection.
//!
//! Callers own the surrounding SQLite transaction. Keeping that boundary
//! outside this module lets a future projection writer update ordinary Memory
//! indexes, the search document, FTS postings, and its watermark atomically.

use std::{collections::HashSet, str::FromStr};

use chrono::{DateTime, Utc};
use git_internal::hash::ObjectHash;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr, Statement, Value};
use thiserror::Error;

use super::{
    domain::{CodeChangeStatus, CompletionStatus, EpisodeRoot, EpisodeRootKind},
    query::{EpisodePathFilter, EpisodeQueryV1, MAX_CANDIDATES},
};
use crate::internal::{ai::linear_ref::LinearRefWriteTransaction, db};

const MAX_SEARCH_TEXT_BYTES: usize = 64 * 1024;
const MAX_MATCH_INPUT_BYTES: usize = 4 * 1024;
const MAX_MATCH_TERM_BYTES: usize = 256;
const MAX_MATCH_TERMS: usize = 32;
const MATCH_QUERY_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub(crate) enum MemoryFtsError {
    #[error("invalid Memory search document field: {field}")]
    InvalidDocument { field: &'static str },
    #[error("invalid plain-text Memory search query: {reason}")]
    InvalidQuery { reason: &'static str },
    #[error("Memory search projection is internally inconsistent")]
    CorruptProjection,
    #[error("Memory search storage operation failed")]
    Storage(#[from] DbErr),
}

/// A caller-owned transaction that acquired SQLite's write lock before any
/// reads were allowed.
///
/// External-content maintenance is read-then-write: updates and deletes must
/// fetch the old text before removing its posting. Accepting an arbitrary
/// deferred transaction would allow a read-lock-to-write-lock upgrade, where
/// SQLite returns `SQLITE_BUSY` immediately instead of honoring busy timeout.
/// The private field prevents callers from certifying an arbitrary transaction
/// as write-locked.
pub(crate) struct MemoryWriteTransaction(DatabaseTransaction);

impl MemoryWriteTransaction {
    pub(crate) async fn begin(database: &DatabaseConnection) -> Result<Self, MemoryFtsError> {
        Ok(Self(db::begin_write_transaction(database).await?))
    }

    /// Borrow the same transaction for the other Memory projection writes
    /// that must commit atomically with the FTS posting.
    pub(crate) const fn as_database_transaction(&self) -> &DatabaseTransaction {
        &self.0
    }

    pub(crate) async fn commit(self) -> Result<(), MemoryFtsError> {
        self.0.commit().await?;
        Ok(())
    }

    pub(crate) async fn rollback(self) -> Result<(), MemoryFtsError> {
        self.0.rollback().await?;
        Ok(())
    }
}

pub(crate) struct EpisodeSearchText {
    goal: String,
    summary: String,
    decisions: String,
    failed_attempts: String,
    unresolved: String,
}

impl EpisodeSearchText {
    pub(crate) fn new(
        goal: impl Into<String>,
        summary: impl Into<String>,
        decisions: impl Into<String>,
        failed_attempts: impl Into<String>,
        unresolved: impl Into<String>,
    ) -> Result<Self, MemoryFtsError> {
        let text = Self {
            goal: goal.into(),
            summary: summary.into(),
            decisions: decisions.into(),
            failed_attempts: failed_attempts.into(),
            unresolved: unresolved.into(),
        };
        let fields = [
            ("goal", text.goal.as_str()),
            ("summary", text.summary.as_str()),
            ("decisions", text.decisions.as_str()),
            ("failed_attempts", text.failed_attempts.as_str()),
            ("unresolved", text.unresolved.as_str()),
        ];
        let mut total_bytes = 0usize;
        for (field, value) in fields {
            if value.contains('\0') {
                return Err(MemoryFtsError::InvalidDocument { field });
            }
            total_bytes =
                total_bytes
                    .checked_add(value.len())
                    .ok_or(MemoryFtsError::InvalidDocument {
                        field: "search_text",
                    })?;
        }
        if total_bytes > MAX_SEARCH_TEXT_BYTES {
            return Err(MemoryFtsError::InvalidDocument {
                field: "search_text",
            });
        }
        Ok(text)
    }
}

pub(crate) struct EpisodeSearchDocument {
    root: EpisodeRoot,
    revision_oid: ObjectHash,
    completion_status: CompletionStatus,
    code_change_status: CodeChangeStatus,
    ended_at: Option<DateTime<Utc>>,
    text: EpisodeSearchText,
}

impl EpisodeSearchDocument {
    pub(crate) const fn new(
        root: EpisodeRoot,
        revision_oid: ObjectHash,
        completion_status: CompletionStatus,
        code_change_status: CodeChangeStatus,
        ended_at: Option<DateTime<Utc>>,
        text: EpisodeSearchText,
    ) -> Self {
        Self {
            root,
            revision_oid,
            completion_status,
            code_change_status,
            ended_at,
            text,
        }
    }

    fn note_id(&self) -> String {
        self.root.note_id().to_string()
    }

    fn revision_oid(&self) -> String {
        self.revision_oid.to_string()
    }

    fn root_kind(&self) -> &'static str {
        match self.root.kind() {
            EpisodeRootKind::Task => "task",
            EpisodeRootKind::Intent => "intent",
        }
    }

    fn completion_status(&self) -> &'static str {
        match self.completion_status {
            CompletionStatus::Completed => "completed",
            CompletionStatus::Failed => "failed",
            CompletionStatus::Cancelled => "cancelled",
        }
    }

    fn code_change_status(&self) -> &'static str {
        match self.code_change_status {
            CodeChangeStatus::Changed => "changed",
            CodeChangeStatus::Unchanged => "unchanged",
            CodeChangeStatus::Unknown => "unknown",
        }
    }

    fn ended_at(&self) -> Option<String> {
        self.ended_at.map(|value| value.to_rfc3339())
    }
}

pub(crate) struct BoundMatchQuery(String);

impl BoundMatchQuery {
    pub(crate) const fn version(&self) -> u32 {
        MATCH_QUERY_VERSION
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) struct EpisodeSearchCandidate {
    pub(crate) note_id: uuid::Uuid,
    pub(crate) revision_oid: ObjectHash,
    pub(crate) root_kind: EpisodeRootKind,
    pub(crate) root_id: String,
    pub(crate) completion_status: CompletionStatus,
    pub(crate) code_change_status: CodeChangeStatus,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) valid_from: Option<DateTime<Utc>>,
    pub(crate) valid_until: Option<DateTime<Utc>>,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    pub(crate) bm25_score: f64,
}

/// Read a bounded candidate set from the frozen projection transaction.
/// Every dynamic value is bound; the SQL shape and BM25 weights are fixed by
/// selector version rather than accepted from a caller.
pub(crate) async fn search_candidates<C: ConnectionTrait>(
    database: &C,
    query: &EpisodeQueryV1,
) -> Result<Vec<EpisodeSearchCandidate>, MemoryFtsError> {
    let mut values = Vec::<Value>::new();
    let mut sql = if query.text.is_some() {
        "SELECT document.note_id, document.revision_oid, document.root_kind,
                document.root_id, document.completion_status,
                document.code_change_status, document.ended_at,
                head.valid_from, head.valid_until, head.expires_at,
                bm25(memory_episode_fts, 8.0, 5.0, 4.0, 3.0, 2.0) AS bm25_score
         FROM memory_episode_fts
         JOIN memory_episode_search_doc AS document
           ON document.rowid = memory_episode_fts.rowid
         JOIN memory_head AS head
           ON head.scope_key = 'repo' AND head.namespace = 'default'
          AND head.note_id = document.note_id
          AND head.live_revision_oid = document.revision_oid
         WHERE memory_episode_fts MATCH ?
           AND head.latest_review_state = 'confirmed'"
            .to_string()
    } else {
        "SELECT document.note_id, document.revision_oid, document.root_kind,
                document.root_id, document.completion_status,
                document.code_change_status, document.ended_at,
                head.valid_from, head.valid_until, head.expires_at,
                0.0 AS bm25_score
         FROM memory_episode_search_doc AS document
         JOIN memory_head AS head
           ON head.scope_key = 'repo' AND head.namespace = 'default'
          AND head.note_id = document.note_id
          AND head.live_revision_oid = document.revision_oid
         WHERE head.latest_review_state = 'confirmed'"
            .to_string()
    };
    if let Some(text) = &query.text {
        values.push(normalize_plain_text_v1(text)?.as_str().to_string().into());
    }
    if let Some(root_kind) = query.root_kind {
        sql.push_str(" AND document.root_kind = ?");
        values.push(root_kind_label(root_kind).into());
    }
    if let Some(root_id) = &query.root_id {
        sql.push_str(" AND document.root_id = ?");
        values.push(root_id.as_str().into());
    }
    if let Some(status) = query.completion_status {
        sql.push_str(" AND document.completion_status = ?");
        values.push(completion_status_label(status).into());
    }
    if let Some(status) = query.code_change_status {
        sql.push_str(" AND document.code_change_status = ?");
        values.push(code_change_status_label(status).into());
    }
    if let Some(from) = query.ended_from {
        sql.push_str(" AND document.ended_at >= ?");
        values.push(from.to_rfc3339().into());
    }
    if let Some(until) = query.ended_until {
        sql.push_str(" AND document.ended_at <= ?");
        values.push(until.to_rfc3339().into());
    }
    if let Some(effective_at) = query.effective_at {
        let effective_at = effective_at.to_rfc3339();
        sql.push_str(
            " AND (head.valid_from IS NULL OR head.valid_from <= ?)
              AND (head.valid_until IS NULL OR head.valid_until > ?)
              AND (head.expires_at IS NULL OR head.expires_at > ?)",
        );
        values.push(effective_at.clone().into());
        values.push(effective_at.clone().into());
        values.push(effective_at.into());
    }
    match &query.path {
        Some(EpisodePathFilter::Exact(path)) => {
            sql.push_str(" AND head.path = ?");
            values.push(path.as_str().into());
        }
        Some(EpisodePathFilter::Prefix(path)) => {
            sql.push_str(" AND (head.path = ? OR (head.path >= ? AND head.path < ?))");
            values.push(path.as_str().into());
            values.push(format!("{path}.").into());
            values.push(format!("{path}.\u{10ffff}").into());
        }
        None => {}
    }
    sql.push_str(
        " ORDER BY bm25_score ASC, document.ended_at DESC,
                   document.note_id ASC, document.revision_oid ASC LIMIT ?",
    );
    values.push((MAX_CANDIDATES as i64).into());
    database
        .query_all_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            sql,
            values,
        ))
        .await?
        .into_iter()
        .map(|row| {
            let note_id: String = row
                .try_get("", "note_id")
                .map_err(|_| MemoryFtsError::CorruptProjection)?;
            let revision_oid: String = row
                .try_get("", "revision_oid")
                .map_err(|_| MemoryFtsError::CorruptProjection)?;
            let root_kind: String = row
                .try_get("", "root_kind")
                .map_err(|_| MemoryFtsError::CorruptProjection)?;
            let ended_at: Option<String> = row
                .try_get("", "ended_at")
                .map_err(|_| MemoryFtsError::CorruptProjection)?;
            let valid_from: Option<String> = row
                .try_get("", "valid_from")
                .map_err(|_| MemoryFtsError::CorruptProjection)?;
            let valid_until: Option<String> = row
                .try_get("", "valid_until")
                .map_err(|_| MemoryFtsError::CorruptProjection)?;
            let expires_at: Option<String> = row
                .try_get("", "expires_at")
                .map_err(|_| MemoryFtsError::CorruptProjection)?;
            Ok(EpisodeSearchCandidate {
                note_id: uuid::Uuid::parse_str(&note_id)
                    .map_err(|_| MemoryFtsError::CorruptProjection)?,
                revision_oid: ObjectHash::from_str(&revision_oid)
                    .map_err(|_| MemoryFtsError::CorruptProjection)?,
                root_kind: parse_root_kind(&root_kind)?,
                root_id: row
                    .try_get("", "root_id")
                    .map_err(|_| MemoryFtsError::CorruptProjection)?,
                completion_status: parse_completion_status(
                    &row.try_get::<String>("", "completion_status")
                        .map_err(|_| MemoryFtsError::CorruptProjection)?,
                )?,
                code_change_status: parse_code_change_status(
                    &row.try_get::<String>("", "code_change_status")
                        .map_err(|_| MemoryFtsError::CorruptProjection)?,
                )?,
                ended_at: ended_at
                    .map(|value| value.parse::<DateTime<Utc>>())
                    .transpose()
                    .map_err(|_| MemoryFtsError::CorruptProjection)?,
                valid_from: parse_optional_timestamp(valid_from)?,
                valid_until: parse_optional_timestamp(valid_until)?,
                expires_at: parse_optional_timestamp(expires_at)?,
                bm25_score: row
                    .try_get("", "bm25_score")
                    .map_err(|_| MemoryFtsError::CorruptProjection)?,
            })
        })
        .collect()
}

fn parse_optional_timestamp(
    value: Option<String>,
) -> Result<Option<DateTime<Utc>>, MemoryFtsError> {
    value
        .map(|value| value.parse::<DateTime<Utc>>())
        .transpose()
        .map_err(|_| MemoryFtsError::CorruptProjection)
}

const fn root_kind_label(kind: EpisodeRootKind) -> &'static str {
    match kind {
        EpisodeRootKind::Task => "task",
        EpisodeRootKind::Intent => "intent",
    }
}

const fn completion_status_label(status: CompletionStatus) -> &'static str {
    match status {
        CompletionStatus::Completed => "completed",
        CompletionStatus::Failed => "failed",
        CompletionStatus::Cancelled => "cancelled",
    }
}

const fn code_change_status_label(status: CodeChangeStatus) -> &'static str {
    match status {
        CodeChangeStatus::Changed => "changed",
        CodeChangeStatus::Unchanged => "unchanged",
        CodeChangeStatus::Unknown => "unknown",
    }
}

fn parse_root_kind(value: &str) -> Result<EpisodeRootKind, MemoryFtsError> {
    match value {
        "task" => Ok(EpisodeRootKind::Task),
        "intent" => Ok(EpisodeRootKind::Intent),
        _ => Err(MemoryFtsError::CorruptProjection),
    }
}

fn parse_completion_status(value: &str) -> Result<CompletionStatus, MemoryFtsError> {
    match value {
        "completed" => Ok(CompletionStatus::Completed),
        "failed" => Ok(CompletionStatus::Failed),
        "cancelled" => Ok(CompletionStatus::Cancelled),
        _ => Err(MemoryFtsError::CorruptProjection),
    }
}

fn parse_code_change_status(value: &str) -> Result<CodeChangeStatus, MemoryFtsError> {
    match value {
        "changed" => Ok(CodeChangeStatus::Changed),
        "unchanged" => Ok(CodeChangeStatus::Unchanged),
        "unknown" => Ok(CodeChangeStatus::Unknown),
        _ => Err(MemoryFtsError::CorruptProjection),
    }
}

/// Convert ordinary text to a bounded FTS5 literal expression.
///
/// Unicode letter/number runs are deduplicated with ASCII case folding, quoted,
/// and joined with a controlled `OR`. Strings such as `OR`, `NEAR`, `-foo`, or
/// `column:value` therefore remain literal terms rather than FTS syntax. The
/// result is still passed through `MATCH ?`; callers never provide SQL or FTS
/// fragments.
pub(crate) fn normalize_plain_text_v1(input: &str) -> Result<BoundMatchQuery, MemoryFtsError> {
    if input.is_empty() || input.len() > MAX_MATCH_INPUT_BYTES {
        return Err(MemoryFtsError::InvalidQuery {
            reason: "input size",
        });
    }
    if input.chars().any(char::is_control) {
        return Err(MemoryFtsError::InvalidQuery {
            reason: "control character",
        });
    }

    let mut literals = Vec::new();
    let mut seen_terms = HashSet::new();
    for term in input.split(|character: char| !character.is_alphanumeric()) {
        if term.is_empty() {
            continue;
        }
        if term.len() > MAX_MATCH_TERM_BYTES {
            return Err(MemoryFtsError::InvalidQuery {
                reason: "term size",
            });
        }
        if !seen_terms.insert(term.to_ascii_lowercase()) {
            continue;
        }
        if literals.len() == MAX_MATCH_TERMS {
            return Err(MemoryFtsError::InvalidQuery {
                reason: "term count",
            });
        }
        literals.push(format!("\"{}\"", term.replace('"', "\"\"")));
    }
    if literals.is_empty() {
        return Err(MemoryFtsError::InvalidQuery {
            reason: "no searchable term",
        });
    }
    Ok(BoundMatchQuery(literals.join(" OR ")))
}

/// Validate the public plain-text search contract without touching SQLite.
///
/// Command adapters call this before checking whether a repository currently
/// has any Memory. That keeps invalid-query behavior independent of repository
/// contents while leaving the normalized FTS expression private to this
/// module.
pub(crate) fn validate_plain_text_query(input: &str) -> Result<(), MemoryFtsError> {
    normalize_plain_text_v1(input).map(|_| ())
}

struct StoredSearchDocument {
    rowid: i64,
    goal: String,
    summary: String,
    decisions: String,
    failed_attempts: String,
    unresolved: String,
}

pub(crate) async fn upsert_document(
    transaction: &MemoryWriteTransaction,
    document: &EpisodeSearchDocument,
) -> Result<i64, MemoryFtsError> {
    upsert_document_on(transaction.as_database_transaction(), document).await
}

pub(super) async fn upsert_document_in_linear_transaction(
    transaction: &LinearRefWriteTransaction<'_>,
    document: &EpisodeSearchDocument,
) -> Result<i64, MemoryFtsError> {
    upsert_document_on(transaction.as_database_transaction(), document).await
}

async fn upsert_document_on(
    transaction: &DatabaseTransaction,
    document: &EpisodeSearchDocument,
) -> Result<i64, MemoryFtsError> {
    let note_id = document.note_id();
    let revision_oid = document.revision_oid();
    if let Some(stored) = read_document(transaction, &note_id, &revision_oid).await? {
        delete_posting(transaction, &stored).await?;
        let result = transaction
            .execute_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "UPDATE memory_episode_search_doc SET
                    root_kind = ?, root_id = ?, completion_status = ?,
                    code_change_status = ?, ended_at = ?, goal = ?, summary = ?,
                    decisions = ?, failed_attempts = ?, unresolved = ?
                 WHERE rowid = ? AND note_id = ? AND revision_oid = ?",
                [
                    document.root_kind().into(),
                    document.root.id().into(),
                    document.completion_status().into(),
                    document.code_change_status().into(),
                    document.ended_at().into(),
                    document.text.goal.as_str().into(),
                    document.text.summary.as_str().into(),
                    document.text.decisions.as_str().into(),
                    document.text.failed_attempts.as_str().into(),
                    document.text.unresolved.as_str().into(),
                    stored.rowid.into(),
                    note_id.into(),
                    revision_oid.into(),
                ],
            ))
            .await?;
        if result.rows_affected() != 1 {
            return Err(MemoryFtsError::CorruptProjection);
        }
        insert_posting(transaction, stored.rowid, &document.text).await?;
        return Ok(stored.rowid);
    }

    let result = transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "INSERT INTO memory_episode_search_doc (
                note_id, revision_oid, root_kind, root_id, completion_status,
                code_change_status, ended_at, goal, summary, decisions,
                failed_attempts, unresolved
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                note_id.into(),
                revision_oid.into(),
                document.root_kind().into(),
                document.root.id().into(),
                document.completion_status().into(),
                document.code_change_status().into(),
                document.ended_at().into(),
                document.text.goal.as_str().into(),
                document.text.summary.as_str().into(),
                document.text.decisions.as_str().into(),
                document.text.failed_attempts.as_str().into(),
                document.text.unresolved.as_str().into(),
            ],
        ))
        .await?;
    let rowid =
        i64::try_from(result.last_insert_id()).map_err(|_| MemoryFtsError::CorruptProjection)?;
    insert_posting(transaction, rowid, &document.text).await?;
    Ok(rowid)
}

pub(crate) async fn delete_document(
    transaction: &MemoryWriteTransaction,
    note_id: uuid::Uuid,
    revision_oid: ObjectHash,
) -> Result<bool, MemoryFtsError> {
    let transaction = transaction.as_database_transaction();
    let note_id = note_id.to_string();
    let revision_oid = revision_oid.to_string();
    let Some(stored) = read_document(transaction, &note_id, &revision_oid).await? else {
        return Ok(false);
    };
    delete_posting(transaction, &stored).await?;
    let result = transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "DELETE FROM memory_episode_search_doc
             WHERE rowid = ? AND note_id = ? AND revision_oid = ?",
            [stored.rowid.into(), note_id.into(), revision_oid.into()],
        ))
        .await?;
    if result.rows_affected() != 1 {
        return Err(MemoryFtsError::CorruptProjection);
    }
    Ok(true)
}

/// Remove only the FTS documents owned by one projection scope.
///
/// The FTS5 table is external-content, so deleting content rows directly would
/// leave stale postings behind. Resolve the scope through the authoritative
/// revision projection, issue one FTS5 delete command per stored document, and
/// then remove the matching content row in the same transaction.
pub(crate) async fn delete_scope_documents(
    transaction: &MemoryWriteTransaction,
    scope_key: &str,
) -> Result<u64, MemoryFtsError> {
    let database = transaction.as_database_transaction();
    let rows = database
        .query_all_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT d.note_id, d.revision_oid
             FROM memory_episode_search_doc AS d
             INNER JOIN memory_revision_index AS r
                ON r.note_id = d.note_id AND r.revision_oid = d.revision_oid
             WHERE r.scope_key = ?
             ORDER BY d.note_id, d.revision_oid",
            [scope_key.into()],
        ))
        .await?;
    let mut deleted = 0_u64;
    for row in rows {
        let note_id: String = row
            .try_get("", "note_id")
            .map_err(|_| MemoryFtsError::CorruptProjection)?;
        let revision_oid: String = row
            .try_get("", "revision_oid")
            .map_err(|_| MemoryFtsError::CorruptProjection)?;
        let note_id = note_id
            .parse()
            .map_err(|_| MemoryFtsError::CorruptProjection)?;
        let revision_oid = revision_oid
            .parse()
            .map_err(|_| MemoryFtsError::CorruptProjection)?;
        if delete_document(transaction, note_id, revision_oid).await? {
            deleted = deleted
                .checked_add(1)
                .ok_or(MemoryFtsError::CorruptProjection)?;
        }
    }
    Ok(deleted)
}

pub(crate) async fn rebuild_index(
    transaction: &MemoryWriteTransaction,
) -> Result<(), MemoryFtsError> {
    let transaction = transaction.as_database_transaction();
    transaction
        .execute_unprepared("INSERT INTO memory_episode_fts(memory_episode_fts) VALUES('rebuild')")
        .await?;
    verify_index(transaction).await
}

async fn verify_index(transaction: &DatabaseTransaction) -> Result<(), MemoryFtsError> {
    transaction
        .execute_unprepared(
            "INSERT INTO memory_episode_fts(memory_episode_fts, rank)
             VALUES('integrity-check', 1)",
        )
        .await?;
    Ok(())
}

async fn read_document(
    transaction: &DatabaseTransaction,
    note_id: &str,
    revision_oid: &str,
) -> Result<Option<StoredSearchDocument>, MemoryFtsError> {
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT rowid, goal, summary, decisions, failed_attempts, unresolved
             FROM memory_episode_search_doc
             WHERE note_id = ? AND revision_oid = ?",
            [note_id.into(), revision_oid.into()],
        ))
        .await?;
    row.map(|row| {
        Ok(StoredSearchDocument {
            rowid: row
                .try_get("", "rowid")
                .map_err(|_| MemoryFtsError::CorruptProjection)?,
            goal: row
                .try_get("", "goal")
                .map_err(|_| MemoryFtsError::CorruptProjection)?,
            summary: row
                .try_get("", "summary")
                .map_err(|_| MemoryFtsError::CorruptProjection)?,
            decisions: row
                .try_get("", "decisions")
                .map_err(|_| MemoryFtsError::CorruptProjection)?,
            failed_attempts: row
                .try_get("", "failed_attempts")
                .map_err(|_| MemoryFtsError::CorruptProjection)?,
            unresolved: row
                .try_get("", "unresolved")
                .map_err(|_| MemoryFtsError::CorruptProjection)?,
        })
    })
    .transpose()
}

async fn insert_posting(
    transaction: &DatabaseTransaction,
    rowid: i64,
    text: &EpisodeSearchText,
) -> Result<(), MemoryFtsError> {
    transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "INSERT INTO memory_episode_fts (
                rowid, goal, summary, decisions, failed_attempts, unresolved
             ) VALUES (?, ?, ?, ?, ?, ?)",
            [
                rowid.into(),
                text.goal.as_str().into(),
                text.summary.as_str().into(),
                text.decisions.as_str().into(),
                text.failed_attempts.as_str().into(),
                text.unresolved.as_str().into(),
            ],
        ))
        .await?;
    Ok(())
}

async fn delete_posting(
    transaction: &DatabaseTransaction,
    stored: &StoredSearchDocument,
) -> Result<(), MemoryFtsError> {
    transaction
        .execute_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "INSERT INTO memory_episode_fts (
                memory_episode_fts, rowid, goal, summary, decisions,
                failed_attempts, unresolved
             ) VALUES ('delete', ?, ?, ?, ?, ?, ?)",
            [
                stored.rowid.into(),
                stored.goal.as_str().into(),
                stored.summary.as_str().into(),
                stored.decisions.as_str().into(),
                stored.failed_attempts.as_str().into(),
                stored.unresolved.as_str().into(),
            ],
        ))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use git_internal::internal::object::types::ObjectType;
    use sea_orm::{Database, DatabaseConnection};

    use super::*;
    use crate::internal::db::migration::run_builtin_migrations;

    async fn test_database() -> DatabaseConnection {
        let database = Database::connect("sqlite::memory:")
            .await
            .expect("connect test database");
        run_builtin_migrations(&database)
            .await
            .expect("apply built-in migrations");
        database
    }

    fn revision_oid(seed: &[u8]) -> ObjectHash {
        ObjectHash::from_type_and_data(ObjectType::Blob, seed)
    }

    async fn seed_revision(
        database: &DatabaseConnection,
        root: &EpisodeRoot,
        revision_oid: ObjectHash,
    ) {
        database
            .execute_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "INSERT INTO memory_note_index (
                    note_id, scope_key, namespace, path, kind, lifecycle,
                    review_state, confidence, trust, sensitivity, visibility,
                    acl_policy_id, origin, idempotency_key, created_at
                 ) VALUES (?, 'repo', 'default', ?, 'episodic', 'accretive',
                    'confirmed', 'high', 'repo_evidence', 'internal',
                    'repo_local', 'default', 'episode_compiler', ?,
                    '2026-08-24T00:00:00Z')",
                [
                    root.note_id().to_string().into(),
                    root.path().into(),
                    format!("episode:{}", root.id()).into(),
                ],
            ))
            .await
            .expect("seed Memory note");
        database
            .execute_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "INSERT INTO memory_revision_index (
                    revision_oid, note_id, scope_key, namespace, origin,
                    producer, rules_version, policy_version,
                    input_fingerprints_json, created_at
                 ) VALUES (?, ?, 'repo', 'default', 'episode_compiler',
                    'test', 1, 'v1', '[]', '2026-08-24T00:00:00Z')",
                [
                    revision_oid.to_string().into(),
                    root.note_id().to_string().into(),
                ],
            ))
            .await
            .expect("seed Memory revision");
    }

    fn document(
        root: EpisodeRoot,
        revision_oid: ObjectHash,
        goal: &str,
        ended_at: Option<DateTime<Utc>>,
    ) -> EpisodeSearchDocument {
        EpisodeSearchDocument::new(
            root,
            revision_oid,
            CompletionStatus::Completed,
            CodeChangeStatus::Changed,
            ended_at,
            EpisodeSearchText::new(goal, "summary", "decision", "failure", "unresolved")
                .expect("valid search text"),
        )
    }

    async fn match_count(database: &DatabaseConnection, query: &str) -> i64 {
        database
            .query_one_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM memory_episode_fts
                 WHERE memory_episode_fts MATCH ?",
                [query.into()],
            ))
            .await
            .expect("query FTS")
            .expect("count row")
            .try_get("", "count")
            .expect("count value")
    }

    async fn assert_integrity(database: &DatabaseConnection) {
        database
            .execute_unprepared(
                "INSERT INTO memory_episode_fts(memory_episode_fts, rank)
                 VALUES('integrity-check', 1)",
            )
            .await
            .expect("external-content index remains consistent");
    }

    #[test]
    fn plain_text_normalizer_quotes_operators_and_rejects_unbounded_input() {
        let query = normalize_plain_text_v1("Alpha alpha ALPHA OR title:beta")
            .expect("ordinary text produces a bound query");
        assert_eq!(query.version(), 1);
        assert_eq!(
            query.as_str(),
            "\"Alpha\" OR \"OR\" OR \"title\" OR \"beta\""
        );

        assert!(normalize_plain_text_v1("").is_err());
        assert!(normalize_plain_text_v1("--- !!!").is_err());
        assert!(normalize_plain_text_v1("alpha\nbeta").is_err());
        assert!(normalize_plain_text_v1(&"x".repeat(MAX_MATCH_TERM_BYTES + 1)).is_err());
        let too_many_terms = (0..=MAX_MATCH_TERMS)
            .map(|index| format!("term{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(normalize_plain_text_v1(&too_many_terms).is_err());
    }

    #[tokio::test]
    async fn natural_language_query_matches_when_only_some_terms_are_present() {
        let database = test_database().await;
        let root = EpisodeRoot::task("task-fts-natural-language").expect("valid root");
        let oid = revision_oid(b"fts-natural-language");
        seed_revision(&database, &root, oid).await;

        let transaction = MemoryWriteTransaction::begin(&database)
            .await
            .expect("begin insert");
        upsert_document(
            &transaction,
            &document(root, oid, "cache invalidation", None),
        )
        .await
        .expect("insert search document");
        transaction.commit().await.expect("commit insert");

        let query = normalize_plain_text_v1("why did the agent change authentication/cache?")
            .expect("normalize natural-language query");
        assert_eq!(match_count(&database, query.as_str()).await, 1);
    }

    #[tokio::test]
    async fn external_content_insert_update_delete_preserves_rowid_and_integrity() {
        let database = test_database().await;
        let root = EpisodeRoot::task("task-fts-lifecycle").expect("valid root");
        let oid = revision_oid(b"fts-lifecycle");
        seed_revision(&database, &root, oid).await;

        let transaction = MemoryWriteTransaction::begin(&database)
            .await
            .expect("begin insert");
        let rowid = upsert_document(&transaction, &document(root.clone(), oid, "oldtoken", None))
            .await
            .expect("insert search document");
        transaction.commit().await.expect("commit insert");
        assert_eq!(match_count(&database, "oldtoken").await, 1);

        let ended_at = "2026-08-24T01:02:03Z"
            .parse::<DateTime<Utc>>()
            .expect("timestamp");
        let transaction = MemoryWriteTransaction::begin(&database)
            .await
            .expect("begin update");
        let updated_rowid = upsert_document(
            &transaction,
            &document(root.clone(), oid, "newtoken", Some(ended_at)),
        )
        .await
        .expect("update search document");
        transaction.commit().await.expect("commit update");
        assert_eq!(updated_rowid, rowid, "an update must preserve FTS rowid");
        assert_eq!(match_count(&database, "oldtoken").await, 0);
        assert_eq!(match_count(&database, "newtoken").await, 1);

        let stored_ended_at: Option<String> = database
            .query_one_raw(Statement::from_string(
                database.get_database_backend(),
                "SELECT ended_at FROM memory_episode_search_doc".to_string(),
            ))
            .await
            .expect("read ended_at")
            .expect("search row")
            .try_get("", "ended_at")
            .expect("nullable ended_at");
        assert!(stored_ended_at.is_some());
        assert_integrity(&database).await;

        let parent_delete = database
            .execute_raw(Statement::from_sql_and_values(
                database.get_database_backend(),
                "DELETE FROM memory_revision_index WHERE revision_oid = ?",
                [oid.to_string().into()],
            ))
            .await;
        assert!(
            parent_delete.is_err(),
            "the content FK must restrict a parent delete that would orphan FTS postings"
        );

        let transaction = MemoryWriteTransaction::begin(&database)
            .await
            .expect("begin delete");
        assert!(
            delete_document(&transaction, root.note_id(), oid)
                .await
                .expect("delete search document")
        );
        transaction.commit().await.expect("commit delete");
        assert_eq!(match_count(&database, "newtoken").await, 0);
        assert_integrity(&database).await;
    }

    #[tokio::test]
    async fn failed_content_update_rolls_back_the_prior_posting_delete() {
        let database = test_database().await;
        let root = EpisodeRoot::task("task-fts-rollback").expect("valid root");
        let oid = revision_oid(b"fts-rollback");
        seed_revision(&database, &root, oid).await;

        let transaction = MemoryWriteTransaction::begin(&database)
            .await
            .expect("begin insert");
        upsert_document(
            &transaction,
            &document(root.clone(), oid, "stabletoken", None),
        )
        .await
        .expect("insert search document");
        transaction.commit().await.expect("commit insert");

        database
            .execute_unprepared(
                "CREATE TRIGGER memory_fts_test_abort
                 BEFORE UPDATE ON memory_episode_search_doc
                 BEGIN SELECT RAISE(ABORT, 'forced test failure'); END",
            )
            .await
            .expect("install deterministic failure");
        let transaction = MemoryWriteTransaction::begin(&database)
            .await
            .expect("begin failing update");
        let error = upsert_document(&transaction, &document(root, oid, "losttoken", None))
            .await
            .expect_err("content update must fail after deleting the old posting");
        assert!(matches!(error, MemoryFtsError::Storage(_)));
        transaction
            .rollback()
            .await
            .expect("roll back failed update");

        assert_eq!(match_count(&database, "stabletoken").await, 1);
        assert_eq!(match_count(&database, "losttoken").await, 0);
        assert_integrity(&database).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn write_locked_transaction_waits_for_a_competing_writer() {
        let directory = tempfile::tempdir().expect("temporary repository database directory");
        let path = directory.path().join("memory-fts-write-lock.db");
        let path = path.to_str().expect("UTF-8 test database path");
        let holder = db::create_database(path)
            .await
            .expect("create repository database");
        let waiter = db::establish_connection(path)
            .await
            .expect("open an independent connection");
        let root = EpisodeRoot::task("task-fts-write-lock").expect("valid root");
        let oid = revision_oid(b"fts-write-lock");
        seed_revision(&holder, &root, oid).await;

        let held = db::begin_write_transaction(&holder)
            .await
            .expect("holder acquires SQLite write lock");
        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            held.commit().await.expect("release competing writer");
        });

        let started = std::time::Instant::now();
        let transaction = MemoryWriteTransaction::begin(&waiter)
            .await
            .expect("Memory writer waits for the lock instead of upgrading after a read");
        upsert_document(&transaction, &document(root, oid, "contendedtoken", None))
            .await
            .expect("write after acquiring the lock");
        transaction.commit().await.expect("commit Memory write");
        release.await.expect("join competing writer");

        assert!(
            started.elapsed() >= std::time::Duration::from_millis(250),
            "the Memory transaction must wait for the holder before reading"
        );
        assert_eq!(match_count(&waiter, "contendedtoken").await, 1);
        assert_integrity(&waiter).await;
    }

    #[tokio::test]
    async fn rebuild_restores_external_content_postings() {
        let database = test_database().await;
        let root = EpisodeRoot::intent("intent-fts-rebuild").expect("valid root");
        let oid = revision_oid(b"fts-rebuild");
        seed_revision(&database, &root, oid).await;

        let transaction = MemoryWriteTransaction::begin(&database)
            .await
            .expect("begin insert");
        upsert_document(&transaction, &document(root, oid, "rebuildtoken", None))
            .await
            .expect("insert search document");
        transaction.commit().await.expect("commit insert");

        database
            .execute_unprepared(
                "INSERT INTO memory_episode_fts(memory_episode_fts) VALUES('delete-all')",
            )
            .await
            .expect("erase postings without changing content");
        assert_eq!(match_count(&database, "rebuildtoken").await, 0);

        let transaction = MemoryWriteTransaction::begin(&database)
            .await
            .expect("begin rebuild");
        rebuild_index(&transaction).await.expect("rebuild FTS");
        transaction.commit().await.expect("commit rebuild");
        assert_eq!(match_count(&database, "rebuildtoken").await, 1);
        assert_integrity(&database).await;
    }
}
