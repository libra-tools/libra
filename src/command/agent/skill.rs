//! `libra agent skill` — discover and search captured skill events (A0-07).
//!
//! Skill events (E7 `SkillEvent`) are extracted from external-agent
//! transcripts and embedded, redacted, into each checkpoint's `metadata.json`
//! blob under the `extraction.skill_events` key. This command builds a
//! read-time [`SkillEventProjection`] over the `agent_checkpoint` catalog —
//! reading each checkpoint's metadata blob, projecting its skill events, and
//! answering queries by skill name, provider, session, and RFC3339 time range.
//! It also exposes the curated per-agent discovery registry
//! ([`discover_skills`]).
//!
//! No dedicated table backs this: the checkpoint metadata blob is the durable
//! source of truth and the projection is rebuilt on demand. A busy repo can
//! upgrade to a materialized `ai_index_skill_event` index later without a wire
//! change (the JSON schema is versioned).

use clap::{Args, Subcommand};
use sea_orm::{ConnectionTrait, Statement};
use serde::Serialize;

use super::checkpoint::{encode_page_cursor, load_metadata_blob, resolve_page_limit};
use crate::{
    internal::{
        ai::observed_agents::{
            AgentKind, DiscoveredSkill, IndexedSkillEvent, SKILL_PROJECTION_SCHEMA_VERSION,
            SkillEventProjection, SkillQuery, discover_skills,
        },
        db::get_db_conn_instance,
    },
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
    },
};

#[derive(Subcommand, Debug)]
pub enum SkillSubcommand {
    /// Search captured skill events by skill/provider/session/time range.
    #[command(about = "Search captured skill events")]
    Search(SkillSearchArgs),
    /// List captured skill events (alias for `search`, same filters).
    #[command(about = "List captured skill events (alias for search)")]
    List(SkillSearchArgs),
    /// Show the curated per-agent discoverable-skill registry.
    #[command(about = "Show the curated per-agent skill registry")]
    Registry(SkillRegistryArgs),
}

#[derive(Args, Debug)]
pub struct SkillSearchArgs {
    /// Only skill events whose skill name matches exactly (e.g. `/review`).
    #[arg(long, value_name = "NAME")]
    pub skill: Option<String>,
    /// Only skill events observed from this agent CLI slug (e.g. `codex`).
    #[arg(long, value_name = "SLUG")]
    pub provider: Option<String>,
    /// Only skill events from this session id.
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    /// Inclusive lower bound on the event timestamp (RFC3339).
    #[arg(long, value_name = "RFC3339")]
    pub since: Option<String>,
    /// Inclusive upper bound on the event timestamp (RFC3339).
    #[arg(long, value_name = "RFC3339")]
    pub until: Option<String>,
    /// Maximum rows to return (default 50, capped at 500).
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
    /// Keyset cursor from the previous page's `next_cursor` (opaque).
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
}

#[derive(Args, Debug)]
pub struct SkillRegistryArgs {
    /// Only the curated skills for this agent CLI slug.
    #[arg(long, value_name = "SLUG")]
    pub provider: Option<String>,
}

#[derive(Serialize)]
struct SkillSearchPage {
    schema_version: u32,
    skill_events: Vec<IndexedSkillEvent>,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
struct SkillRegistryPage {
    schema_version: u32,
    skills: Vec<DiscoveredSkill>,
}

pub async fn execute_safe(command: SkillSubcommand, output: &OutputConfig) -> CliResult<()> {
    match command {
        SkillSubcommand::Search(a) | SkillSubcommand::List(a) => search(a, output).await,
        SkillSubcommand::Registry(a) => registry(a, output).await,
    }
}

/// The parsed cursor for skill-event keyset pagination: `(timestamp_unix,
/// dedup_key)` where dedup_key is `"{session_id}:{event.id}"`.
fn decode_skill_cursor(raw: &str) -> CliResult<(i64, String)> {
    super::checkpoint::decode_page_cursor(raw)
}

fn dedup_key(row: &IndexedSkillEvent) -> String {
    format!("{}:{}", row.session_id, row.event.id)
}

/// Reject a non-RFC3339 `--since`/`--until` as a usage error instead of
/// letting the projection treat it as "no bound".
fn validate_rfc3339(value: &str, flag: &str) -> CliResult<()> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|e| {
            CliError::command_usage(format!(
                "{flag} must be an RFC3339 timestamp (e.g. 2026-07-09T00:00:00Z): {e}"
            ))
        })
}

/// Event timestamp as unix seconds for keyset ordering; unparseable stamps
/// sort last (0).
fn event_ts_unix(row: &IndexedSkillEvent) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&row.event.timestamp)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

async fn search(args: SkillSearchArgs, output: &OutputConfig) -> CliResult<()> {
    let (limit, clamp_note) = resolve_page_limit(args.limit);
    if let Some(note) = &clamp_note {
        eprintln!("{note}");
    }
    // Decode the cursor before touching the DB so a malformed value is a pure
    // usage error.
    let cursor = args
        .cursor
        .as_deref()
        .map(decode_skill_cursor)
        .transpose()?;

    // Validate the RFC3339 bounds up front: a typo in --since/--until must be
    // a usage error, never a silently-unbounded search (the projection filter
    // treats an unparseable bound as "no bound", which would over-return).
    if let Some(since) = &args.since {
        validate_rfc3339(since, "--since")?;
    }
    if let Some(until) = &args.until {
        validate_rfc3339(until, "--until")?;
    }

    let projection = build_projection(args.session.as_deref()).await?;
    let query = SkillQuery {
        skill: args.skill.clone(),
        provider: args.provider.clone(),
        session: args.session.clone(),
        since: args.since.clone(),
        until: args.until.clone(),
    };

    // Newest-first, deterministic total order: (timestamp DESC, key DESC).
    let mut matched: Vec<IndexedSkillEvent> =
        projection.search(&query).into_iter().cloned().collect();
    matched.sort_by(|a, b| {
        event_ts_unix(b)
            .cmp(&event_ts_unix(a))
            .then_with(|| dedup_key(b).cmp(&dedup_key(a)))
    });

    // Keyset: keep only rows strictly after the cursor position.
    if let Some((cursor_ts, cursor_key)) = &cursor {
        matched.retain(|row| {
            let ts = event_ts_unix(row);
            ts < *cursor_ts || (ts == *cursor_ts && &dedup_key(row) < cursor_key)
        });
    }

    let next_cursor = if matched.len() as u64 > limit {
        matched.truncate(limit as usize);
        matched
            .last()
            .map(|row| encode_page_cursor(event_ts_unix(row), &dedup_key(row)))
    } else {
        None
    };

    if output.is_json() {
        let page = SkillSearchPage {
            schema_version: SKILL_PROJECTION_SCHEMA_VERSION,
            skill_events: matched,
            next_cursor,
        };
        return emit_json_data("agent_skill_events", &page, output);
    }
    if output.quiet {
        return Ok(());
    }
    if matched.is_empty() {
        println!("no skill events matched");
        return Ok(());
    }
    for row in &matched {
        let checkpoint = row.checkpoint_id.as_deref().unwrap_or("-");
        println!(
            "{}  {}  session={}  checkpoint={}  {}",
            row.event.timestamp, row.event.skill.name, row.session_id, checkpoint, row.provider
        );
    }
    if next_cursor.is_some() {
        println!("(more — pass --cursor <next_cursor>; use --json to read it)");
    }
    Ok(())
}

/// Scan the `agent_checkpoint` catalog and project every checkpoint's
/// `extraction.skill_events` into an in-memory index. An optional session
/// pre-filter bounds the scan. Unreadable / non-JSON / skill-event-free
/// checkpoints are skipped (fail-open — a search never errors on one corrupt
/// checkpoint).
async fn build_projection(session: Option<&str>) -> CliResult<SkillEventProjection> {
    let mut projection = SkillEventProjection::new();
    let conn = get_db_conn_instance().await;
    if !super::checkpoint::table_exists(&conn, "agent_checkpoint").await? {
        return Ok(projection);
    }
    let backend = conn.get_database_backend();
    let (sql, values): (String, Vec<sea_orm::Value>) = if let Some(session) = session {
        (
            "SELECT checkpoint_id, session_id, metadata_blob_oid FROM agent_checkpoint \
             WHERE session_id = ? ORDER BY created_at DESC, checkpoint_id DESC"
                .to_string(),
            vec![session.into()],
        )
    } else {
        (
            "SELECT checkpoint_id, session_id, metadata_blob_oid FROM agent_checkpoint \
             ORDER BY created_at DESC, checkpoint_id DESC"
                .to_string(),
            Vec::new(),
        )
    };
    let rows = conn
        .query_all(Statement::from_sql_and_values(backend, &sql, values))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_checkpoint: {e}")))?;

    for row in rows {
        let checkpoint_id: String = row.try_get_by("checkpoint_id").unwrap_or_default();
        let session_id: String = row.try_get_by("session_id").unwrap_or_default();
        let metadata_blob_oid: String = row.try_get_by("metadata_blob_oid").unwrap_or_default();
        if metadata_blob_oid.is_empty() {
            continue;
        }
        let Ok(metadata) = load_metadata_blob(&metadata_blob_oid) else {
            continue;
        };
        let events = match parse_skill_events(&metadata) {
            Some(events) if !events.is_empty() => events,
            _ => continue,
        };
        // A checkpoint belongs to one agent, so all its events share a
        // provider slug (the event source agent).
        let provider = events
            .first()
            .map(|e| e.source.agent.clone())
            .unwrap_or_default();
        projection.ingest(&session_id, Some(&checkpoint_id), &provider, events);
    }
    Ok(projection)
}

/// Parse `extraction.skill_events` out of a checkpoint metadata JSON blob.
fn parse_skill_events(
    metadata: &str,
) -> Option<Vec<crate::internal::ai::observed_agents::SkillEvent>> {
    let value: serde_json::Value = serde_json::from_str(metadata).ok()?;
    let events = value.get("extraction")?.get("skill_events")?;
    serde_json::from_value(events.clone()).ok()
}

async fn registry(args: SkillRegistryArgs, output: &OutputConfig) -> CliResult<()> {
    let mut skills: Vec<DiscoveredSkill> = Vec::new();
    match &args.provider {
        Some(slug) => {
            let kind = AgentKind::from_cli_slug(slug).ok_or_else(|| {
                CliError::command_usage(format!(
                    "unknown agent '{slug}' (known: claude-code, codex, opencode, …)"
                ))
            })?;
            skills.extend(discover_skills(kind));
        }
        None => {
            for kind in AgentKind::all() {
                skills.extend(discover_skills(*kind));
            }
        }
    }

    if output.is_json() {
        let page = SkillRegistryPage {
            schema_version: SKILL_PROJECTION_SCHEMA_VERSION,
            skills,
        };
        return emit_json_data("agent_skill_registry", &page, output);
    }
    if output.quiet {
        return Ok(());
    }
    if skills.is_empty() {
        println!("no discoverable skills for the selected agent(s)");
        return Ok(());
    }
    for skill in &skills {
        println!("{}  {}", skill.provider, skill.name);
    }
    Ok(())
}
