//! Operations projection for the window: real Operation v2 rows through the
//! shared snapshot reader. Read-only: no operation rows are written.

use super::{
    repo,
    snapshot::DataError,
    state::{self, SourceState},
};

/// One Operation v2 row shown by the Operations surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRow {
    pub op_id: String,
    pub command_name: String,
    /// Redacted summary excerpt (≤200 chars, never an absolute path).
    pub description: String,
    pub actor: String,
    pub status: String,
    pub start_ts: i64,
    pub end_ts: Option<i64>,
    pub view_id: String,
}

/// One Operations read: the id snapshot rows plus typed source state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationsRead {
    pub state: SourceState,
    pub rows: Vec<OperationRow>,
    pub truncated: bool,
}

/// Rows per displayed page (the plan's 200-row cap).
pub const OPERATIONS_PAGE_SIZE: usize = 200;

/// Read the whole operation id snapshot for this repository, newest first.
pub async fn read_operations(
    scope: &crate::internal::worktree_scope::RequestScope,
) -> Result<OperationsRead, DataError> {
    let db_path = scope.storage.join(crate::utils::util::DATABASE);
    match state::probe_path(&db_path) {
        state::ProbeOutcome::Absent => {
            return Ok(OperationsRead {
                state: SourceState::Absent,
                rows: Vec::new(),
                truncated: false,
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
    // Validate schema compatibility on THIS connection, before reading rows.
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
    let (entries, truncated) = crate::command::op::operation_snapshot_rows(&conn, &repo_id)
        .await
        .map_err(|error| {
            DataError::new(
                state::classify_store_message(&error.to_string()),
                "operation history could not be read",
            )
        })?;
    conn.close().await.ok();
    let redactor = crate::internal::ai::observed_agents::redaction::Redactor::new_default();
    let rows: Vec<OperationRow> = entries
        .into_iter()
        .map(|entry| OperationRow {
            op_id: entry.op_id,
            command_name: entry.command_name,
            description: sanitize_paths(
                &redact(&redactor, &entry.description),
                scope.worktree_root.as_path(),
                scope.storage.as_path(),
            )
            .chars()
            .take(SUMMARY_CHAR_CAP)
            .collect(),
            actor: entry.actor,
            status: entry.status,
            start_ts: entry.start_ts,
            end_ts: entry.end_ts,
            view_id: entry.view_id,
        })
        .collect();
    let state = if rows.is_empty() {
        SourceState::Absent
    } else {
        SourceState::Available
    };
    Ok(OperationsRead {
        state,
        rows,
        truncated,
    })
}

/// Byte/row bounds for one page of a fixed snapshot.
pub fn page_range(len: usize, page: usize) -> (usize, usize) {
    let start = page.saturating_mul(OPERATIONS_PAGE_SIZE).min(len);
    let end = (start + OPERATIONS_PAGE_SIZE).min(len);
    (start, end)
}

/// Slice one page out of an id snapshot. Pages are stable: the snapshot is
/// fixed, so no row repeats or disappears between pages.
pub fn page_slice(rows: &[OperationRow], page: usize) -> &[OperationRow] {
    let (start, end) = page_range(rows.len(), page);
    &rows[start..end]
}

/// A filter predicate over real rows (command and free-text search).
pub fn matches_filter(row: &OperationRow, command: &str, query: &str) -> bool {
    let command = command.trim().to_lowercase();
    let query = query.trim().to_lowercase();
    (command.is_empty() || row.command_name.to_lowercase() == command)
        && (query.is_empty()
            || row.command_name.to_lowercase().contains(&query)
            || row.description.to_lowercase().contains(&query)
            || row.op_id.to_lowercase().contains(&query))
}

/// Replace repository roots and any remaining absolute path with a repo-relative
/// label so no host path reaches the window.
fn sanitize_paths(
    text: &str,
    worktree_root: &std::path::Path,
    storage: &std::path::Path,
) -> String {
    let mut out = text.to_string();
    for root in [worktree_root, storage] {
        if let Some(root) = root.to_str()
            && !root.is_empty()
        {
            out = out.replace(root, "<repo>");
        }
    }
    out.split_whitespace()
        .map(sanitize_token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strip absolute paths from a token while leaving relative names
/// (`feat/search`) and ordinary words untouched. Handles quoted/wrapped paths,
/// `key=/abs` prefixes, drive letters, URLs, and comma/semicolon lists.
fn sanitize_token(token: &str) -> String {
    let mut out = String::new();
    for part in token.split_inclusive([',', ';']) {
        let (body, delimiter) = split_delimiter(part);
        out.push_str(&sanitize_path_part(body));
        out.push_str(delimiter);
    }
    out
}

fn split_delimiter(part: &str) -> (&str, &str) {
    let end = part
        .char_indices()
        .rev()
        .find(|(_, c)| *c == ',' || *c == ';')
        .map(|(index, _)| index)
        .unwrap_or(part.len());
    (&part[..end], &part[end..])
}

fn is_absolute_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with('\\')
        || (value.len() > 1
            && value.as_bytes()[0].is_ascii_alphabetic()
            && value.as_bytes()[1] == b':')
}

fn basename(value: &str) -> String {
    value
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

/// Peel wrapping punctuation/quotes so `(/abs)`, `[/abs]`, `` `/abs` `` and
/// `"..."` are all recognized as paths.
fn strip_wrappers(value: &str) -> (&str, &str, &str) {
    let lead_len = value
        .char_indices()
        .take_while(|(_, c)| matches!(c, '(' | '[' | '{' | '<' | '"' | '\'' | '`'))
        .map(|(index, c)| index + c.len_utf8())
        .last()
        .unwrap_or(0);
    let rest = &value[lead_len..];
    let tail_len = rest
        .char_indices()
        .rev()
        .take_while(|(_, c)| matches!(c, ')' | ']' | '}' | '>' | '"' | '\'' | '`'))
        .map(|(index, _)| rest.len() - index)
        .last()
        .unwrap_or(0);
    let body_end = rest.len().saturating_sub(tail_len);
    (&value[..lead_len], &rest[..body_end], &rest[body_end..])
}

fn sanitize_path_part(part: &str) -> String {
    let (lead, body, tail) = strip_wrappers(part);
    // URLs are paths even when the filename contains `=`.
    if body.contains("://") {
        return format!("{lead}{}{tail}", basename(body));
    }
    if is_absolute_path(body) {
        return format!("{lead}<path>{tail}");
    }
    // Try EVERY assignment/colon boundary: `a=b=/abs/file` and
    // `error:/abs/file` must still sanitize.
    for (index, separator) in body
        .char_indices()
        .filter(|(_, c)| *c == '=' || *c == ':' || *c == '(' || *c == '>')
    {
        let key = &body[..index];
        let value = &body[index + separator.len_utf8()..];
        let (value_lead, value_body, value_tail) = strip_wrappers(value);
        if is_absolute_path(value_body) || value_body.contains("://") {
            return format!(
                "{lead}{key}{separator}{value_lead}{}{value_tail}{tail}",
                basename(value_body)
            );
        }
    }
    part.to_string()
}

fn redact(
    redactor: &crate::internal::ai::observed_agents::redaction::Redactor,
    text: &str,
) -> String {
    let (bytes, _report) = redactor.redact(text.as_bytes());
    String::from_utf8_lossy(bytes.bytes()).into_owned()
}

/// The production filter-then-paginate composition used by the renderer.
pub fn filtered_page<'a>(
    rows: &'a [OperationRow],
    command: &str,
    query: &str,
    page: usize,
) -> (Vec<&'a OperationRow>, usize) {
    let filtered: Vec<&OperationRow> = rows
        .iter()
        .filter(|row| matches_filter(row, command, query))
        .collect();
    let (start, end) = page_range(filtered.len(), page);
    let page_count = if filtered.is_empty() {
        1
    } else {
        filtered.len().div_ceil(OPERATIONS_PAGE_SIZE)
    };
    (filtered[start..end].to_vec(), page_count)
}

/// Display summaries are bounded excerpts.
const SUMMARY_CHAR_CAP: usize = 200;
