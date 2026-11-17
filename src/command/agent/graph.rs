//! Read-only graph projection for captured external-agent sessions.
//!
//! This module deliberately does not reuse `command::graph`'s projection
//! resolver. External-agent capture is keyed by `agent_session.session_id`,
//! not by an orchestrator `thread_id`. Output is the frozen capture-graph
//! JSON v1 schema (`--json` / `--machine`); the interactive TUI entry was
//! removed in the W5 breaking release.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use clap::Args;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, QueryResult, Statement, TransactionTrait, TryGetable,
};
use serde::Serialize;

use crate::{
    internal::ai::{capture::store, observed_agents::CoverageSchemaVersion},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util::try_get_storage_path,
    },
};

pub const AGENT_GRAPH_EXAMPLES: &str = "\
EXAMPLES:
    libra --json agent graph <session>              Emit the frozen capture-graph JSON v1 schema
    libra --machine agent graph <session>           Emit compact machine-readable JSON
    libra --json agent graph <session> --repo /path/to/repo  Inspect capture data in another repository";

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
    created_at: i64,
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
    // Breaking change: the interactive capture-graph TUI is removed; the
    // frozen JSON/machine schema is the only output. Refuse before resolving
    // repository storage or loading the capture so the rejection is
    // deterministic and independent of repository/capture state. The CLI
    // preflight (`command_preflight`) likewise skips repo resolution for
    // non-JSON invocations.
    if !output.is_json() {
        return Err(CliError::command_usage(
            "`libra agent graph` no longer opens an interactive TUI",
        )
        .with_hint(
            "rerun as `libra --json agent graph <session>` or `libra --machine agent graph <session>` for structured output.",
        ));
    }

    let storage_root = try_get_storage_path(args.repo.clone()).map_err(|_| {
        CliError::repo_not_found()
            .with_hint("verify that --repo names an initialized Libra repository.")
    })?;

    let graph = load_agent_graph(&storage_root, &args.session).await?;
    emit_json_data("agent_graph", &graph, output)
}

async fn load_agent_graph(storage_root: &Path, session_id: &str) -> CliResult<AgentGraphOutput> {
    let connection = store::open(storage_root, std::time::Duration::from_secs(30))
        .await
        .map_err(|_| graph_store_error("open the repository capture catalog"))?;
    let transaction = connection
        .begin()
        .await
        .map_err(|_| graph_store_error("start a consistent capture-graph read"))?;

    let result = load_agent_graph_from_connection(&transaction, session_id).await;
    match result {
        Ok(graph) => {
            transaction
                .commit()
                .await
                .map_err(|_| graph_store_error("finish the capture-graph read"))?;
            Ok(graph)
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
) -> CliResult<AgentGraphOutput> {
    if tombstone_exists(connection, session_id).await? {
        return Ok(AgentGraphOutput {
            schema_version: 1,
            state: "erased".to_string(),
            session: None,
            turns: Vec::new(),
            subagents: SubagentsOutput {
                available: false,
                unavailable_reason: Some("erased".to_string()),
                nodes: Vec::new(),
            },
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
    let checkpoints = load_checkpoints(connection, session_id).await?;
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

    Ok(AgentGraphOutput {
        schema_version: 1,
        state: "present".to_string(),
        session: Some(session),
        turns,
        subagents,
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
) -> CliResult<BTreeMap<String, CheckpointStructure>> {
    let rows = connection
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT checkpoint_id, created_at \
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
            created_at: required(&row, "created_at")?,
        };
        checkpoints.insert(checkpoint.checkpoint_id.clone(), checkpoint);
    }
    Ok(checkpoints)
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
        let coverage_schema_version = coverage_version(&row)?;
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
            coverage_schema_version: coverage_version(&row)?,
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

fn coverage_version(row: &QueryResult) -> CliResult<i64> {
    let version: i64 = required(row, "coverage_schema_version")?;
    CoverageSchemaVersion::try_from(version)
        .map(CoverageSchemaVersion::as_i64)
        .map_err(|_| {
            graph_store_error("read an unsupported coverage schema version").with_hint(
                "use a Libra release that supports the stored coverage version; run `libra agent doctor` to inspect the catalog, and do not rewrite version numbers or delete evidence.",
            )
        })
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

#[cfg(test)]
mod tests {
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
