//! Read-only graph projection for captured external-agent sessions.
//!
//! This module deliberately does not reuse `command::graph`'s projection
//! resolver. External-agent capture is keyed by `agent_session.session_id`,
//! not by an orchestrator `thread_id`; only the terminal shell and visual
//! layout are shared (plan-20260713 ADR-DR-20).

use std::{
    collections::BTreeMap,
    io::{IsTerminal, stdin, stdout},
    path::{Path, PathBuf},
    time::Duration,
};

use clap::Args;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    prelude::{Color, Line, Span, Style},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, QueryResult, Statement, TransactionTrait, TryGetable,
};
use serde::Serialize;

use crate::{
    command::agent::checkpoint::load_checkpoint_transcript_bytes_from_storage,
    internal::{
        ai::observed_agents::Redactor,
        db::get_db_conn_instance_for_path,
        tui::{Tui, tui_init, tui_restore},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util::{DATABASE, try_get_storage_path},
    },
};

pub const AGENT_GRAPH_EXAMPLES: &str = "\
EXAMPLES:
    libra agent graph <session>                     Browse captured turns and revisions
    libra agent graph <session> --repo /path/to/repo  Inspect capture data in another repository
    libra --json agent graph <session>              Emit the frozen capture-graph JSON v1 schema
    libra --machine agent graph <session>           Emit compact machine-readable JSON";

#[derive(Args, Debug)]
#[command(after_help = AGENT_GRAPH_EXAMPLES)]
pub struct GraphArgs {
    /// Captured session id from `libra agent session list`
    #[arg(value_name = "SESSION")]
    pub session: String,

    /// Path to a Libra repository to inspect (default: discover from current directory)
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AgentGraphOutput {
    schema_version: u32,
    state: String,
    session: Option<SessionOutput>,
    turns: Vec<TurnOutput>,
    subagents: SubagentsOutput,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SessionOutput {
    session_id: String,
    agent_kind: String,
    state: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct TurnOutput {
    logical_turn_key: String,
    ordinal: usize,
    coverage_schema_version: Option<i64>,
    coverage_state: String,
    completeness: Option<String>,
    current_revision: Option<i64>,
    checkpoint_id: Option<String>,
    source_channel: Option<String>,
    revisions: Vec<RevisionOutput>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RevisionOutput {
    revision: i64,
    completeness: String,
    checkpoint_id: String,
    source_channel: String,
    created_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SubagentsOutput {
    available: bool,
    unavailable_reason: Option<String>,
    nodes: Vec<SubagentNodeOutput>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SubagentNodeOutput {
    checkpoint_id: String,
    link_state: String,
    boundary_checkpoint_id: Option<String>,
    created_at: i64,
}

#[derive(Debug, Clone)]
struct CheckpointStructure {
    checkpoint_id: String,
    parent_checkpoint_id: Option<String>,
    scope: String,
    created_at: i64,
    content: CheckpointContentSummary,
}

/// Bounded, redacted content used only by the interactive graph detail pane.
/// JSON graph output remains the frozen metadata-only schema, while the TUI
/// can answer what was in a captured turn without rendering an entire
/// transcript.
#[derive(Debug, Clone, Default)]
struct CheckpointContentSummary {
    availability: String,
    truncated: bool,
    event_count: usize,
    user_message_count: usize,
    assistant_message_count: usize,
    user_preview: Option<String>,
    assistant_preview: Option<String>,
}

#[derive(Debug, Clone)]
struct AgentGraphProjection {
    output: AgentGraphOutput,
    checkpoints: BTreeMap<String, CheckpointStructure>,
}

#[derive(Debug)]
struct ClaimRow {
    logical_turn_key: String,
    coverage_schema_version: i64,
    state: String,
    completeness: String,
    revision: i64,
    checkpoint_id: String,
    source_channel: String,
}

pub async fn execute_safe(args: GraphArgs, output: &OutputConfig) -> CliResult<()> {
    let storage_root = try_get_storage_path(args.repo.clone()).map_err(|_| {
        CliError::repo_not_found()
            .with_hint("verify that --repo names an initialized Libra repository.")
    })?;
    let interactive = stdin().is_terminal() && stdout().is_terminal();
    let projection = load_agent_graph(&storage_root, &args.session, interactive).await?;

    if output.is_json() {
        return emit_json_data("agent_graph", &projection.output, output);
    }

    if !interactive {
        return Err(CliError::command_usage(
            "agent graph requires an interactive terminal unless --json or --machine is used",
        )
        .with_hint("rerun as `libra --json agent graph <session>` for structured output."));
    }

    run_agent_graph_tui(projection).map_err(|error| {
        CliError::io(format!("failed to run agent graph TUI: {error}"))
            .with_hint("rerun with --json for non-interactive output.")
    })
}

async fn load_agent_graph(
    storage_root: &Path,
    session_id: &str,
    include_content: bool,
) -> CliResult<AgentGraphProjection> {
    let db_path = storage_root.join(DATABASE);
    let connection = get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|_| graph_store_error("open the repository capture catalog"))?;
    let transaction = connection
        .begin()
        .await
        .map_err(|_| graph_store_error("start a consistent capture-graph read"))?;

    let result =
        load_agent_graph_from_connection(&transaction, session_id, storage_root, include_content)
            .await;
    match result {
        Ok(projection) => {
            transaction
                .commit()
                .await
                .map_err(|_| graph_store_error("finish the capture-graph read"))?;
            Ok(projection)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

async fn load_agent_graph_from_connection<C: ConnectionTrait>(
    connection: &C,
    session_id: &str,
    storage_root: &Path,
    include_content: bool,
) -> CliResult<AgentGraphProjection> {
    if tombstone_exists(connection, session_id).await? {
        return Ok(AgentGraphProjection {
            output: AgentGraphOutput {
                schema_version: 1,
                state: "erased".to_string(),
                session: None,
                turns: Vec::new(),
                subagents: SubagentsOutput {
                    available: false,
                    unavailable_reason: Some("erased".to_string()),
                    nodes: Vec::new(),
                },
            },
            checkpoints: BTreeMap::new(),
        });
    }

    let session = load_session(connection, session_id).await?.ok_or_else(|| {
        CliError::fatal(format!(
            "captured agent session '{}' is unknown",
            safe_display(session_id)
        ))
        .with_stable_code(StableErrorCode::AgentGraphSessionUnknown)
        .with_hint("run `libra agent session list` and pass its exact session_id.")
    })?;
    let checkpoints =
        load_checkpoints(connection, session_id, storage_root, include_content).await?;
    let mut turns = load_indexed_turns(connection, session_id).await?;
    validate_turn_checkpoints(&turns, &checkpoints)?;
    if turns.is_empty() {
        let mut chronology = checkpoints.values().collect::<Vec<_>>();
        chronology.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.checkpoint_id.cmp(&right.checkpoint_id))
        });
        turns = chronology
            .into_iter()
            .enumerate()
            .map(|(ordinal, checkpoint)| TurnOutput {
                logical_turn_key: format!("checkpoint:{}", checkpoint.checkpoint_id),
                ordinal,
                coverage_schema_version: None,
                coverage_state: "unindexed".to_string(),
                completeness: None,
                current_revision: None,
                checkpoint_id: Some(checkpoint.checkpoint_id.clone()),
                source_channel: None,
                revisions: Vec::new(),
            })
            .collect();
    }
    let subagents = load_subagents(connection, session_id, &checkpoints).await?;

    Ok(AgentGraphProjection {
        output: AgentGraphOutput {
            schema_version: 1,
            state: "present".to_string(),
            session: Some(session),
            turns,
            subagents,
        },
        checkpoints,
    })
}

async fn tombstone_exists<C: ConnectionTrait>(connection: &C, session_id: &str) -> CliResult<bool> {
    let row = connection
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT erased_session_id FROM agent_import_tombstone \
             WHERE erased_session_id = ? LIMIT 1",
            [session_id.to_owned().into()],
        ))
        .await
        .map_err(|_| graph_store_error("read the local erase barrier"))?;
    Ok(row.is_some())
}

async fn load_session<C: ConnectionTrait>(
    connection: &C,
    session_id: &str,
) -> CliResult<Option<SessionOutput>> {
    let row = connection
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT session_id, agent_kind, state, started_at AS created_at, \
                    last_event_at AS updated_at \
             FROM agent_session WHERE session_id = ? LIMIT 1",
            [session_id.to_owned().into()],
        ))
        .await
        .map_err(|_| graph_store_error("read the captured session"))?;
    row.map(|row| {
        Ok(SessionOutput {
            session_id: required(&row, "session_id")?,
            agent_kind: required(&row, "agent_kind")?,
            state: required(&row, "state")?,
            created_at: required(&row, "created_at")?,
            updated_at: required(&row, "updated_at")?,
        })
    })
    .transpose()
}

async fn load_checkpoints<C: ConnectionTrait>(
    connection: &C,
    session_id: &str,
    storage_root: &Path,
    include_content: bool,
) -> CliResult<BTreeMap<String, CheckpointStructure>> {
    let rows = connection
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT checkpoint_id, session_id, parent_checkpoint_id, scope, tree_oid, created_at \
             FROM agent_checkpoint WHERE session_id = ? \
             ORDER BY created_at, checkpoint_id",
            [session_id.to_owned().into()],
        ))
        .await
        .map_err(|_| graph_store_error("read checkpoint structure"))?;
    let mut checkpoints = BTreeMap::new();
    for row in rows {
        let checkpoint_id = required::<String>(&row, "checkpoint_id")?;
        let checkpoint = CheckpointStructure {
            checkpoint_id: checkpoint_id.clone(),
            parent_checkpoint_id: required(&row, "parent_checkpoint_id")?,
            scope: required(&row, "scope")?,
            created_at: required(&row, "created_at")?,
            content: if include_content {
                load_checkpoint_content_summary(
                    storage_root,
                    &checkpoint_id,
                    &required::<String>(&row, "tree_oid")?,
                )
            } else {
                CheckpointContentSummary::default()
            },
        };
        checkpoints.insert(checkpoint.checkpoint_id.clone(), checkpoint);
    }
    Ok(checkpoints)
}

const GRAPH_CONTENT_READ_MAX_BYTES: u64 = 256 * 1024;
const GRAPH_PREVIEW_MAX_CHARS: usize = 240;

fn load_checkpoint_content_summary(
    storage_root: &Path,
    checkpoint_id: &str,
    tree_oid: &str,
) -> CheckpointContentSummary {
    let Ok((bytes, truncated)) = load_checkpoint_transcript_bytes_from_storage(
        storage_root,
        checkpoint_id,
        tree_oid,
        GRAPH_CONTENT_READ_MAX_BYTES,
    ) else {
        return CheckpointContentSummary {
            availability: "unavailable".to_string(),
            ..CheckpointContentSummary::default()
        };
    };
    summarize_transcript(&bytes, truncated)
}

fn summarize_transcript(bytes: &[u8], truncated: bool) -> CheckpointContentSummary {
    let mut summary = CheckpointContentSummary {
        availability: "available".to_string(),
        truncated,
        ..CheckpointContentSummary::default()
    };
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        summary.event_count += 1;
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(serde_json::Value::as_str) != Some("message") {
            continue;
        }
        let Some(role) = payload.get("role").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(text) = message_text(payload) else {
            continue;
        };
        let Some(preview) = compact_preview(&text) else {
            continue;
        };
        match role {
            "user" => {
                summary.user_message_count += 1;
                summary.user_preview = Some(preview);
            }
            "assistant" => {
                summary.assistant_message_count += 1;
                summary.assistant_preview = Some(preview);
            }
            _ => {}
        }
    }
    summary
}

fn message_text(payload: &serde_json::Value) -> Option<String> {
    if let Some(text) = payload.get("text").and_then(serde_json::Value::as_str) {
        return Some(text.to_string());
    }
    payload
        .get("content")
        .and_then(serde_json::Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|text| !text.trim().is_empty())
}

fn compact_preview(text: &str) -> Option<String> {
    let redactor = Redactor::new_default();
    let (redacted, _) = redactor.redact(text.as_bytes());
    let compact = String::from_utf8_lossy(redacted.bytes())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact.is_empty() {
        return None;
    }
    let mut preview = compact
        .chars()
        .take(GRAPH_PREVIEW_MAX_CHARS)
        .collect::<String>();
    if compact.chars().count() > GRAPH_PREVIEW_MAX_CHARS {
        preview.push_str("...");
    }
    Some(preview)
}

async fn load_indexed_turns<C: ConnectionTrait>(
    connection: &C,
    session_id: &str,
) -> CliResult<Vec<TurnOutput>> {
    let claim_rows = connection
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT logical_turn_key, coverage_schema_version, state, completeness, revision, \
                    checkpoint_id, source_channel \
             FROM agent_coverage_claim \
             WHERE session_id = ? AND revision > 0 AND checkpoint_id IS NOT NULL \
             ORDER BY logical_turn_key, coverage_schema_version",
            [session_id.to_owned().into()],
        ))
        .await
        .map_err(|_| graph_store_error("read current turn coverage"))?;
    let revision_rows = connection
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT logical_turn_key, coverage_schema_version, revision, completeness, \
                    checkpoint_id, source_channel, created_at \
             FROM agent_coverage_revision WHERE session_id = ? \
             ORDER BY logical_turn_key, coverage_schema_version, revision",
            [session_id.to_owned().into()],
        ))
        .await
        .map_err(|_| graph_store_error("read turn revision history"))?;

    let mut revisions = BTreeMap::<(String, i64), Vec<RevisionOutput>>::new();
    for row in revision_rows {
        let logical_turn_key: String = required(&row, "logical_turn_key")?;
        let coverage_schema_version: i64 = required(&row, "coverage_schema_version")?;
        revisions
            .entry((logical_turn_key, coverage_schema_version))
            .or_default()
            .push(RevisionOutput {
                revision: required(&row, "revision")?,
                completeness: required(&row, "completeness")?,
                checkpoint_id: required(&row, "checkpoint_id")?,
                source_channel: required(&row, "source_channel")?,
                created_at: required(&row, "created_at")?,
            });
    }

    let mut claims = Vec::with_capacity(claim_rows.len());
    for row in claim_rows {
        claims.push(ClaimRow {
            logical_turn_key: required(&row, "logical_turn_key")?,
            coverage_schema_version: required(&row, "coverage_schema_version")?,
            state: required(&row, "state")?,
            completeness: required(&row, "completeness")?,
            revision: required(&row, "revision")?,
            checkpoint_id: required(&row, "checkpoint_id")?,
            source_channel: required(&row, "source_channel")?,
        });
    }

    let mut turns = Vec::new();
    for claim in claims {
        let key = (
            claim.logical_turn_key.clone(),
            claim.coverage_schema_version,
        );
        let Some(history) = revisions.remove(&key) else {
            // A reserved or structurally incomplete claim is not a committed
            // indexed turn. Readers never manufacture a revision for it.
            continue;
        };
        let Some(current) = history
            .iter()
            .find(|revision| revision.revision == claim.revision)
        else {
            return Err(graph_store_error(
                "validate the current turn against revision history",
            ));
        };
        if current.checkpoint_id != claim.checkpoint_id {
            return Err(graph_store_error(
                "validate the current turn checkpoint against its revision",
            ));
        }
        // During an incomplete -> complete upgrade, reservation deliberately
        // puts the incoming digest/completeness/channel on the claim while its
        // revision/checkpoint still point to the last committed revision. The
        // graph is a committed-data projection, so render metadata from that
        // revision until the final catalog transaction advances the pointer.
        if matches!(claim.state.as_str(), "catalog_committed" | "conflicted")
            && (current.completeness != claim.completeness
                || current.source_channel != claim.source_channel)
        {
            return Err(graph_store_error(
                "validate current turn metadata against its revision",
            ));
        }
        let current_completeness = current.completeness.clone();
        let current_source_channel = current.source_channel.clone();
        let ordinal = turns.len();
        turns.push(TurnOutput {
            logical_turn_key: claim.logical_turn_key,
            ordinal,
            coverage_schema_version: Some(claim.coverage_schema_version),
            coverage_state: "indexed".to_string(),
            completeness: Some(current_completeness),
            current_revision: Some(claim.revision),
            checkpoint_id: Some(claim.checkpoint_id),
            source_channel: Some(current_source_channel),
            revisions: history,
        });
    }
    Ok(turns)
}

fn validate_turn_checkpoints(
    turns: &[TurnOutput],
    checkpoints: &BTreeMap<String, CheckpointStructure>,
) -> CliResult<()> {
    for turn in turns {
        if turn
            .checkpoint_id
            .as_ref()
            .is_some_and(|checkpoint_id| !checkpoints.contains_key(checkpoint_id))
            || turn
                .revisions
                .iter()
                .any(|revision| !checkpoints.contains_key(&revision.checkpoint_id))
        {
            return Err(graph_store_error(
                "validate turn checkpoints within the captured session",
            ));
        }
    }
    Ok(())
}

async fn load_subagents<C: ConnectionTrait>(
    connection: &C,
    session_id: &str,
    checkpoints: &BTreeMap<String, CheckpointStructure>,
) -> CliResult<SubagentsOutput> {
    let rows = connection
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT l.content_checkpoint_id AS checkpoint_id, l.link_state, \
                    l.boundary_checkpoint_id, c.created_at \
             FROM agent_subagent_link AS l \
             JOIN agent_checkpoint AS c ON c.checkpoint_id = l.content_checkpoint_id \
             WHERE l.parent_session_id = ? \
             ORDER BY c.created_at, l.content_checkpoint_id",
            [session_id.to_owned().into()],
        ))
        .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error)
            if error
                .to_string()
                .contains("no such table: agent_subagent_link") =>
        {
            return Ok(SubagentsOutput {
                available: false,
                unavailable_reason: Some("schema_unavailable".to_string()),
                nodes: Vec::new(),
            });
        }
        Err(_) => return Err(graph_store_error("read subagent capture links")),
    };

    let mut nodes = Vec::with_capacity(rows.len());
    for row in rows {
        let checkpoint_id: String = required(&row, "checkpoint_id")?;
        let link_state: String = required(&row, "link_state")?;
        let boundary_checkpoint_id: Option<String> = required(&row, "boundary_checkpoint_id")?;
        if !matches!(link_state.as_str(), "resolved" | "unresolved")
            || !checkpoints.contains_key(&checkpoint_id)
            || boundary_checkpoint_id
                .as_ref()
                .is_some_and(|boundary| !checkpoints.contains_key(boundary))
        {
            return Err(graph_store_error(
                "validate subagent links within the captured session",
            ));
        }
        nodes.push(SubagentNodeOutput {
            checkpoint_id,
            link_state,
            boundary_checkpoint_id,
            created_at: required(&row, "created_at")?,
        });
    }
    Ok(SubagentsOutput {
        available: true,
        unavailable_reason: None,
        nodes,
    })
}

fn required<T: TryGetable>(row: &QueryResult, column: &str) -> CliResult<T> {
    row.try_get("", column)
        .map_err(|_| graph_store_error("decode capture-graph metadata"))
}

fn graph_store_error(action: &str) -> CliError {
    CliError::fatal(format!("failed to {action}"))
        .with_stable_code(StableErrorCode::AgentCheckpointStoreInconsistent)
        .with_hint("run `libra agent doctor` to inspect the capture catalog.")
}

fn safe_display(value: &str) -> String {
    let mut output = value
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect::<String>();
    if value
        .chars()
        .filter(|character| !character.is_control())
        .count()
        > 160
    {
        output.push_str("...");
    }
    output
}

// GitHub Dark palette copied from the existing thread graph TUI shell.
const COLOR_BG: Color = Color::Rgb(13, 17, 23);
const COLOR_BG_PANEL: Color = Color::Rgb(1, 4, 9);
const COLOR_BG_SEL: Color = Color::Rgb(31, 111, 235);
const COLOR_FG: Color = Color::Rgb(201, 209, 217);
const COLOR_FG_MUTED: Color = Color::Rgb(110, 118, 129);
const COLOR_BORDER: Color = Color::Rgb(48, 54, 61);
const COLOR_ACCENT: Color = Color::Rgb(88, 166, 255);

#[derive(Debug, Clone)]
struct TuiRow {
    label: String,
    details: Vec<(String, String)>,
}

struct AgentGraphTuiApp {
    rows: Vec<TuiRow>,
    state: ListState,
}

fn append_content_details(
    details: &mut Vec<(String, String)>,
    content: Option<&CheckpointContentSummary>,
) {
    let Some(content) = content else {
        details.push(("content_status".to_string(), "not linked".to_string()));
        return;
    };
    let status = if content.availability.is_empty() {
        "not loaded"
    } else {
        content.availability.as_str()
    };
    details.push(("content_status".to_string(), status.to_string()));
    if content.event_count > 0 {
        details.push((
            "events".to_string(),
            format!(
                "{} (user {}, assistant {}){}",
                content.event_count,
                content.user_message_count,
                content.assistant_message_count,
                if content.truncated {
                    ", preview truncated"
                } else {
                    ""
                }
            ),
        ));
    }
    if let Some(preview) = &content.user_preview {
        details.push(("user_content".to_string(), preview.clone()));
    }
    if let Some(preview) = &content.assistant_preview {
        details.push(("assistant_content".to_string(), preview.clone()));
    }
}

impl AgentGraphTuiApp {
    fn new(projection: &AgentGraphProjection) -> Self {
        let mut rows = Vec::new();
        if projection.output.state == "erased" {
            rows.push(TuiRow {
                label: "session [erased]".to_string(),
                details: vec![("state".to_string(), "erased".to_string())],
            });
        } else if let Some(session) = &projection.output.session {
            let mut session_details = vec![
                ("agent_kind".to_string(), safe_display(&session.agent_kind)),
                ("state".to_string(), safe_display(&session.state)),
                ("created_at".to_string(), session.created_at.to_string()),
                ("updated_at".to_string(), session.updated_at.to_string()),
                (
                    "checkpoints".to_string(),
                    projection.checkpoints.len().to_string(),
                ),
            ];
            let latest_checkpoint = projection
                .checkpoints
                .values()
                .max_by_key(|checkpoint| checkpoint.created_at);
            append_content_details(
                &mut session_details,
                latest_checkpoint.map(|checkpoint| &checkpoint.content),
            );
            rows.push(TuiRow {
                label: format!("session {}", safe_display(&session.session_id)),
                details: session_details,
            });
            for turn in &projection.output.turns {
                let current_checkpoint = turn
                    .checkpoint_id
                    .as_ref()
                    .and_then(|id| projection.checkpoints.get(id));
                let current_scope = current_checkpoint
                    .map(|checkpoint| checkpoint.scope.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let mut turn_details = vec![
                    ("coverage_state".to_string(), turn.coverage_state.clone()),
                    (
                        "completeness".to_string(),
                        turn.completeness
                            .clone()
                            .unwrap_or_else(|| "unindexed".to_string()),
                    ),
                    (
                        "current_revision".to_string(),
                        turn.current_revision
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "none".to_string()),
                    ),
                    ("scope".to_string(), current_scope),
                    (
                        "source_channel".to_string(),
                        turn.source_channel
                            .clone()
                            .unwrap_or_else(|| "none".to_string()),
                    ),
                ];
                append_content_details(
                    &mut turn_details,
                    current_checkpoint.map(|checkpoint| &checkpoint.content),
                );
                rows.push(TuiRow {
                    label: format!(
                        "  turn[{}] {} [{}]",
                        turn.ordinal,
                        safe_display(&turn.logical_turn_key),
                        turn.coverage_state
                    ),
                    details: turn_details,
                });
                for revision in &turn.revisions {
                    let checkpoint = projection.checkpoints.get(&revision.checkpoint_id);
                    let mut revision_details = vec![
                        (
                            "checkpoint_id".to_string(),
                            safe_display(&revision.checkpoint_id),
                        ),
                        (
                            "scope".to_string(),
                            checkpoint
                                .map(|value| value.scope.clone())
                                .unwrap_or_else(|| "unknown".to_string()),
                        ),
                        ("created_at".to_string(), revision.created_at.to_string()),
                    ];
                    append_content_details(
                        &mut revision_details,
                        checkpoint.map(|checkpoint| &checkpoint.content),
                    );
                    rows.push(TuiRow {
                        label: format!(
                            "    revision {} {} ({})",
                            revision.revision, revision.completeness, revision.source_channel
                        ),
                        details: revision_details,
                    });
                }
            }
            if projection.output.subagents.available {
                for node in &projection.output.subagents.nodes {
                    let checkpoint = projection.checkpoints.get(&node.checkpoint_id);
                    let mut subagent_details = vec![
                        ("scope".to_string(), "subagent".to_string()),
                        ("link_state".to_string(), node.link_state.clone()),
                        (
                            "boundary_checkpoint_id".to_string(),
                            node.boundary_checkpoint_id
                                .as_deref()
                                .map(safe_display)
                                .unwrap_or_else(|| "none".to_string()),
                        ),
                        (
                            "parent_checkpoint_id".to_string(),
                            checkpoint
                                .and_then(|value| value.parent_checkpoint_id.as_deref())
                                .map(safe_display)
                                .unwrap_or_else(|| "none".to_string()),
                        ),
                        (
                            "created_at".to_string(),
                            checkpoint
                                .map(|value| value.created_at)
                                .unwrap_or(node.created_at)
                                .to_string(),
                        ),
                    ];
                    append_content_details(
                        &mut subagent_details,
                        checkpoint.map(|checkpoint| &checkpoint.content),
                    );
                    rows.push(TuiRow {
                        label: format!(
                            "  subagent {} [{}]",
                            safe_display(&node.checkpoint_id),
                            node.link_state
                        ),
                        details: subagent_details,
                    });
                }
            } else {
                rows.push(TuiRow {
                    label: "  subagents [unavailable]".to_string(),
                    details: vec![(
                        "unavailable_reason".to_string(),
                        projection
                            .output
                            .subagents
                            .unavailable_reason
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string()),
                    )],
                });
            }
        }

        let mut state = ListState::default();
        if !rows.is_empty() {
            state.select(Some(0));
        }
        Self { rows, state }
    }

    fn move_up(&mut self) {
        let selected = self.state.selected().unwrap_or(0);
        self.state.select(Some(selected.saturating_sub(1)));
    }

    fn move_down(&mut self) {
        let selected = self.state.selected().unwrap_or(0);
        let last = self.rows.len().saturating_sub(1);
        self.state.select(Some((selected + 1).min(last)));
    }
}

fn run_agent_graph_tui(projection: AgentGraphProjection) -> std::io::Result<()> {
    let terminal = tui_init()?;
    let _guard = scopeguard::guard((), |_| {
        let _ = tui_restore();
    });
    let mut tui = Tui::new(terminal);
    tui.enter_alt_screen()?;
    let mut app = AgentGraphTuiApp::new(&projection);

    loop {
        tui.draw(|frame| render_agent_graph(frame, &mut app))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::Home | KeyCode::Char('g') => app.state.select(Some(0)),
                KeyCode::End | KeyCode::Char('G') => {
                    app.state.select(Some(app.rows.len().saturating_sub(1)));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn render_agent_graph(frame: &mut Frame<'_>, app: &mut AgentGraphTuiApp) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(COLOR_BG)), area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(" Libra ", Style::default().fg(COLOR_ACCENT)),
        Span::styled("Agent Capture Graph", Style::default().fg(COLOR_FG)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(COLOR_BORDER))
            .style(Style::default().bg(COLOR_BG_PANEL)),
    );
    frame.render_widget(title, vertical[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(vertical[1]);
    render_tree_pane(frame, panes[0], app);
    render_detail_pane(frame, panes[1], app);

    let footer = Paragraph::new(" ↑/k ↓/j select   g/G first/last   q/Esc quit ")
        .style(Style::default().fg(COLOR_FG_MUTED).bg(COLOR_BG_PANEL));
    frame.render_widget(footer, vertical[2]);
}

fn render_tree_pane(frame: &mut Frame<'_>, area: Rect, app: &mut AgentGraphTuiApp) {
    let items = app
        .rows
        .iter()
        .map(|row| ListItem::new(safe_display(&row.label)))
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(
            Block::default()
                .title(" capture structure ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(COLOR_BORDER)),
        )
        .style(Style::default().fg(COLOR_FG).bg(COLOR_BG))
        .highlight_style(Style::default().fg(Color::White).bg(COLOR_BG_SEL))
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, area, &mut app.state);
}

fn render_detail_pane(frame: &mut Frame<'_>, area: Rect, app: &AgentGraphTuiApp) {
    let lines = app
        .state
        .selected()
        .and_then(|index| app.rows.get(index))
        .map(|row| {
            row.details
                .iter()
                .map(|(key, value)| {
                    Line::from(vec![
                        Span::styled(
                            format!("{}: ", safe_display(key)),
                            Style::default().fg(COLOR_FG_MUTED),
                        ),
                        Span::styled(safe_display(value), Style::default().fg(COLOR_FG)),
                    ])
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let details = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" details ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(COLOR_BORDER)),
        )
        .style(Style::default().bg(COLOR_BG))
        .wrap(Wrap { trim: false });
    frame.render_widget(details, area);
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn fixture_projection() -> AgentGraphProjection {
        let session = SessionOutput {
            session_id: "codex__fixture".to_string(),
            agent_kind: "codex".to_string(),
            state: "stopped".to_string(),
            created_at: 1,
            updated_at: 2,
        };
        AgentGraphProjection {
            output: AgentGraphOutput {
                schema_version: 1,
                state: "present".to_string(),
                session: Some(session),
                turns: vec![TurnOutput {
                    logical_turn_key: "turn:1".to_string(),
                    ordinal: 0,
                    coverage_schema_version: Some(1),
                    coverage_state: "indexed".to_string(),
                    completeness: Some("complete".to_string()),
                    current_revision: Some(2),
                    checkpoint_id: Some("checkpoint-current".to_string()),
                    source_channel: Some("live".to_string()),
                    revisions: vec![RevisionOutput {
                        revision: 1,
                        completeness: "incomplete".to_string(),
                        checkpoint_id: "checkpoint-shared".to_string(),
                        source_channel: "live".to_string(),
                        created_at: 1,
                    }],
                }],
                subagents: SubagentsOutput {
                    available: true,
                    unavailable_reason: None,
                    nodes: vec![SubagentNodeOutput {
                        checkpoint_id: "subagent-content".to_string(),
                        link_state: "unresolved".to_string(),
                        boundary_checkpoint_id: None,
                        created_at: 3,
                    }],
                },
            },
            checkpoints: BTreeMap::from([
                (
                    "checkpoint-current".to_string(),
                    CheckpointStructure {
                        checkpoint_id: "checkpoint-current".to_string(),
                        parent_checkpoint_id: None,
                        scope: "committed".to_string(),
                        created_at: 2,
                        content: CheckpointContentSummary {
                            availability: "available".to_string(),
                            event_count: 4,
                            user_message_count: 1,
                            assistant_message_count: 1,
                            user_preview: Some("show the captured turn".to_string()),
                            assistant_preview: Some("the turn was captured".to_string()),
                            ..CheckpointContentSummary::default()
                        },
                    },
                ),
                (
                    "subagent-content".to_string(),
                    CheckpointStructure {
                        checkpoint_id: "subagent-content".to_string(),
                        parent_checkpoint_id: None,
                        scope: "subagent".to_string(),
                        created_at: 3,
                        content: CheckpointContentSummary::default(),
                    },
                ),
            ]),
        }
    }

    #[test]
    fn agent_graph_renders_session_turn_revisions_and_subagents() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let mut app = AgentGraphTuiApp::new(&fixture_projection());
        terminal
            .draw(|frame| render_agent_graph(frame, &mut app))
            .expect("render capture graph");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Agent Capture Graph"));
        assert!(rendered.contains("codex__fixture"));
        assert!(rendered.contains("turn[0] turn:1"));
        assert!(rendered.contains("revision 1 incomplete"));
        assert!(rendered.contains("subagent-content"));
        assert!(rendered.contains("show the captured turn"));
        assert!(rendered.contains("the turn was captured"));
    }

    #[test]
    fn graph_content_preview_extracts_redacted_user_and_assistant_messages() {
        let transcript = br#"
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"inspect AKIAIOSFODNN7EXAMPLE"}]}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done\nwith details"}]}}
"#;
        let summary = summarize_transcript(transcript, false);
        assert_eq!(summary.event_count, 2);
        assert_eq!(summary.user_message_count, 1);
        assert_eq!(summary.assistant_message_count, 1);
        assert!(
            summary
                .user_preview
                .as_deref()
                .is_some_and(|value| value.contains("<REDACTED:"))
        );
        assert_eq!(
            summary.assistant_preview.as_deref(),
            Some("done with details")
        );
    }

    #[test]
    fn graph_source_has_no_capture_mutations_or_writer_dependencies() {
        let source = include_str!("graph.rs");
        for forbidden in [
            concat!("INSERT INTO ", "agent_"),
            concat!("UPDATE ", "agent_"),
            concat!("DELETE FROM ", "agent_"),
            concat!("agent_import", "::"),
            concat!("opencode_export", "::"),
            concat!("coverage_gate", "::reserve"),
            concat!("ai::history::", "HistoryManager"),
            concat!("projection::", "ProjectionResolver"),
            concat!("projection::", "ThreadBundle"),
        ] {
            assert!(
                !source.contains(forbidden),
                "capture graph must remain read-only and independent of `{forbidden}`"
            );
        }
    }
}
