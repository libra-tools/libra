//! `libra agent session …` subcommands. V1 surfaces rows from `agent_session`
//! and lets operators mark captured sessions stopped/active again without
//! rewriting provider transcripts. Phase 4.1 follow-up `promote --as-intent` lifts
//! a captured external-agent session into Libra's own `refs/libra/intent`
//! AI history (entire.md §14.4 item 2).

use clap::{Args, Subcommand};
use sea_orm::{ConnectionTrait, Statement};
use serde::Serialize;

use super::checkpoint::{
    PAGE_SCHEMA_VERSION, decode_page_cursor, encode_page_cursor, resolve_page_limit,
};
use crate::{
    internal::{ai::observed_agents::AgentKind, db::get_db_conn_instance},
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
        text,
    },
};

#[derive(Subcommand, Debug)]
pub enum SessionSubcommand {
    /// List captured sessions.
    #[command(about = "List captured sessions")]
    List(SessionListArgs),
    /// Show a single session by id.
    #[command(about = "Show a captured session")]
    Show(SessionShowArgs),
    /// Stop a captured session.
    #[command(about = "Stop a captured session")]
    Stop(SessionStopArgs),
    /// Resume a stopped session.
    #[command(about = "Resume a stopped session")]
    Resume(SessionResumeArgs),
    /// Cross-system promotion: surface a captured session on
    /// `refs/libra/intent` so Libra's own AI tooling sees it.
    #[command(about = "Promote a captured session to libra/intent")]
    Promote(SessionPromoteArgs),
    /// Walk the session's normalized events and emit one
    /// `ToolCallRecord`-shaped JSON entry per pre/post tool use pair.
    /// Phase 4.3 (entire.md §14.4 item 3).
    #[command(about = "Derive ToolCallRecord entries from a captured session")]
    DeriveToolCalls(SessionDeriveToolCallsArgs),
}

#[derive(Args, Debug)]
pub struct SessionListArgs {
    /// Filter by agent kind (slug, e.g. `claude-code`).
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
    /// Filter by state (`active`, `stopped`, …).
    #[arg(long, value_name = "STATE")]
    pub state: Option<String>,
    /// Maximum rows to return (default 50, capped at 500) — AG-20
    /// metadata-first pagination.
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
    /// Keyset cursor from the previous page's `next_cursor` (opaque;
    /// AG-20). Do not construct by hand.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
}

#[derive(Args, Debug)]
pub struct SessionShowArgs {
    /// `agent_session.session_id` of the session to inspect (from `libra agent session list`)
    #[arg(value_name = "SESSION_ID")]
    pub session_id: String,
    /// Materialise the captured transcript at the given path (Phase 2)
    #[arg(long, value_name = "PATH")]
    pub extract_transcript: Option<String>,
}

#[derive(Args, Debug)]
pub struct SessionStopArgs {
    /// `agent_session.session_id` of the session to mark as stopped
    #[arg(value_name = "SESSION_ID")]
    pub session_id: String,
}

#[derive(Args, Debug)]
pub struct SessionResumeArgs {
    /// `agent_session.session_id` of the stopped session to resume
    #[arg(value_name = "SESSION_ID")]
    pub session_id: String,
}

#[derive(Args, Debug)]
pub struct SessionDeriveToolCallsArgs {
    /// `agent_session.session_id` of the captured session whose
    /// SessionStore JSONL we should walk.
    pub session_id: String,
}

#[derive(Args, Debug)]
pub struct SessionPromoteArgs {
    /// `agent_session.session_id` of the captured external-agent session
    /// to promote.
    pub session_id: String,
    /// Mark the new revision as an `Intent` on `refs/libra/intent`.
    /// Currently the only promotion target — kept as an explicit flag so
    /// future targets (e.g. `--as-task`, `--as-plan`) plug in without
    /// reshaping the CLI.
    #[arg(long, default_value_t = true)]
    pub as_intent: bool,
    /// Override the auto-derived prompt text. Defaults to a synthetic
    /// summary that names the source agent_kind + provider_session_id
    /// so the promoted intent is recognisable in the projection log.
    #[arg(long, value_name = "TEXT")]
    pub prompt: Option<String>,
    /// Inspect what would be created without writing to
    /// `refs/libra/intent`. JSON mode emits the full payload that
    /// `--apply` would persist.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn execute_safe(cmd: SessionSubcommand, output: &OutputConfig) -> CliResult<()> {
    match cmd {
        SessionSubcommand::List(args) => list(args, output).await,
        SessionSubcommand::Show(args) => show(args, output).await,
        SessionSubcommand::Stop(args) => stop(args, output).await,
        SessionSubcommand::Resume(args) => resume(args, output).await,
        SessionSubcommand::Promote(args) => promote(args, output).await,
        SessionSubcommand::DeriveToolCalls(args) => derive_tool_calls(args, output).await,
    }
}

async fn derive_tool_calls(
    args: SessionDeriveToolCallsArgs,
    output: &OutputConfig,
) -> CliResult<()> {
    use crate::{
        internal::ai::{observed_agents::derive_tool_call_records, session::SessionStore},
        utils::util,
    };

    // Load the SessionState directly from the agent capture's SessionStore
    // (Phase 3.4 partition: `<libra_dir>/sessions/agent/<session_id>/`).
    let repo_path = util::try_get_storage_path(None).map_err(|_| CliError::repo_not_found())?;
    let store = SessionStore::from_storage_path_with_subdir(&repo_path, "agent");
    let session = store.load(&args.session_id).map_err(|e| {
        CliError::fatal(format!(
            "failed to load SessionStore JSONL for '{}': {e}. \
             Was the session captured by the hook runtime under sessions/agent/?",
            args.session_id
        ))
    })?;
    let records = derive_tool_call_records(&session);

    if output.is_json() {
        return emit_json_data(
            "agent_session_derive_tool_calls",
            &serde_json::json!({
                "session_id": args.session_id,
                "records": records,
                "count": records.len(),
            }),
            output,
        );
    }
    if output.quiet {
        return Ok(());
    }
    if records.is_empty() {
        println!(
            "(no tool_use events derived from session '{}')",
            args.session_id
        );
        return Ok(());
    }
    println!(
        "Derived {} tool call(s) from '{}':",
        records.len(),
        args.session_id
    );
    println!("{:<24}  {:<14}  success", "tool_name", "action");
    for r in &records {
        println!("{:<24}  {:<14}  {}", r.tool_name, r.action, r.success);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct SessionRow {
    session_id: String,
    agent_kind: String,
    state: String,
    working_dir: String,
    started_at: i64,
    last_event_at: i64,
}

#[derive(Debug, Serialize)]
struct SessionMutationOutput {
    action: &'static str,
    session_id: String,
    previous_state: String,
    state: String,
    updated: bool,
    stopped_at: Option<i64>,
    last_event_at: i64,
}

#[derive(Debug, Serialize)]
struct TranscriptExtraction {
    source_path: String,
    output_path: String,
    bytes: u64,
}

#[derive(Debug, Clone, Copy)]
enum SessionMutationKind {
    Stop,
    Resume,
}

impl SessionMutationKind {
    fn action(self) -> &'static str {
        match self {
            SessionMutationKind::Stop => "stop",
            SessionMutationKind::Resume => "resume",
        }
    }

    fn json_kind(self) -> &'static str {
        match self {
            SessionMutationKind::Stop => "agent_session_stop",
            SessionMutationKind::Resume => "agent_session_resume",
        }
    }
}

fn extract_transcript_from_metadata(
    metadata_json: &str,
    output_path: &str,
) -> CliResult<TranscriptExtraction> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json).map_err(|e| {
        CliError::fatal(format!(
            "captured session metadata_json is not valid JSON; cannot extract transcript: {e}"
        ))
    })?;
    let source = metadata
        .get("transcript_path")
        .and_then(|value| value.as_str())
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| {
            CliError::fatal(
                "captured session metadata_json does not contain transcript_path; cannot extract transcript",
            )
        })?;
    let source_path = std::path::PathBuf::from(source);
    let output = std::path::PathBuf::from(output_path);
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            CliError::fatal(format!(
                "failed to create transcript output directory '{}': {e}",
                parent.display()
            ))
        })?;
    }
    let bytes = std::fs::copy(&source_path, &output).map_err(|e| {
        CliError::fatal(format!(
            "failed to copy transcript from '{}' to '{}': {e}",
            source_path.display(),
            output.display()
        ))
    })?;
    Ok(TranscriptExtraction {
        source_path: source_path.display().to_string(),
        output_path: output.display().to_string(),
        bytes,
    })
}

/// Build the paginated `session list` SQL. Extracted so the EXPLAIN QUERY
/// PLAN tests run the exact production statement against the
/// `idx_agent_session_started_paging` index (never a table SCAN, never a
/// temp B-tree). Placeholder order: `[agent_kind,] [state,] [started_at,
/// started_at, session_id,] limit`. Keyset shape matches the index:
/// `(started_at DESC, session_id ASC)` — see
/// [`super::checkpoint::encode_page_cursor`].
pub(super) fn session_page_sql(with_agent: bool, with_state: bool, with_cursor: bool) -> String {
    let mut sql = String::from(
        "SELECT session_id, agent_kind, state, working_dir, started_at, last_event_at \
         FROM agent_session WHERE 1=1",
    );
    if with_agent {
        sql.push_str(" AND agent_kind = ?");
    }
    if with_state {
        sql.push_str(" AND state = ?");
    }
    if with_cursor {
        sql.push_str(" AND (started_at < ? OR (started_at = ? AND session_id > ?))");
    }
    sql.push_str(" ORDER BY started_at DESC, session_id ASC LIMIT ?");
    sql
}

/// One page of `session list` output. The JSON `data` payload carries the
/// rows under `sessions` (per-row schema unchanged from the
/// pre-pagination output) plus `next_cursor` — the opaque `--cursor`
/// token for the next page, `null` once the listing is exhausted.
#[derive(Debug, Serialize)]
struct SessionListPage {
    schema_version: u32,
    sessions: Vec<SessionRow>,
    next_cursor: Option<String>,
}

async fn list(args: SessionListArgs, output: &OutputConfig) -> CliResult<()> {
    let (limit, clamp_note) = resolve_page_limit(args.limit);
    if let Some(note) = &clamp_note {
        eprintln!("{note}");
    }
    // Decode the cursor before touching the database so a malformed value
    // is a pure usage error.
    let cursor = args.cursor.as_deref().map(decode_page_cursor).transpose()?;

    let conn = get_db_conn_instance().await;
    let backend = conn.get_database_backend();

    if !table_exists(&conn, "agent_session").await? {
        return emit_list(
            &SessionListPage {
                schema_version: PAGE_SCHEMA_VERSION,
                sessions: Vec::new(),
                next_cursor: None,
            },
            output,
        );
    }

    let sql = session_page_sql(args.agent.is_some(), args.state.is_some(), cursor.is_some());
    let mut values: Vec<sea_orm::Value> = Vec::new();
    if let Some(agent) = &args.agent {
        // The CLI accepts hyphenated slugs (`claude-code`) but the database
        // stores the snake_case `agent_kind` (`claude_code`). Translate to
        // the storage form so a `--agent claude-code` filter actually
        // matches rows. Codex review P1 #6.
        let normalized = match AgentKind::from_cli_slug(agent) {
            Some(kind) => kind.as_db_str().to_string(),
            None => agent.clone(),
        };
        values.push(normalized.into());
    }
    if let Some(state) = &args.state {
        values.push(state.clone().into());
    }
    if let Some((timestamp, id)) = &cursor {
        values.push((*timestamp).into());
        values.push((*timestamp).into());
        values.push(id.clone().into());
    }
    // Fetch one row beyond the page to learn whether another page exists
    // without a second COUNT query.
    values.push((limit as i64 + 1).into());

    let stmt = Statement::from_sql_and_values(backend, &sql, values);
    let rows = conn
        .query_all(stmt)
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_session: {e}")))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(SessionRow {
            session_id: row
                .try_get_by::<String, _>("session_id")
                .unwrap_or_default(),
            agent_kind: row
                .try_get_by::<String, _>("agent_kind")
                .unwrap_or_default(),
            state: row.try_get_by::<String, _>("state").unwrap_or_default(),
            working_dir: row
                .try_get_by::<String, _>("working_dir")
                .unwrap_or_default(),
            started_at: row.try_get_by::<i64, _>("started_at").unwrap_or_default(),
            last_event_at: row
                .try_get_by::<i64, _>("last_event_at")
                .unwrap_or_default(),
        });
    }
    let next_cursor = if out.len() as u64 > limit {
        out.truncate(limit as usize);
        out.last()
            .map(|row| encode_page_cursor(row.started_at, &row.session_id))
    } else {
        None
    };
    emit_list(
        &SessionListPage {
            schema_version: PAGE_SCHEMA_VERSION,
            sessions: out,
            next_cursor,
        },
        output,
    )
}

async fn show(args: SessionShowArgs, output: &OutputConfig) -> CliResult<()> {
    let conn = get_db_conn_instance().await;
    let backend = conn.get_database_backend();

    if !table_exists(&conn, "agent_session").await? {
        return Err(CliError::fatal(format!(
            "no captured session matches '{}': agent_session table not yet present (run `libra init`?)",
            args.session_id
        )));
    }

    let stmt = Statement::from_sql_and_values(
        backend,
        "SELECT session_id, agent_kind, state, working_dir, started_at, last_event_at, \
                COALESCE(metadata_json, '{}') AS metadata_json \
         FROM agent_session WHERE session_id = ? LIMIT 1",
        [args.session_id.clone().into()],
    );
    let row = conn
        .query_one(stmt)
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_session: {e}")))?;
    match row {
        Some(row) => {
            let payload = SessionRow {
                session_id: row
                    .try_get_by::<String, _>("session_id")
                    .unwrap_or_default(),
                agent_kind: row
                    .try_get_by::<String, _>("agent_kind")
                    .unwrap_or_default(),
                state: row.try_get_by::<String, _>("state").unwrap_or_default(),
                working_dir: row
                    .try_get_by::<String, _>("working_dir")
                    .unwrap_or_default(),
                started_at: row.try_get_by::<i64, _>("started_at").unwrap_or_default(),
                last_event_at: row
                    .try_get_by::<i64, _>("last_event_at")
                    .unwrap_or_default(),
            };
            let transcript = if let Some(path) = args.extract_transcript.as_deref() {
                let metadata_json = row.try_get_by::<String, _>("metadata_json").map_err(|e| {
                    CliError::fatal(format!(
                        "agent_session.metadata_json for '{}' could not be decoded as TEXT: {e}",
                        args.session_id
                    ))
                })?;
                Some(extract_transcript_from_metadata(&metadata_json, path)?)
            } else {
                None
            };
            if output.is_json() && transcript.is_some() {
                return emit_json_data(
                    "agent_session",
                    &serde_json::json!({
                        "session": payload,
                        "extracted_transcript": transcript,
                    }),
                    output,
                );
            }
            emit_one(&payload, output)?;
            if let Some(transcript) = transcript
                && !output.quiet
            {
                println!("transcript     : {}", transcript.output_path);
                println!("transcript_src : {}", transcript.source_path);
                println!("transcript_len : {} bytes", transcript.bytes);
            }
            Ok(())
        }
        None => Err(CliError::fatal(format!(
            "no captured session matches id '{}'",
            args.session_id
        ))),
    }
}

async fn stop(args: SessionStopArgs, output: &OutputConfig) -> CliResult<()> {
    let conn = get_db_conn_instance().await;
    let result = mutate_session_state(&conn, &args.session_id, SessionMutationKind::Stop).await?;
    emit_session_mutation(&result, output)
}

async fn resume(args: SessionResumeArgs, output: &OutputConfig) -> CliResult<()> {
    let conn = get_db_conn_instance().await;
    let result = mutate_session_state(&conn, &args.session_id, SessionMutationKind::Resume).await?;
    emit_session_mutation(&result, output)
}

async fn mutate_session_state(
    conn: &(impl ConnectionTrait + ?Sized),
    session_id: &str,
    kind: SessionMutationKind,
) -> CliResult<SessionMutationOutput> {
    let backend = conn.get_database_backend();
    if !table_exists(conn, "agent_session").await? {
        return Err(CliError::fatal(format!(
            "no captured session matches '{session_id}': agent_session table not yet present (run `libra init`?)"
        )));
    }

    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT state, stopped_at, last_event_at \
             FROM agent_session WHERE session_id = ? LIMIT 1",
            [session_id.into()],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_session: {e}")))?
        .ok_or_else(|| CliError::fatal(format!("no captured session matches id '{session_id}'")))?;

    let current_state = row.try_get_by::<String, _>("state").map_err(|e| {
        CliError::fatal(format!(
            "agent_session.state for '{session_id}' could not be decoded as TEXT: {e}"
        ))
    })?;
    let current_stopped_at = row
        .try_get_by::<Option<i64>, _>("stopped_at")
        .map_err(|e| {
            CliError::fatal(format!(
                "agent_session.stopped_at for '{session_id}' could not be decoded as INTEGER: {e}"
            ))
        })?;
    let current_last_event_at = row.try_get_by::<i64, _>("last_event_at").map_err(|e| {
        CliError::fatal(format!(
            "agent_session.last_event_at for '{session_id}' could not be decoded as INTEGER: {e}"
        ))
    })?;

    if current_state == "quarantined" {
        return Err(CliError::fatal(format!(
            "cannot {} captured session '{session_id}' because it is quarantined; inspect the capture state before mutating it",
            kind.action()
        )));
    }

    match kind {
        SessionMutationKind::Stop if current_state == "stopped" => {
            return Ok(SessionMutationOutput {
                action: kind.action(),
                session_id: session_id.to_string(),
                previous_state: current_state.clone(),
                state: current_state,
                updated: false,
                stopped_at: current_stopped_at,
                last_event_at: current_last_event_at,
            });
        }
        SessionMutationKind::Resume if current_state == "active" => {
            return Ok(SessionMutationOutput {
                action: kind.action(),
                session_id: session_id.to_string(),
                previous_state: current_state.clone(),
                state: current_state,
                updated: false,
                stopped_at: current_stopped_at,
                last_event_at: current_last_event_at,
            });
        }
        SessionMutationKind::Resume if current_state != "stopped" => {
            return Err(CliError::fatal(format!(
                "cannot resume captured session '{session_id}' from state '{current_state}'; only stopped sessions can be resumed"
            )));
        }
        _ => {}
    }

    let now = chrono::Utc::now().timestamp();
    let (new_state, new_stopped_at) = match kind {
        SessionMutationKind::Stop => ("stopped", Some(now)),
        SessionMutationKind::Resume => ("active", None),
    };
    conn.execute(Statement::from_sql_and_values(
        backend,
        "UPDATE agent_session \
         SET state = ?, last_event_at = ?, stopped_at = ? \
         WHERE session_id = ?",
        vec![
            new_state.into(),
            now.into(),
            new_stopped_at.into(),
            session_id.to_string().into(),
        ],
    ))
    .await
    .map_err(|e| {
        CliError::fatal(format!(
            "failed to update agent_session state for '{session_id}': {e}"
        ))
    })?;

    Ok(SessionMutationOutput {
        action: kind.action(),
        session_id: session_id.to_string(),
        previous_state: current_state,
        state: new_state.to_string(),
        updated: true,
        stopped_at: new_stopped_at,
        last_event_at: now,
    })
}

fn emit_session_mutation(result: &SessionMutationOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        let kind = match result.action {
            "stop" => SessionMutationKind::Stop,
            "resume" => SessionMutationKind::Resume,
            _ => SessionMutationKind::Stop,
        };
        return emit_json_data(kind.json_kind(), result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.updated {
        println!(
            "session '{}' {}: {} -> {}",
            result.session_id, result.action, result.previous_state, result.state
        );
    } else {
        println!("session '{}' already {}", result.session_id, result.state);
    }
    Ok(())
}

fn emit_list(page: &SessionListPage, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("agent_sessions", page, output);
    }
    if output.quiet {
        return Ok(());
    }
    if page.sessions.is_empty() {
        println!("(no captured sessions)");
        return Ok(());
    }
    for line in format_session_list_human(page) {
        println!("{line}");
    }
    Ok(())
}

fn format_session_list_human(page: &SessionListPage) -> Vec<String> {
    format_session_list_human_at(page, chrono::Local::now().timestamp())
}

fn format_session_list_human_at(page: &SessionListPage, now: i64) -> Vec<String> {
    let started_at_values: Vec<String> = page
        .sessions
        .iter()
        .map(|row| text::relative_date_at(now, row.started_at))
        .collect();
    let (session_id_width, agent_kind_width, state_width, started_at_width) =
        session_list_column_widths(&page.sessions, &started_at_values);
    let mut lines =
        Vec::with_capacity(page.sessions.len() + usize::from(page.next_cursor.is_some()) + 1);
    lines.push(format!(
        "{:<session_id_width$}  {:<agent_kind_width$}  {:<state_width$}  {:<started_at_width$}",
        "session_id", "agent_kind", "state", "started_at"
    ));
    for (r, started_at) in page.sessions.iter().zip(started_at_values.iter()) {
        lines.push(format!(
            "{:<session_id_width$}  {:<agent_kind_width$}  {:<state_width$}  {:<started_at_width$}",
            r.session_id, r.agent_kind, r.state, started_at
        ));
    }
    if let Some(cursor) = &page.next_cursor {
        lines.push(format!(
            "(more rows available — next page: --cursor {cursor})"
        ));
    }
    lines
}

fn session_list_column_widths(
    rows: &[SessionRow],
    started_at_values: &[String],
) -> (usize, usize, usize, usize) {
    let session_id_width = rows
        .iter()
        .map(|row| row.session_id.len())
        .chain(std::iter::once("session_id".len()))
        .max()
        .unwrap_or("session_id".len())
        .max(37);
    let agent_kind_width = rows
        .iter()
        .map(|row| row.agent_kind.len())
        .chain(std::iter::once("agent_kind".len()))
        .max()
        .unwrap_or("agent_kind".len())
        .max(14);
    let state_width = rows
        .iter()
        .map(|row| row.state.len())
        .chain(std::iter::once("state".len()))
        .max()
        .unwrap_or("state".len())
        .max(10);
    let started_at_width = started_at_values
        .iter()
        .map(String::len)
        .chain(std::iter::once("started_at".len()))
        .max()
        .unwrap_or("started_at".len())
        .max(20);
    (
        session_id_width,
        agent_kind_width,
        state_width,
        started_at_width,
    )
}

fn emit_one(row: &SessionRow, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("agent_session", row, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("session_id    : {}", row.session_id);
    println!("agent_kind    : {}", row.agent_kind);
    println!("state         : {}", row.state);
    println!("working_dir   : {}", row.working_dir);
    println!("started_at    : {}", row.started_at);
    println!("last_event_at : {}", row.last_event_at);
    Ok(())
}

async fn table_exists(conn: &(impl ConnectionTrait + ?Sized), name: &str) -> CliResult<bool> {
    let backend = conn.get_database_backend();
    let stmt = Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
        [name.into()],
    );
    conn.query_one(stmt)
        .await
        .map(|row| row.is_some())
        .map_err(|e| CliError::fatal(format!("failed to query sqlite_master: {e}")))
}

/// Snapshot of the agent_session columns the promote path needs. Kept
/// as a small struct rather than re-using `SessionRow` (which lacks
/// `provider_session_id` and `metadata_json`) so the promotion logic
/// can run with the minimum the Intent constructor requires.
#[derive(Debug, Clone)]
struct AgentSessionSnapshot {
    session_id: String,
    agent_kind: String,
    provider_session_id: String,
    state: String,
    working_dir: String,
    metadata_json: String,
    started_at: i64,
    last_event_at: i64,
}

async fn load_agent_session_snapshot(
    conn: &sea_orm::DatabaseConnection,
    session_id: &str,
) -> CliResult<AgentSessionSnapshot> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT session_id, agent_kind, provider_session_id, state, working_dir, \
                    COALESCE(metadata_json, '{}') AS metadata_json, \
                    started_at, last_event_at \
             FROM agent_session WHERE session_id = ? LIMIT 1",
            [session_id.into()],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_session: {e}")))?
        .ok_or_else(|| CliError::fatal(format!("no captured session matches id '{session_id}'")))?;

    // Codex round-1 follow-up: surface schema-drift as actionable
    // errors rather than silently falling back to empty strings, which
    // would otherwise let a promotion proceed with empty identity
    // fields. Each column is typed and the failure carries the column
    // name so downstream debugging is direct.
    let column = |name: &str| -> CliResult<String> {
        row.try_get_by::<String, _>(name).map_err(|e| {
            CliError::fatal(format!(
                "agent_session.{name} for '{session_id}' could not be decoded as TEXT: {e}"
            ))
        })
    };
    let column_i64 = |name: &str| -> CliResult<i64> {
        row.try_get_by::<i64, _>(name).map_err(|e| {
            CliError::fatal(format!(
                "agent_session.{name} for '{session_id}' could not be decoded as INTEGER: {e}"
            ))
        })
    };
    Ok(AgentSessionSnapshot {
        session_id: column("session_id")?,
        agent_kind: column("agent_kind")?,
        provider_session_id: column("provider_session_id")?,
        state: column("state")?,
        working_dir: column("working_dir")?,
        metadata_json: column("metadata_json")?,
        started_at: column_i64("started_at")?,
        last_event_at: column_i64("last_event_at")?,
    })
}

/// Build the structured `IntentSpec` payload from a captured session
/// snapshot. Lives in a free function (rather than inline in `promote`)
/// so unit tests can pin the schema without spinning up the database.
fn build_intent_spec_from_snapshot(snapshot: &AgentSessionSnapshot) -> serde_json::Value {
    let captured_metadata: serde_json::Value =
        serde_json::from_str(&snapshot.metadata_json).unwrap_or(serde_json::json!({}));
    serde_json::json!({
        "schema": "libra.agent.promotion.v1",
        "source": "agent_session",
        "agent_kind": snapshot.agent_kind,
        "provider_session_id": snapshot.provider_session_id,
        "session_id": snapshot.session_id,
        "state": snapshot.state,
        "working_dir": snapshot.working_dir,
        "started_at": snapshot.started_at,
        "last_event_at": snapshot.last_event_at,
        "captured_metadata": captured_metadata,
    })
}

/// Default prompt text when the operator did not supply `--prompt`. The
/// shape is stable across releases — projection / index code matches
/// against the `libra.agent.promotion.v1` schema in the spec rather
/// than parsing this string.
///
/// Codex round-1 follow-up: the prompt deliberately does NOT include
/// `working_dir`. Intent objects can land on `refs/libra/intent` that
/// later flows to a remote (R2 / D1 sync, push to a fork) where local
/// filesystem paths leak host details. The full `working_dir` is still
/// available on the structured `IntentSpec` for tools that need it,
/// just not in the user-visible prompt text.
fn default_promotion_prompt(snapshot: &AgentSessionSnapshot) -> String {
    format!(
        "Promoted from external agent session [{}:{}].",
        snapshot.agent_kind, snapshot.provider_session_id
    )
}

async fn promote(args: SessionPromoteArgs, output: &OutputConfig) -> CliResult<()> {
    use crate::utils::util;

    let conn = get_db_conn_instance().await;
    let repo_path = util::try_get_storage_path(None).map_err(|_| CliError::repo_not_found())?;
    promote_with_conn(&conn, repo_path, &args, output).await
}

/// Connection-bound dispatch for `promote --as-intent`. Used by the
/// outer `promote` (which threads in `get_db_conn_instance` and
/// `try_get_storage_path`) and by fixture tests that drive both
/// dry-run and apply paths against an in-memory SQLite + tempdir.
/// Codex round-2 follow-up — the dry-run branch was previously not
/// exercised through the actual dispatch.
async fn promote_with_conn(
    conn: &sea_orm::DatabaseConnection,
    repo_path: std::path::PathBuf,
    args: &SessionPromoteArgs,
    output: &OutputConfig,
) -> CliResult<()> {
    if !args.as_intent {
        return Err(CliError::command_usage(
            "libra agent session promote currently requires --as-intent (only \
             promotion target supported in this phase)",
        ));
    }

    if !table_exists(conn, "agent_session").await? {
        return Err(CliError::fatal(format!(
            "no captured session matches '{}': agent_session table not yet present (run `libra init`?)",
            args.session_id
        )));
    }

    if args.dry_run {
        let snapshot = load_agent_session_snapshot(conn, &args.session_id).await?;
        let prompt = args
            .prompt
            .clone()
            .unwrap_or_else(|| default_promotion_prompt(&snapshot));
        let spec_value = build_intent_spec_from_snapshot(&snapshot);
        use git_internal::internal::object::{intent::Intent, types::ActorRef};
        let actor = ActorRef::system("libra-agent-promote")
            .map_err(|e| CliError::fatal(format!("construct ActorRef: {e}")))?;
        let intent = Intent::new(actor, prompt.clone())
            .map_err(|e| CliError::fatal(format!("construct Intent: {e}")))?;
        let intent_id = intent.header().object_id().to_string();

        if output.is_json() {
            let payload = serde_json::json!({
                "session_id": args.session_id,
                "as_intent": true,
                "applied": false,
                "intent_id": intent_id,
                "prompt": prompt,
                "spec": spec_value,
            });
            return emit_json_data("agent_session_promote", &payload, output);
        }
        if !output.quiet {
            println!("Dry run — no objects written.");
            println!("session_id : {}", snapshot.session_id);
            println!("agent_kind : {}", snapshot.agent_kind);
            println!("intent_id  : {intent_id}");
            println!("prompt     : {prompt}");
            println!("Re-run without --dry-run to write to refs/libra/intent.");
        }
        return Ok(());
    }

    let (intent_id, spec_value, blob_hash) =
        promote_as_intent_with_conn(conn, repo_path, args).await?;

    if output.is_json() {
        let payload = serde_json::json!({
            "session_id": args.session_id,
            "as_intent": true,
            "applied": true,
            "intent_id": intent_id,
            "intent_blob_oid": blob_hash.to_string(),
            "spec": spec_value,
            "history_ref": crate::internal::ai::history::AI_REF,
        });
        return emit_json_data("agent_session_promote", &payload, output);
    }
    if !output.quiet {
        println!("Promoted captured session to refs/libra/intent.");
        println!("intent_id       : {intent_id}");
        println!("intent_blob_oid : {blob_hash}");
        println!(
            "source agent    : {}:{}",
            spec_value["agent_kind"].as_str().unwrap_or(""),
            spec_value["provider_session_id"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

/// Connection-bound core of the `promote --as-intent` flow. Extracted
/// from `promote` so fixture tests can drive it against an in-memory
/// SQLite + tempdir without going through `get_db_conn_instance` and
/// `try_get_storage_path`. Returns the freshly-written Intent's UUID.
async fn promote_as_intent_with_conn(
    conn: &sea_orm::DatabaseConnection,
    repo_path: std::path::PathBuf,
    args: &SessionPromoteArgs,
) -> CliResult<(String, serde_json::Value, git_internal::hash::ObjectHash)> {
    use std::sync::Arc;

    use git_internal::internal::object::{intent::Intent, types::ActorRef};

    use crate::{
        internal::ai::history::HistoryManager,
        utils::{storage::local::LocalStorage, storage_ext::StorageExt},
    };

    let snapshot = load_agent_session_snapshot(conn, &args.session_id).await?;
    let prompt = args
        .prompt
        .clone()
        .unwrap_or_else(|| default_promotion_prompt(&snapshot));
    let spec_value = build_intent_spec_from_snapshot(&snapshot);

    let actor = ActorRef::system("libra-agent-promote")
        .map_err(|e| CliError::fatal(format!("construct ActorRef: {e}")))?;
    let mut intent = Intent::new(actor, prompt.clone())
        .map_err(|e| CliError::fatal(format!("construct Intent: {e}")))?;
    intent.set_spec(Some(git_internal::internal::object::intent::IntentSpec(
        spec_value.clone(),
    )));
    let intent_id = intent.header().object_id().to_string();

    let objects_dir = repo_path.join("objects");
    std::fs::create_dir_all(&objects_dir)
        .map_err(|e| CliError::fatal(format!("create objects dir: {e}")))?;
    let storage = Arc::new(LocalStorage::new(objects_dir));
    let history = HistoryManager::new(storage.clone(), repo_path, Arc::new(conn.clone()));
    let blob_hash = storage
        .put_tracked(&intent, &history)
        .await
        .map_err(|e| CliError::fatal(format!("write Intent to refs/libra/intent: {e}")))?;
    Ok((intent_id, spec_value, blob_hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_fixture() -> AgentSessionSnapshot {
        AgentSessionSnapshot {
            session_id: "claude__abc-123".to_string(),
            agent_kind: "claude_code".to_string(),
            provider_session_id: "abc-123".to_string(),
            state: "stopped".to_string(),
            working_dir: "/Users/eli/repo".to_string(),
            metadata_json: "{\"transcript_path\":\"/tmp/x.jsonl\"}".to_string(),
            started_at: 1_700_000_000,
            last_event_at: 1_700_000_500,
        }
    }

    #[test]
    fn intent_spec_carries_agent_kind_and_provider_session_id() {
        let snapshot = snapshot_fixture();
        let spec = build_intent_spec_from_snapshot(&snapshot);
        assert_eq!(spec["schema"], "libra.agent.promotion.v1");
        assert_eq!(spec["agent_kind"], "claude_code");
        assert_eq!(spec["provider_session_id"], "abc-123");
        assert_eq!(spec["session_id"], "claude__abc-123");
        assert_eq!(spec["state"], "stopped");
        assert_eq!(spec["working_dir"], "/Users/eli/repo");
        assert_eq!(spec["started_at"], 1_700_000_000);
        // captured_metadata round-trips the agent_session.metadata_json
        // verbatim so projection consumers can see the transcript_path
        // alongside the promotion-specific fields.
        assert_eq!(spec["captured_metadata"]["transcript_path"], "/tmp/x.jsonl");
    }

    #[test]
    fn intent_spec_handles_corrupt_metadata_json() {
        let mut snapshot = snapshot_fixture();
        snapshot.metadata_json = "not valid json".to_string();
        let spec = build_intent_spec_from_snapshot(&snapshot);
        // Falls through to an empty object rather than failing the
        // promotion. The agent_kind / provider_session_id we control
        // directly are still present.
        assert!(spec["captured_metadata"].is_object());
        assert_eq!(spec["captured_metadata"].as_object().unwrap().len(), 0);
        assert_eq!(spec["agent_kind"], "claude_code");
    }

    #[test]
    fn default_prompt_names_kind_and_session_without_local_path() {
        let prompt = default_promotion_prompt(&snapshot_fixture());
        assert!(prompt.contains("claude_code"));
        assert!(prompt.contains("abc-123"));
        // Codex round-1 follow-up: working_dir must NOT appear in the
        // prompt text — it would leak local paths into refs that may
        // sync to remotes.
        assert!(
            !prompt.contains("/Users/eli/repo"),
            "default prompt must not embed the local working_dir"
        );
    }

    #[test]
    fn session_list_human_output_aligns_after_long_session_id() {
        let now = 1_700_000_000;
        let page = SessionListPage {
            schema_version: PAGE_SCHEMA_VERSION,
            sessions: vec![
                SessionRow {
                    session_id: format!("claude__{}", "x".repeat(80)),
                    agent_kind: "claude_code".to_string(),
                    state: "active".to_string(),
                    working_dir: "/tmp/repo".to_string(),
                    started_at: now - 2 * 3_600,
                    last_event_at: 1_700_000_100,
                },
                SessionRow {
                    session_id: "short-session".to_string(),
                    agent_kind: "codex".to_string(),
                    state: "stopped".to_string(),
                    working_dir: "/tmp/repo".to_string(),
                    started_at: now - 20 * 86_400,
                    last_event_at: 1_700_000_110,
                },
            ],
            next_cursor: Some("v1:1700000010:short-session".to_string()),
        };

        let lines = format_session_list_human_at(&page, now);
        let header = &lines[0];
        let first_row = &lines[1];
        let second_row = &lines[2];

        let agent_col = header.find("agent_kind").unwrap();
        let state_col = header.find("state").unwrap();
        let started_col = header.find("started_at").unwrap();

        assert_eq!(first_row.find("claude_code").unwrap(), agent_col);
        assert_eq!(second_row.find("codex").unwrap(), agent_col);
        assert_eq!(first_row.find("active").unwrap(), state_col);
        assert_eq!(second_row.find("stopped").unwrap(), state_col);
        assert_eq!(first_row.find("2 hours ago").unwrap(), started_col);
        assert_eq!(second_row.find("3 weeks ago").unwrap(), started_col);
        assert_eq!(
            lines.last().unwrap(),
            "(more rows available — next page: --cursor v1:1700000010:short-session)"
        );
    }

    use sea_orm::{ConnectOptions, Database, DatabaseConnection, ExecResult};
    use tempfile::TempDir;

    use crate::internal::db::{
        ensure_ai_runtime_contract_schema, migration::run_builtin_migrations,
    };

    const LEGACY_BOOTSTRAP_SQL: &str = include_str!("../../../sql/sqlite_20260309_init.sql");

    /// Fresh-DB fixture mirroring the hook runtime / cloud-restore tests.
    /// `repo_path` is rooted at `<tempdir>/.libra/` so `objects/` and
    /// `libra.db` co-locate the way production does.
    async fn fresh_repo() -> (TempDir, DatabaseConnection, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let db_path = repo_path.join(crate::utils::util::DATABASE);
        std::fs::File::create(&db_path).unwrap();
        let url = format!("sqlite://{}", db_path.display());
        let mut opts = ConnectOptions::new(url);
        opts.sqlx_logging(false);
        let conn = Database::connect(opts).await.unwrap();
        let backend = conn.get_database_backend();
        for raw in LEGACY_BOOTSTRAP_SQL.split(';') {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let _: ExecResult = conn
                .execute(Statement::from_string(backend, trimmed.to_string()))
                .await
                .unwrap_or_else(|e| panic!("legacy bootstrap stmt failed: {trimmed}\n{e}"));
        }
        ensure_ai_runtime_contract_schema(&conn).await.unwrap();
        run_builtin_migrations(&conn).await.unwrap();
        (dir, conn, repo_path)
    }

    async fn insert_agent_session_fixture(
        conn: &DatabaseConnection,
        session_id: &str,
        state: &str,
        stopped_at: Option<i64>,
    ) {
        insert_agent_session_fixture_with_metadata(conn, session_id, state, stopped_at, "{}").await;
    }

    async fn insert_agent_session_fixture_with_metadata(
        conn: &DatabaseConnection,
        session_id: &str,
        state: &str,
        stopped_at: Option<i64>,
        metadata_json: &str,
    ) {
        let backend = conn.get_database_backend();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, stopped_at
             ) VALUES (?, 'claude_code', ?, ?, '/tmp/repo', ?, '{}', 1700000000, \
                       1700000100, ?)",
            vec![
                session_id.to_string().into(),
                format!("{session_id}-provider").into(),
                state.to_string().into(),
                metadata_json.to_string().into(),
                stopped_at.into(),
            ],
        ))
        .await
        .unwrap();
    }

    async fn read_agent_session_state(
        conn: &DatabaseConnection,
        session_id: &str,
    ) -> (String, Option<i64>, i64) {
        let backend = conn.get_database_backend();
        let row = conn
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT state, stopped_at, last_event_at \
                 FROM agent_session WHERE session_id = ? LIMIT 1",
                [session_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        (
            row.try_get_by::<String, _>("state").unwrap(),
            row.try_get_by::<Option<i64>, _>("stopped_at").unwrap(),
            row.try_get_by::<i64, _>("last_event_at").unwrap(),
        )
    }

    #[tokio::test]
    async fn agent_session_stop_marks_active_session_stopped() {
        let (_dir, conn, _repo_path) = fresh_repo().await;
        insert_agent_session_fixture(&conn, "claude__stop-active", "active", None).await;

        let result = mutate_session_state(&conn, "claude__stop-active", SessionMutationKind::Stop)
            .await
            .unwrap();

        assert_eq!(result.action, "stop");
        assert!(result.updated);
        assert_eq!(result.previous_state, "active");
        assert_eq!(result.state, "stopped");
        assert_eq!(result.stopped_at, Some(result.last_event_at));

        let (state, stopped_at, last_event_at) =
            read_agent_session_state(&conn, "claude__stop-active").await;
        assert_eq!(state, "stopped");
        assert_eq!(stopped_at, result.stopped_at);
        assert_eq!(last_event_at, result.last_event_at);
    }

    #[tokio::test]
    async fn agent_session_resume_marks_stopped_session_active() {
        let (_dir, conn, _repo_path) = fresh_repo().await;
        insert_agent_session_fixture(
            &conn,
            "claude__resume-stopped",
            "stopped",
            Some(1_700_000_100),
        )
        .await;

        let result =
            mutate_session_state(&conn, "claude__resume-stopped", SessionMutationKind::Resume)
                .await
                .unwrap();

        assert_eq!(result.action, "resume");
        assert!(result.updated);
        assert_eq!(result.previous_state, "stopped");
        assert_eq!(result.state, "active");
        assert_eq!(result.stopped_at, None);

        let (state, stopped_at, last_event_at) =
            read_agent_session_state(&conn, "claude__resume-stopped").await;
        assert_eq!(state, "active");
        assert_eq!(stopped_at, None);
        assert_eq!(last_event_at, result.last_event_at);
    }

    #[tokio::test]
    async fn agent_session_resume_rejects_non_stopped_session_states() {
        let (_dir, conn, _repo_path) = fresh_repo().await;
        insert_agent_session_fixture(&conn, "claude__resume-condensed", "condensed", None).await;

        let err = mutate_session_state(
            &conn,
            "claude__resume-condensed",
            SessionMutationKind::Resume,
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("only stopped sessions can be resumed"),
            "{err}"
        );
    }

    #[test]
    fn agent_session_extract_transcript_copies_metadata_path() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("captured.jsonl");
        let output = dir.path().join("nested").join("copy.jsonl");
        std::fs::write(&source, "{\"type\":\"message\"}\n").unwrap();
        let metadata = serde_json::json!({
            "transcript_path": source,
        })
        .to_string();

        let result =
            extract_transcript_from_metadata(&metadata, output.to_string_lossy().as_ref()).unwrap();

        assert_eq!(result.bytes, 19);
        assert_eq!(result.source_path, source.display().to_string());
        assert_eq!(result.output_path, output.display().to_string());
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            "{\"type\":\"message\"}\n"
        );
    }

    /// Phase 4.2 acceptance: promoting a captured session writes an
    /// Intent blob into `<repo>/objects/` and advances
    /// `refs/libra/intent` to a commit whose root tree contains
    /// `intent/<intent_id>` pointing at the blob hash returned by
    /// `put_tracked`.
    #[tokio::test]
    async fn promote_writes_intent_to_libra_intent_ref() {
        let (_dir, conn, repo_path) = fresh_repo().await;

        let backend = conn.get_database_backend();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES ('claude__sess-promote', 'claude_code', 'sess-promote', 'stopped', \
                       '/tmp/repo', '{\"transcript_path\":\"/tmp/repo/.claude/sess.jsonl\"}', \
                       '{}', 1700000000, 1700000500)",
            [],
        ))
        .await
        .unwrap();

        let args = SessionPromoteArgs {
            session_id: "claude__sess-promote".to_string(),
            as_intent: true,
            prompt: None,
            dry_run: false,
        };
        let (intent_id, spec_value, blob_hash) =
            super::promote_as_intent_with_conn(&conn, repo_path.clone(), &args)
                .await
                .expect("promote_as_intent_with_conn");

        // Spec round-trip — agent_kind / provider_session_id must
        // survive verbatim so projection tooling can attribute the
        // Intent back to the captured session.
        assert_eq!(spec_value["agent_kind"], "claude_code");
        assert_eq!(spec_value["provider_session_id"], "sess-promote");
        assert!(!intent_id.is_empty());

        // The Intent blob landed in `<repo>/objects/<prefix>/<rest>`.
        let blob_str = blob_hash.to_string();
        let object_path = repo_path
            .join("objects")
            .join(&blob_str[..2])
            .join(&blob_str[2..]);
        assert!(
            object_path.exists(),
            "Intent blob missing at {object_path:?}"
        );

        // Walk the actual on-disk Git objects: refs/libra/intent →
        // commit → root tree → intent/ subtree → <intent_id> entry.
        // The leaf entry's hash MUST equal the blob_hash returned by
        // `put_tracked`; otherwise we have a Git-ref/intent
        // misalignment that downstream projection tooling can't
        // recover from. Codex round-1 follow-up: previously this only
        // checked that the ref had a commit, not that the tree
        // entry pointed at the right blob.
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        use crate::internal::{ai::history::AI_REF, model::reference};
        let ref_row = reference::Entity::find()
            .filter(reference::Column::Name.eq(AI_REF))
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
            .one(&conn)
            .await
            .unwrap()
            .expect("refs/libra/intent must exist after promote");
        let head_commit = ref_row
            .commit
            .expect("refs/libra/intent must have a commit hash");

        let entry = read_tree_entry(&repo_path, &head_commit, "intent", &intent_id);
        assert_eq!(
            entry, blob_str,
            "intent/<intent_id> tree entry must point at the blob hash returned by put_tracked"
        );
    }

    /// Walk `<repo>/objects/<commit_oid>` → its root tree → `<type>` →
    /// `<id>` and return the leaf entry's OID. Panics with a precise
    /// message at any step that is missing or malformed.
    fn read_tree_entry(
        repo_path: &std::path::Path,
        commit_oid: &str,
        object_type: &str,
        object_id: &str,
    ) -> String {
        let read_object = |oid: &str| -> Vec<u8> {
            let path = repo_path.join("objects").join(&oid[..2]).join(&oid[2..]);
            let raw = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("read object {oid} at {}: {e}", path.display()));
            let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
            let mut decoded = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
            decoded
        };
        let commit = read_object(commit_oid);
        let header_end = commit.iter().position(|&b| b == 0).unwrap();
        let body = &commit[header_end + 1..];
        let body_text = std::str::from_utf8(body).unwrap();
        let tree_line = body_text.lines().next().unwrap();
        let root_tree_oid = tree_line.strip_prefix("tree ").unwrap().to_string();

        let type_subtree_oid = lookup_tree_entry(&read_object(&root_tree_oid), object_type)
            .unwrap_or_else(|| panic!("root tree missing entry '{object_type}'"));
        lookup_tree_entry(&read_object(&type_subtree_oid), object_id)
            .unwrap_or_else(|| panic!("'{object_type}' subtree missing entry '{object_id}'"))
    }

    fn lookup_tree_entry(tree_object: &[u8], name: &str) -> Option<String> {
        let header_end = tree_object.iter().position(|&b| b == 0).unwrap();
        let body = &tree_object[header_end + 1..];
        let mut cursor = 0;
        while cursor < body.len() {
            let space_pos = cursor + body[cursor..].iter().position(|&b| b == b' ').unwrap();
            let name_start = space_pos + 1;
            let null_pos = name_start + body[name_start..].iter().position(|&b| b == 0).unwrap();
            let entry_name = std::str::from_utf8(&body[name_start..null_pos]).unwrap();
            let hash_start = null_pos + 1;
            let hash_bytes = &body[hash_start..hash_start + 20];
            if entry_name == name {
                return Some(hex::encode(hash_bytes));
            }
            cursor = hash_start + 20;
        }
        None
    }

    /// Codex round-2 follow-up: actually invoke `promote_with_conn`
    /// with `dry_run: true` and verify the dispatch leaves the object
    /// store and the ref row untouched. The previous test only
    /// exercised `load_agent_session_snapshot` /
    /// `build_intent_spec_from_snapshot`, missing the actual dry-run
    /// branch.
    #[tokio::test]
    async fn promote_dry_run_does_not_write() {
        let (_dir, conn, repo_path) = fresh_repo().await;

        let backend = conn.get_database_backend();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES ('claude__sess-dry', 'claude_code', 'sess-dry', 'stopped', '/tmp', \
                       '{}', '{}', 0, 0)",
            [],
        ))
        .await
        .unwrap();

        let args = SessionPromoteArgs {
            session_id: "claude__sess-dry".to_string(),
            as_intent: true,
            prompt: None,
            dry_run: true,
        };
        // Build a quiet OutputConfig so the dispatch's println! /
        // emit_json_data branches are exercised without polluting the
        // test runner's stdout.
        let output = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };
        super::promote_with_conn(&conn, repo_path.clone(), &args, &output)
            .await
            .expect("dry-run dispatch should succeed");

        // After the actual dry-run dispatch, `objects/` must remain
        // empty and `refs/libra/intent` must not exist.
        let objects_dir = repo_path.join("objects");
        let exists_with_contents = objects_dir.exists()
            && std::fs::read_dir(&objects_dir)
                .map(|d| d.count() > 0)
                .unwrap_or(false);
        assert!(
            !exists_with_contents,
            "dry-run path must not populate objects/"
        );

        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        use crate::internal::{ai::history::AI_REF, model::reference};
        let ref_row = reference::Entity::find()
            .filter(reference::Column::Name.eq(AI_REF))
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
            .one(&conn)
            .await
            .unwrap();
        assert!(
            ref_row.is_none(),
            "refs/libra/intent must not be created on dry-run"
        );
    }

    /// Promote with an explicit `--prompt` overrides the auto-derived
    /// summary. The spec still carries the original session metadata,
    /// AND the override actually persists into the stored Intent's
    /// `prompt` field on disk. Codex round-1 follow-up: previously
    /// this test only checked the spec, leaving the prompt-override
    /// path unverified.
    #[tokio::test]
    async fn promote_honors_explicit_prompt_override() {
        let (_dir, conn, repo_path) = fresh_repo().await;
        let backend = conn.get_database_backend();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES ('claude__sess-prompt', 'claude_code', 'sess-prompt', 'stopped', '/tmp', \
                       '{}', '{}', 0, 0)",
            [],
        ))
        .await
        .unwrap();

        let args = SessionPromoteArgs {
            session_id: "claude__sess-prompt".to_string(),
            as_intent: true,
            prompt: Some("Refactor the auth module".to_string()),
            dry_run: false,
        };
        let (_intent_id, spec_value, blob_hash) =
            super::promote_as_intent_with_conn(&conn, repo_path.clone(), &args)
                .await
                .unwrap();
        // Spec retains agent metadata regardless of prompt override.
        assert_eq!(spec_value["agent_kind"], "claude_code");
        assert_eq!(spec_value["provider_session_id"], "sess-prompt");

        // Read back the stored Intent blob and confirm the prompt
        // override actually persisted. The Intent struct serialises
        // `prompt` at the top level via `serde(flatten)`-style header.
        let blob_str = blob_hash.to_string();
        let object_path = repo_path
            .join("objects")
            .join(&blob_str[..2])
            .join(&blob_str[2..]);
        let raw = std::fs::read(&object_path).unwrap();
        let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
        let header_end = decoded.iter().position(|&b| b == 0).unwrap();
        let body = &decoded[header_end + 1..];
        let parsed: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(
            parsed["prompt"], "Refactor the auth module",
            "stored Intent must carry the --prompt override verbatim"
        );
    }

    /// Codex round-1 follow-up: schema drift in the agent_session
    /// columns must surface as an actionable error rather than a
    /// silent success-with-empty-strings. We simulate this by
    /// dropping the table and asserting the loader fails with a
    /// recognisable message — this exercises the
    /// `unwrap_or_default` removal at the column-decoding layer.
    #[tokio::test]
    async fn load_snapshot_surfaces_missing_session_with_actionable_error() {
        let (_dir, conn, _repo_path) = fresh_repo().await;
        let err = super::load_agent_session_snapshot(&conn, "no-such-session")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("no captured session matches"),
            "missing session must surface a recognisable error: {err}"
        );
    }
}
