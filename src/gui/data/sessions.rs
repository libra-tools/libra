//! Sessions projection for the window: real captured sessions from the
//! read-only capture catalog.
//!
//! Facts come from the shared capture readers — the catalog rows for scope,
//! state, timestamps and commit links, and `SessionSummaryRead` for the
//! checkpoint metadata (model, title, usage). Nothing here writes: the
//! repository database is opened without schema management and the capture
//! catalog is opened read-only.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(feature = "storage-sqlite")]
use sea_orm::ConnectionTrait;
#[cfg(any(feature = "storage-sqlite", test))]
use sea_orm::TransactionTrait;

use super::{
    snapshot::DataError,
    state::{self, SourceState},
};
#[cfg(feature = "storage-redb")]
use crate::storage::{CaptureStore, RedbCaptureStore, RedbConfigStore, RedbGitStateStore};
use crate::{internal::ai::capture::summary::SummaryErrorKind, storage::StorageBackendKind};

/// One captured agent session row shown by the Sessions surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub agent: String,
    pub state: String,
    pub started_at: i64,
    pub last_event_at: i64,
    /// When the session actually stopped; absent for active sessions.
    pub stopped_at: Option<i64>,
    /// Working directory label (folder name, never an absolute path).
    pub workdir_label: String,
    pub model: Option<String>,
    pub title: Option<String>,
    /// Redacted summary excerpt.
    pub summary: String,
    pub usage_input: Option<i64>,
    pub usage_output: Option<i64>,
    pub usage_calls: Option<i64>,
    pub checkpoints: i64,
    /// Real `agent_session_commit` links (short OIDs).
    pub commits: Vec<String>,
    /// The capture summary status (`available`, `pending`, `unknown`, …).
    pub summary_status: String,
    /// Whether readable content exists for this session.
    pub readable: bool,
}

/// One Sessions read: rows plus the typed source state the window renders.
#[derive(Debug)]
pub struct SessionsRead {
    pub state: SourceState,
    pub rows: Vec<SessionRow>,
}

/// Read up to 200 captured sessions for this repository scope, newest first.
pub async fn read_sessions(
    scope: &crate::internal::worktree_scope::RequestScope,
) -> Result<SessionsRead, DataError> {
    let backend =
        crate::storage::runtime::detect_repo_backend(&scope.storage).map_err(|error| {
            super::context::map_storage_error(
                &error,
                "the repository backend could not be detected",
            )
        })?;
    match backend {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                read_sessions_redb(scope).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err(DataError::new(
                    SourceState::Corrupt,
                    "this Libra binary was compiled without `storage-redb`",
                ))
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let capture_path =
                    crate::internal::ai::capture::store::capture_database_path(&scope.storage);
                match state::probe_path(&capture_path) {
                    state::ProbeOutcome::Absent => {
                        return Ok(SessionsRead {
                            state: SourceState::Absent,
                            rows: Vec::new(),
                        });
                    }
                    state::ProbeOutcome::Unreadable { kind } => {
                        return Err(DataError::new(
                            SourceState::Unreadable { kind },
                            "the capture catalog cannot be opened for reading",
                        ));
                    }
                    state::ProbeOutcome::Present => {}
                }
                let capture_conn = crate::internal::ai::capture::store::open_readonly(
                    &scope.storage,
                    super::repo::READ_BUSY_TIMEOUT,
                )
                .await
                .map_err(|error| {
                    DataError::new(
                        state::classify_open_error(&error),
                        "the capture catalog is unusable",
                    )
                })?;
                // Schema compatibility is checked before any query: an older catalog needs
                // an upgrade and a newer one needs a newer binary, neither is corruption.
                let schema_row = capture_conn
                    .query_one_raw(sea_orm::Statement::from_string(
                        capture_conn.get_database_backend(),
                        "SELECT MAX(version) AS version FROM schema_versions".to_string(),
                    ))
                    .await
                    .map_err(|error| {
                        DataError::new(
                            state::classify_store_message(&error.to_string()),
                            "the capture schema ledger could not be read",
                        )
                    })?;
                let schema_version = schema_row
                    .as_ref()
                    .and_then(|row| row.try_get::<i64>("", "version").ok());
                match schema_version {
                    Some(version)
                        if version < crate::internal::db::schema::CAPTURE_SCHEMA_VERSION =>
                    {
                        return Err(DataError::new(
                            SourceState::SchemaOld,
                            "the capture catalog schema is older than this binary",
                        ));
                    }
                    Some(version)
                        if version > crate::internal::db::schema::CAPTURE_SCHEMA_VERSION =>
                    {
                        return Err(DataError::new(
                            SourceState::SchemaNew,
                            "the capture catalog schema is newer than this binary",
                        ));
                    }
                    Some(_) => {}
                    None => {
                        return Err(DataError::new(
                            SourceState::Corrupt,
                            "the capture catalog has no schema version",
                        ));
                    }
                }
                let repo_db = scope.storage.join(crate::utils::util::DATABASE);
                let repo_db_str = repo_db.to_str().ok_or_else(|| {
                    DataError::new(
                        SourceState::Corrupt,
                        "the repository database path is not UTF-8",
                    )
                })?;
                let repo_conn = crate::internal::db::open_connection_without_schema_management(
                    repo_db_str,
                    super::repo::READ_BUSY_TIMEOUT,
                )
                .await
                .map_err(|error| {
                    DataError::new(
                        state::classify_open_error(&error),
                        "the repository database is unusable",
                    )
                })?;
                let capture_scope = crate::internal::ai::capture_scope::CaptureScope::resolve(
                    &repo_conn,
                    &scope.worktree_root,
                )
                .await
                .map_err(|error| {
                    DataError::new(
                        state::classify_error_chain(&error),
                        "capture scope could not be resolved",
                    )
                })?;
                let predicate =
                    crate::internal::ai::capture_scope::CaptureScope::matches_existing_sql("s");
                let sql = format!(
                    "{} \
                       AND {predicate} \
                       AND NOT EXISTS (SELECT 1 FROM agent_import_tombstone t \
                                        WHERE t.agent_kind = s.agent_kind \
                                          AND t.provider_session_id = s.provider_session_id) \
                      ORDER BY s.started_at DESC, s.session_id ASC \
                      LIMIT {SESSION_ROW_CAP}",
                    crate::command::agent::SESSION_PAGE_COLUMNS
                );
                let rows = capture_conn
                    .query_all_raw(sea_orm::Statement::from_sql_and_values(
                        capture_conn.get_database_backend(),
                        sql,
                        capture_scope.sql_values(),
                    ))
                    .await
                    .map_err(|error| {
                        DataError::new(
                            state::classify_store_message(&error.to_string()),
                            "captured sessions could not be read",
                        )
                    })?;
                let ids: Vec<String> = rows
                    .iter()
                    .map(|row| row.try_get::<String>("", "session_id").unwrap_or_default())
                    .collect();
                let agents: Vec<String> = rows
                    .iter()
                    .map(|row| row.try_get::<String>("", "agent_kind").unwrap_or_default())
                    .collect();
                let links = read_commit_links(&capture_conn, &ids).await?;
                let summaries =
                    read_summaries(&capture_conn, &scope.storage, &ids, &agents).await?;

                let redactor =
                    crate::internal::ai::observed_agents::redaction::Redactor::new_default();
                let mut sessions = Vec::with_capacity(rows.len());
                for (index, row) in rows.iter().enumerate() {
                    let id = ids.get(index).cloned().unwrap_or_default();
                    let checkpoints = count_rows(&capture_conn, "agent_checkpoint", &id).await?;
                    let commits = links.get(&id).cloned().unwrap_or_default();
                    sessions.push(build_session_row(
                        &redactor,
                        RawSessionFields {
                            id: id.clone(),
                            agent_kind: row.try_get::<String>("", "agent_kind").unwrap_or_default(),
                            state: row.try_get("", "state").unwrap_or_default(),
                            started_at: row.try_get("", "started_at").unwrap_or(0),
                            last_event_at: row.try_get("", "last_event_at").unwrap_or(0),
                            stopped_at: row.try_get::<Option<i64>>("", "stopped_at").ok().flatten(),
                            working_dir: row
                                .try_get::<String>("", "working_dir")
                                .unwrap_or_default(),
                            checkpoints,
                            commits,
                        },
                        summaries.get(&id),
                    ));
                }
                capture_conn.close().await.ok();
                repo_conn.close().await.ok();
                Ok(SessionsRead {
                    state: SourceState::Available,
                    rows: sessions,
                })
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err(DataError::new(
                    SourceState::Corrupt,
                    "this Libra binary was compiled without `storage-sqlite`",
                ))
            }
        }
    }
}

struct RawSessionFields {
    id: String,
    agent_kind: String,
    state: String,
    started_at: i64,
    last_event_at: i64,
    stopped_at: Option<i64>,
    working_dir: String,
    checkpoints: i64,
    commits: Vec<String>,
}

fn build_session_row(
    redactor: &crate::internal::ai::observed_agents::redaction::Redactor,
    raw: RawSessionFields,
    summary_result: Option<&SummaryResult>,
) -> SessionRow {
    let summary_view = summary_result.and_then(|result| result.as_ref().ok());
    let summary_text = summary_view
        .and_then(|view| view.title.clone())
        .unwrap_or_default();
    let summary = redact(redactor, &summary_text)
        .chars()
        .take(SUMMARY_CHAR_CAP)
        .collect::<String>();
    let title = summary_view
        .and_then(|view| view.title.clone())
        .map(|title| redact(redactor, &title));
    let model = summary_view.and_then(|view| view.model.clone());
    let usage = summary_view.and_then(|view| view.token_usage.as_ref());
    let summary_status = match summary_result {
        Some(Ok(view)) => view.status.to_string(),
        Some(Err(SummaryErrorKind::Budget)) => "unavailable".to_string(),
        Some(Err(SummaryErrorKind::Transient)) => "unavailable".to_string(),
        Some(Err(SummaryErrorKind::Corrupt)) => "malformed".to_string(),
        Some(Err(SummaryErrorKind::Catalog)) => "unavailable".to_string(),
        None => "unknown".to_string(),
    };
    let has_content = summary_view.is_some_and(|view| {
        view.title.is_some()
            || view.prompt_count.is_some_and(|count| count > 0)
            || view.modified_file_count.is_some_and(|count| count > 0)
    });
    SessionRow {
        id: raw.id,
        agent: agent_label(&raw.agent_kind),
        state: raw.state,
        started_at: raw.started_at,
        last_event_at: raw.last_event_at,
        stopped_at: raw.stopped_at,
        workdir_label: folder_label(&raw.working_dir),
        model,
        title,
        summary,
        usage_input: usage.map(|usage| usage.input_tokens as i64),
        usage_output: usage.map(|usage| usage.output_tokens as i64),
        usage_calls: summary_view
            .and_then(|view| view.prompt_count)
            .map(|count| count as i64),
        checkpoints: raw.checkpoints,
        commits: raw.commits,
        summary_status,
        readable: has_content,
    }
}

#[cfg(feature = "storage-redb")]
async fn read_sessions_redb(
    scope: &crate::internal::worktree_scope::RequestScope,
) -> Result<SessionsRead, DataError> {
    let capture_path = match crate::storage::runtime::resolve_repo_capture_redb_path(&scope.storage)
    {
        Ok(path) => path,
        Err(crate::storage::StorageBackendError::NotFound { .. }) => {
            return Ok(SessionsRead {
                state: SourceState::Absent,
                rows: Vec::new(),
            });
        }
        Err(error) => {
            return Err(super::context::map_storage_error(
                &error,
                "the capture catalog path could not be resolved",
            ));
        }
    };
    match state::probe_path(&capture_path) {
        state::ProbeOutcome::Absent => {
            return Ok(SessionsRead {
                state: SourceState::Absent,
                rows: Vec::new(),
            });
        }
        state::ProbeOutcome::Unreadable { kind } => {
            return Err(DataError::new(
                SourceState::Unreadable { kind },
                "the capture catalog cannot be opened for reading",
            ));
        }
        state::ProbeOutcome::Present => {}
    }

    let meta_path = capture_path.clone();
    let meta_state = tokio::task::spawn_blocking(move || {
        super::context::inspect_redb_meta_schema(
            &meta_path,
            crate::internal::db::schema::DatabaseRole::Capture,
        )
    })
    .await
    .map_err(|_| {
        DataError::new(
            SourceState::Corrupt,
            "the capture catalog metadata schema inspection task failed",
        )
    })??;
    if !meta_state.is_available() {
        let message = match meta_state {
            SourceState::SchemaNew => "the capture catalog schema is newer than this binary",
            SourceState::SchemaOld => "the capture catalog schema is older than this binary",
            _ => "the capture catalog schema is not compatible",
        };
        return Err(DataError::new(meta_state, message));
    }

    let capture_store = RedbCaptureStore::open_read_only(&capture_path)
        .await
        .map_err(|error| {
            super::context::map_storage_error(&error, "the capture catalog is unusable")
        })?;

    let schema_versions = capture_store
        .list_schema_versions()
        .await
        .map_err(|error| {
            super::context::map_storage_error(&error, "the capture schema ledger could not be read")
        })?;
    match schema_versions.iter().map(|record| record.version).max() {
        Some(version) if version < crate::internal::db::schema::CAPTURE_SCHEMA_VERSION => {
            return Err(DataError::new(
                SourceState::SchemaOld,
                "the capture catalog schema is older than this binary",
            ));
        }
        Some(version) if version > crate::internal::db::schema::CAPTURE_SCHEMA_VERSION => {
            return Err(DataError::new(
                SourceState::SchemaNew,
                "the capture catalog schema is newer than this binary",
            ));
        }
        Some(_) => {}
        None => {
            return Err(DataError::new(
                SourceState::Corrupt,
                "the capture catalog has no schema version",
            ));
        }
    }

    let git_db_path =
        crate::storage::runtime::resolve_repo_git_redb_path(&scope.storage).map_err(|error| {
            super::context::map_storage_error(&error, "the repository database is unusable")
        })?;
    let cfg_path = git_db_path.clone();
    let config_store = tokio::task::spawn_blocking(move || {
        RedbConfigStore::open_read_only(
            &cfg_path,
            crate::internal::db::schema::DatabaseRole::Repository,
        )
    })
    .await
    .map_err(|_| {
        DataError::new(
            SourceState::Corrupt,
            "the repository configuration store open task failed",
        )
    })?
    .map_err(|error| {
        super::context::map_storage_error(&error, "the repository database is unusable")
    })?;

    let state_path = git_db_path;
    let state_store =
        tokio::task::spawn_blocking(move || RedbGitStateStore::open_read_only(&state_path))
            .await
            .map_err(|_| {
                DataError::new(
                    SourceState::Corrupt,
                    "the repository state store open task failed",
                )
            })?
            .map_err(|error| {
                super::context::map_storage_error(&error, "the repository database is unusable")
            })?;

    let capture_scope = crate::internal::ai::capture_scope::CaptureScope::resolve_with_stores(
        &config_store,
        &state_store,
        &scope.worktree_root,
    )
    .await
    .map_err(|error| {
        DataError::new(
            state::classify_error_chain(&error),
            "capture scope could not be resolved",
        )
    })?;
    drop(state_store);

    let tombstones = capture_store
        .list_import_tombstones()
        .await
        .map_err(|error| {
            super::context::map_storage_error(&error, "captured sessions could not be read")
        })?;
    let tombstoned: std::collections::HashSet<(String, String)> = tombstones
        .into_iter()
        .map(|record| (record.agent_kind, record.provider_session_id))
        .collect();

    let mut matched: Vec<crate::storage::AgentSessionRecord> = capture_store
        .list_sessions_by_started_desc()
        .await
        .map_err(|error| {
            super::context::map_storage_error(&error, "captured sessions could not be read")
        })?
        .into_iter()
        .filter(|session| {
            capture_scope.matches_scope_fields(
                session.scope_state,
                session.repo_id.as_deref(),
                session.worktree_id.as_deref(),
                session.workspace_id.as_deref(),
                session.workspace_fence,
            ) && !tombstoned.contains(&(
                session.agent_kind.as_str().to_string(),
                session.provider_session_id.clone(),
            ))
        })
        .collect();
    matched.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    matched.truncate(SESSION_ROW_CAP);

    let ids: Vec<String> = matched.iter().map(|row| row.session_id.clone()).collect();
    let agents: Vec<String> = matched
        .iter()
        .map(|row| row.agent_kind.as_str().to_string())
        .collect();

    let mut links: HashMap<String, Vec<String>> = HashMap::new();
    let mut total_links = 0usize;
    for id in &ids {
        if total_links >= COMMIT_LINK_QUERY_CAP {
            break;
        }
        let session_commits = capture_store
            .list_session_commits(id)
            .await
            .map_err(|error| {
                super::context::map_storage_error(
                    &error,
                    "captured session commit links could not be read",
                )
            })?;
        let mut short_oids = Vec::new();
        for link in session_commits {
            if short_oids.len() >= COMMIT_LINK_CAP || total_links >= COMMIT_LINK_QUERY_CAP {
                break;
            }
            if !link.commit_oid.is_empty() {
                short_oids.push(short_oid(&link.commit_oid));
                total_links += 1;
            }
        }
        if !short_oids.is_empty() {
            links.insert(id.clone(), short_oids);
        }
    }

    let summaries =
        read_summaries_with_capture(&capture_store, &config_store, &scope.storage, &ids, &agents)
            .await?;

    let redactor = crate::internal::ai::observed_agents::redaction::Redactor::new_default();
    let mut sessions = Vec::with_capacity(matched.len());
    for row in matched {
        let checkpoints = capture_store
            .list_checkpoints_by_session(&row.session_id)
            .await
            .map_err(|error| {
                super::context::map_storage_error(
                    &error,
                    "captured session counts could not be read",
                )
            })?;
        let checkpoint_count = i64::try_from(checkpoints.len()).unwrap_or(i64::MAX);
        let commits = links.get(&row.session_id).cloned().unwrap_or_default();
        let summary_result = summaries.get(&row.session_id);
        sessions.push(build_session_row(
            &redactor,
            RawSessionFields {
                id: row.session_id,
                agent_kind: row.agent_kind.as_str().to_string(),
                state: row.state.as_str().to_string(),
                started_at: row.started_at,
                last_event_at: row.last_event_at,
                stopped_at: row.stopped_at,
                working_dir: row.working_dir,
                checkpoints: checkpoint_count,
                commits,
            },
            summary_result,
        ));
    }
    Ok(SessionsRead {
        state: SourceState::Available,
        rows: sessions,
    })
}

#[cfg(feature = "storage-redb")]
async fn read_summaries_with_capture(
    capture: &dyn CaptureStore,
    config: &dyn crate::storage::ConfigStore,
    storage: &std::path::Path,
    ids: &[String],
    agents: &[String],
) -> Result<HashMap<String, SummaryResult>, DataError> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let budget = crate::internal::ai::checkpoint_reader::CheckpointReadBudget::new(
        Instant::now() + SUMMARY_BUDGET,
        Arc::new(|| false),
    );
    let mut prepared = Vec::with_capacity(ids.len());
    for (id, agent) in ids.iter().zip(agents) {
        let read =
            crate::internal::ai::capture::summary::SessionSummaryRead::prepare_for_with_capture(
                capture,
                config,
                storage,
                id,
                agent,
                budget.clone(),
            )
            .await;
        match read {
            Ok(read) => prepared.push((id.clone(), Ok(read))),
            Err(error) => {
                let kind = classify_summary_error(&error);
                if kind == SummaryErrorKind::Catalog {
                    if let Some(storage_err) =
                        error.downcast_ref::<crate::storage::StorageBackendError>()
                    {
                        return Err(super::context::map_storage_error(
                            storage_err,
                            "session summaries could not be read",
                        ));
                    }
                    return Err(DataError::new(
                        state::classify_error_chain(&error),
                        "session summaries could not be read",
                    ));
                }
                prepared.push((id.clone(), Err(kind)));
            }
        }
    }
    for (id, read) in prepared {
        let result = match read {
            Ok(read) => read.finish_result().await,
            Err(kind) => Err(kind),
        };
        out.insert(id, result);
    }
    Ok(out)
}

#[cfg(feature = "storage-sqlite")]
async fn read_commit_links(
    conn: &sea_orm::DatabaseConnection,
    ids: &[String],
) -> Result<HashMap<String, Vec<String>>, DataError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT session_id, commit_oid FROM (\
           SELECT session_id, commit_oid, \
                  ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY rowid) AS rn \
             FROM agent_session_commit WHERE session_id IN ({placeholders})\
         ) WHERE rn <= {COMMIT_LINK_CAP} ORDER BY session_id, rn LIMIT {COMMIT_LINK_QUERY_CAP}"
    );
    let rows = conn
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            conn.get_database_backend(),
            sql,
            ids.iter().map(|id| id.clone().into()).collect::<Vec<_>>(),
        ))
        .await
        .map_err(|error| {
            DataError::new(
                state::classify_store_message(&error.to_string()),
                "captured session commit links could not be read",
            )
        })?;
    let wanted: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut links: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let session_id: String = row.try_get("", "session_id").unwrap_or_default();
        if !wanted.contains(session_id.as_str()) {
            continue;
        }
        let oid: String = row.try_get("", "commit_oid").unwrap_or_default();
        let entry = links.entry(session_id).or_default();
        if entry.len() < COMMIT_LINK_CAP && !oid.is_empty() {
            entry.push(short_oid(&oid));
        }
    }
    Ok(links)
}

/// Read checkpoint metadata through the shared summary reader. A shared budget
/// bounds the enrichment; sessions without a readable checkpoint stay
/// `unknown` rather than inventing facts.
type SummaryResult =
    Result<crate::internal::ai::capture::summary::SessionSummaryView, SummaryErrorKind>;

#[cfg(any(feature = "storage-sqlite", test))]
pub(crate) async fn read_summaries(
    conn: &sea_orm::DatabaseConnection,
    storage: &std::path::Path,
    ids: &[String],
    agents: &[String],
) -> Result<HashMap<String, SummaryResult>, DataError> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let budget = crate::internal::ai::checkpoint_reader::CheckpointReadBudget::new(
        Instant::now() + SUMMARY_BUDGET,
        Arc::new(|| false),
    );
    let txn = conn.begin().await.map_err(|error| {
        DataError::new(
            state::classify_store_message(&error.to_string()),
            "session summaries could not be read",
        )
    })?;
    let mut prepared = Vec::with_capacity(ids.len());
    for (id, agent) in ids.iter().zip(agents) {
        let read = crate::internal::ai::capture::summary::SessionSummaryRead::prepare_for(
            &txn,
            storage,
            id,
            agent,
            budget.clone(),
        )
        .await;
        match read {
            Ok(read) => prepared.push((id.clone(), Ok(read))),
            Err(error) => {
                let kind = classify_summary_error(&error);
                // A catalog-level failure is the source's problem, not one
                // row's: fail the page instead of degrading every row.
                if kind == SummaryErrorKind::Catalog {
                    return Err(DataError::new(
                        state::classify_error_chain(&error),
                        "session summaries could not be read",
                    ));
                }
                prepared.push((id.clone(), Err(kind)));
            }
        }
    }
    let _ = txn.rollback().await;
    for (id, read) in prepared {
        let result = match read {
            Ok(read) => read.finish_result().await,
            Err(kind) => Err(kind),
        };
        out.insert(id, result);
    }
    Ok(out)
}

/// Classify a preparation failure by its typed cause, not by text.
fn classify_summary_error(error: &anyhow::Error) -> SummaryErrorKind {
    if error
        .downcast_ref::<crate::storage::StorageBackendError>()
        .is_some()
    {
        return SummaryErrorKind::Catalog;
    }
    if error
        .downcast_ref::<crate::internal::ai::capture::catalog::CaptureCatalogError>()
        .is_some_and(|catalog| {
            matches!(
                catalog,
                crate::internal::ai::capture::catalog::CaptureCatalogError::Database
                    | crate::internal::ai::capture::catalog::CaptureCatalogError::DatabaseBusy
            )
        })
    {
        return SummaryErrorKind::Catalog;
    }
    if let Some(reader) =
        error.downcast_ref::<crate::internal::ai::checkpoint_reader::CheckpointReaderError>()
    {
        return match reader {
            crate::internal::ai::checkpoint_reader::CheckpointReaderError::Budget => {
                SummaryErrorKind::Budget
            }
            crate::internal::ai::checkpoint_reader::CheckpointReaderError::CatalogUnavailable
            | crate::internal::ai::checkpoint_reader::CheckpointReaderError::CatalogBusy => {
                SummaryErrorKind::Catalog
            }
            _ => SummaryErrorKind::Corrupt,
        };
    }
    let message = error.to_string();
    if message.contains("budget") || message.contains("deadline") {
        SummaryErrorKind::Budget
    } else if message.contains("database") || message.contains("busy") {
        SummaryErrorKind::Transient
    } else {
        SummaryErrorKind::Corrupt
    }
}

#[cfg(feature = "storage-sqlite")]
async fn count_rows(
    conn: &sea_orm::DatabaseConnection,
    table: &str,
    session_id: &str,
) -> Result<i64, DataError> {
    let sql = format!("SELECT COUNT(*) AS count FROM {table} WHERE session_id = ?");
    let row = conn
        .query_one_raw(sea_orm::Statement::from_sql_and_values(
            conn.get_database_backend(),
            sql,
            [session_id.into()],
        ))
        .await
        .map_err(|error| {
            DataError::new(
                state::classify_store_message(&error.to_string()),
                "captured session counts could not be read",
            )
        })?;
    Ok(row
        .map(|row| row.try_get::<i64>("", "count").unwrap_or(0))
        .unwrap_or(0))
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(8).collect()
}

fn folder_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string())
}

/// Redact secrets from captured text before it reaches the window.
pub fn redact(
    redactor: &crate::internal::ai::observed_agents::redaction::Redactor,
    text: &str,
) -> String {
    let (bytes, _report) = redactor.redact(text.as_bytes());
    String::from_utf8_lossy(bytes.bytes()).into_owned()
}

/// Format an epoch-second timestamp as UTC `HH:MM` (test seam).
pub fn clock_from_epoch(seconds: i64) -> String {
    if seconds <= 0 {
        return "—".to_string();
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
        .map(|value| value.format("%H:%M").to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// Display label for a capture agent kind (the UI filter chips use these).
pub fn agent_label(kind: &str) -> String {
    match kind {
        "claude_code" => "Claude Code".to_string(),
        "codex" => "Codex".to_string(),
        "opencode" => "OpenCode".to_string(),
        "grok" => "Grok".to_string(),
        "hermes" => "Hermes".to_string(),
        "pi" => "Pi".to_string(),
        other => other.to_string(),
    }
}

/// Sessions reads are capped at 200 rows.
pub const SESSION_ROW_CAP: usize = 200;
/// Commit links shown per session are capped.
pub(crate) const COMMIT_LINK_CAP: usize = 20;
/// Summaries are display excerpts, not payloads.
const SUMMARY_CHAR_CAP: usize = 512;
/// Shared enrichment budget for checkpoint metadata reads.
const SUMMARY_BUDGET: Duration = Duration::from_secs(3);
/// Total commit-link rows read for one page.
const COMMIT_LINK_QUERY_CAP: usize = 4000;
