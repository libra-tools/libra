//! Memory projection for the window: bounded episode rows with stale semantics.
//!
//! Read-only against the projected tables; a stale projection shows zero rows
//! (fail-closed) with a rebuild hint, matching `libra memory`.

use super::{
    repo,
    snapshot::DataError,
    state::{self, SourceState},
};

/// One projected episode shown by the Memory surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRow {
    pub episode_id: String,
    pub source_kind: String,
    pub started_at: i64,
    pub ended_at: i64,
    /// Redacted title excerpt (≤200 chars, no absolute paths).
    pub title: String,
    /// Redacted body excerpt (≤200 chars, no absolute paths).
    pub body: String,
    pub actor: Option<String>,
    pub outcome: String,
    pub evidence_count: usize,
    pub path_count: usize,
}

/// One Memory read: bounded rows plus typed source state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRead {
    pub state: SourceState,
    pub rows: Vec<MemoryRow>,
}

/// The display cap for one Memory page.
pub const MEMORY_ROW_CAP: usize = 200;

/// Read the projection for this repository: stale → zero rows, absent →
/// absent, otherwise a bounded 200-row projection.
pub async fn read_memory(
    scope: &crate::internal::worktree_scope::RequestScope,
) -> Result<MemoryRead, DataError> {
    let db_path = scope.storage.join(crate::utils::util::DATABASE);
    match state::probe_path(&db_path) {
        state::ProbeOutcome::Absent => {
            return Ok(MemoryRead {
                state: SourceState::Absent,
                rows: Vec::new(),
            });
        }
        state::ProbeOutcome::Unreadable { kind } => {
            return Err(DataError::new(
                SourceState::Unreadable { kind },
                "the repository database cannot be opened for reading",
            ));
        }
        state::ProbeOutcome::Present => {}
    }
    let db_str = db_path.to_str().ok_or_else(|| {
        DataError::new(
            SourceState::Corrupt,
            "the repository database path is not UTF-8",
        )
    })?;
    let conn = crate::internal::db::open_connection_without_schema_management(
        db_str,
        repo::READ_BUSY_TIMEOUT,
    )
    .await
    .map_err(|error| {
        DataError::new(
            state::classify_open_error(&error),
            "the repository database is unusable",
        )
    })?;
    let compatibility = crate::internal::db::schema::inspect_schema_for_connection(
        &conn,
        crate::internal::db::schema::DatabaseRole::Repository,
    )
    .await
    .map_err(|error| {
        DataError::new(
            state::classify_store_message(&error.to_string()),
            "the repository schema ledger could not be read",
        )
    })?;
    match state::classify_schema(&compatibility) {
        SourceState::Available => {}
        other => {
            return Err(DataError::new(
                other,
                "the repository schema does not match this binary",
            ));
        }
    }
    let repo_id = crate::internal::config::ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await
        .map_err(|error| {
            DataError::new(
                state::classify_store_message(&error.to_string()),
                "the repository identity could not be read",
            )
        })?
        .map(|entry| entry.value)
        .ok_or_else(|| {
            DataError::new(
                SourceState::Corrupt,
                "the repository has no recorded identity",
            )
        })?;
    let stale = crate::internal::ai::memory::reader::projection_is_stale(&conn, &repo_id)
        .await
        .map_err(|error| {
            DataError::new(
                state::classify_store_message(&error.to_string()),
                "the memory projection state could not be read",
            )
        })?;
    match stale {
        None => {
            conn.close().await.ok();
            return Ok(MemoryRead {
                state: SourceState::Absent,
                rows: Vec::new(),
            });
        }
        Some(true) => {
            conn.close().await.ok();
            return Ok(MemoryRead {
                state: SourceState::Stale,
                rows: Vec::new(),
            });
        }
        Some(false) => {}
    }
    let episodes =
        crate::internal::ai::memory::reader::list_episodes_bounded(&conn, &repo_id, MEMORY_ROW_CAP)
            .await
            .map_err(|error| {
                DataError::new(
                    state::classify_store_message(&error.to_string()),
                    "memory episodes could not be read",
                )
            })?;
    conn.close().await.ok();
    let redactor = crate::internal::ai::observed_agents::redaction::Redactor::new_default();
    let rows: Vec<MemoryRow> = episodes
        .into_iter()
        .map(|episode| MemoryRow {
            episode_id: episode.episode_id,
            source_kind: episode.source_kind.as_str().to_string(),
            started_at: episode.started_at,
            ended_at: episode.ended_at,
            title: sanitize(&redact(&redactor, &episode.title)),
            body: sanitize(&redact(&redactor, &episode.body)),
            actor: episode
                .actor
                .map(|actor| sanitize(&redact(&redactor, &actor))),
            outcome: format!("{:?}", episode.outcome).to_lowercase(),
            evidence_count: episode.evidence.len(),
            path_count: episode.paths.len(),
        })
        .collect();
    let state = if rows.is_empty() {
        SourceState::Absent
    } else {
        SourceState::Available
    };
    Ok(MemoryRead { state, rows })
}

/// Filter predicate over real rows (source kind and free-text search).
pub fn matches_filter(row: &MemoryRow, source: &str, query: &str) -> bool {
    let source = source.trim().to_lowercase();
    let query = query.trim().to_lowercase();
    (source.is_empty() || row.source_kind == source)
        && (query.is_empty()
            || row.title.to_lowercase().contains(&query)
            || row.body.to_lowercase().contains(&query)
            || row.episode_id.to_lowercase().contains(&query))
}

fn redact(
    redactor: &crate::internal::ai::observed_agents::redaction::Redactor,
    text: &str,
) -> String {
    let (bytes, _report) = redactor.redact(text.as_bytes());
    String::from_utf8_lossy(bytes.bytes()).into_owned()
}

/// Bound a display excerpt: no absolute paths (shared sanitizer), ≤200 chars.
fn sanitize(text: &str) -> String {
    super::operations::sanitize_paths(text, std::path::Path::new(""), std::path::Path::new(""))
        .chars()
        .take(200)
        .collect()
}
