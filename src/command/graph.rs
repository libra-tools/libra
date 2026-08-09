//! Thread graph TUI for inspecting AI workflow version state.
//!
//! Thread structure (Intent/Plan/Task/Run/PatchSet) is sourced from
//! [`ThreadBundle`] projection indexes. Code-UI-equivalent transcript,
//! interaction, and status overlays must fold workflow events through
//! [`crate::internal::ai::web::code_ui::graph_code_ui_read_model_from_events`]
//! — the same bounded entry used by Code UI resume.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    prelude::{Color, Line, Modifier, Span, Style, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use unicode_width::UnicodeWidthChar;
use uuid::Uuid;

/// Ranking key for choosing the freshest Code session overlay candidate.
type GraphOverlaySessionRank = (SystemTime, DateTime<Utc>, String);

use crate::{
    internal::{
        ai::{
            history::HistoryManager,
            projection::{ProjectionRebuilder, ProjectionResolver, ThreadBundle},
            session::{SessionJsonlStore, SessionStore},
            web::code_ui::{
                CodeUiCapabilities, CodeUiInteractionStatus, CodeUiProviderInfo,
                CodeUiSessionSnapshot, CodeUiSessionStatus, graph_code_ui_read_model_from_events,
                initial_snapshot,
            },
        },
        db::establish_connection,
        model::{
            ai_index_intent_plan, ai_index_intent_task, ai_index_plan_step_task,
            ai_index_run_event, ai_index_run_patchset, ai_index_task_run, ai_thread_intent,
        },
        tui::{Tui, tui_init, tui_restore},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        storage::{Storage, local::LocalStorage},
        util::{DATABASE, try_get_storage_path},
    },
};

const MAX_OBJECT_DETAIL_LINES: usize = 160;
const MAX_OBJECT_DETAIL_LINE_CHARS: usize = 240;
const MIN_DETAIL_VALUE_WIDTH: usize = 12;
const TREE_PANE_WEIGHT: u16 = 11;
const LIST_PANE_WEIGHT: u16 = 10;
const DETAIL_PANE_WEIGHT: u16 = 14;

// ── GitHub Dark theme palette (matches the design bundle) ────────────────────
const COLOR_BG: Color = Color::Rgb(13, 17, 23); //          #0d1117
const COLOR_BG_PANEL: Color = Color::Rgb(1, 4, 9); //       #010409 (top/help bars)
const COLOR_BG_SEL: Color = Color::Rgb(31, 111, 235); //    #1f6feb (focused selection)
const COLOR_BG_SEL_DIM: Color = Color::Rgb(33, 38, 45); //  #21262d (unfocused selection)
const COLOR_FG: Color = Color::Rgb(201, 209, 217); //       #c9d1d9
const COLOR_FG_MUTED: Color = Color::Rgb(110, 118, 129); // #6e7681
const COLOR_BORDER: Color = Color::Rgb(48, 54, 61); //      #30363d
const COLOR_ACCENT: Color = Color::Rgb(88, 166, 255); //    #58a6ff
const COLOR_BRAND: Color = Color::Rgb(255, 123, 114); //    #ff7b72
const COLOR_HINT: Color = Color::Rgb(124, 133, 144); //     #7d8590
const COLOR_KEYCAP_BG: Color = Color::Rgb(33, 38, 45); //   #21262d
const COLOR_SEL_FG: Color = Color::Rgb(255, 255, 255);
const COLOR_SEL_HINT: Color = Color::Rgb(188, 217, 255); // #bcd9ff

// ── k9s-flavoured status palette (glyph + tint + 5-char label) ───────────────
#[derive(Debug, Clone, Copy)]
struct StatusInfo {
    glyph: &'static str,
    color: Color,
    label: &'static str,
}

const STATUS_SUCCEEDED: StatusInfo = StatusInfo {
    glyph: "●",
    color: Color::Rgb(126, 231, 135),
    label: "OK",
};
const STATUS_RUNNING: StatusInfo = StatusInfo {
    glyph: "◐",
    color: Color::Rgb(88, 166, 255),
    label: "RUN",
};
const STATUS_QUEUED: StatusInfo = StatusInfo {
    glyph: "○",
    color: Color::Rgb(139, 148, 158),
    label: "WAIT",
};
const STATUS_BLOCKED: StatusInfo = StatusInfo {
    glyph: "▲",
    color: Color::Rgb(240, 136, 62),
    label: "BLOCK",
};
const STATUS_FAILED: StatusInfo = StatusInfo {
    glyph: "✖",
    color: Color::Rgb(255, 123, 114),
    label: "FAIL",
};
const STATUS_NEUTRAL: StatusInfo = StatusInfo {
    glyph: "·",
    color: Color::Rgb(139, 148, 158),
    label: "—",
};

fn status_for_event_kind(event_kind: &str) -> Option<StatusInfo> {
    match event_kind.to_ascii_lowercase().as_str() {
        "completed" | "succeeded" | "success" | "ok" => Some(STATUS_SUCCEEDED),
        "failed" | "errored" | "error" | "fail" => Some(STATUS_FAILED),
        "started" | "running" | "in_progress" | "in-progress" => Some(STATUS_RUNNING),
        "blocked" | "stalled" => Some(STATUS_BLOCKED),
        "queued" | "pending" | "waiting" => Some(STATUS_QUEUED),
        _ => None,
    }
}

/// `--help` examples shown in `libra graph --help` output.
///
/// `graph` renders the version-graph for a canonical Libra Thread ID
/// (UUID). The banner pins the default invocation, the `--repo` override
/// for running outside the current repository, and a JSON variant for
/// agents so users see all supported forms without reading the design
/// doc. Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
pub const GRAPH_EXAMPLES: &str = "\
EXAMPLES:
    libra graph <thread-uuid>                          Render the version-graph for a thread ID
    libra graph <thread-uuid> --repo /path/to/repo     Inspect a graph in another Libra repository
    libra graph --json <thread-uuid>                   Structured JSON output for agents";

/// Command-line arguments for `libra graph`.
#[derive(Parser, Debug)]
#[command(after_help = GRAPH_EXAMPLES)]
pub struct GraphArgs {
    /// Canonical Libra Thread UUID to inspect
    #[arg(value_name = "THREAD_UUID")]
    pub thread_id: String,

    /// Path to a Libra repository to inspect (default: discover from current directory)
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,
}

/// Execute `libra graph`.
pub async fn execute_safe(args: GraphArgs, output: &OutputConfig) -> CliResult<()> {
    let requested_thread_id = Uuid::parse_str(&args.thread_id).map_err(|error| {
        CliError::command_usage(format!(
            "graph expects a canonical thread_id UUID (got '{}': {error})",
            args.thread_id
        ))
    })?;

    let storage_root = try_get_storage_path(args.repo.clone()).map_err(|error| {
        CliError::repo_not_found()
            .with_hint(format!("failed to resolve repository storage: {error}"))
    })?;

    let graph = load_thread_graph(&storage_root, requested_thread_id)
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to load thread graph for '{}': {error:#}",
                args.thread_id
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
            .with_hint("run `libra code` first so the thread projection can be recorded.")
        })?;

    // `--json` / `--machine` emit the graph as structured data instead of the
    // interactive TUI — the agent-friendly path (the TUI needs a terminal).
    if output.is_json() {
        return emit_json_data("graph", &graph.to_json(), output);
    }

    run_graph_tui(graph).map_err(|error| {
        CliError::io(format!("failed to run graph TUI: {error}"))
            .with_hint("run this command from an interactive terminal.")
    })?;

    Ok(())
}

async fn load_thread_graph(storage_root: &Path, requested_thread_id: Uuid) -> Result<ThreadGraph> {
    let db_path = storage_root.join(DATABASE);
    let db_path_str = db_path.to_str().ok_or_else(|| {
        anyhow::anyhow!("database path is not valid UTF-8: {}", db_path.display())
    })?;
    let db_conn = establish_connection(db_path_str)
        .await
        .with_context(|| format!("failed to open repository database '{}'", db_path.display()))?;
    let storage = std::sync::Arc::new(LocalStorage::new(storage_root.join("objects")));
    let history = HistoryManager::new(
        storage.clone(),
        storage_root.to_path_buf(),
        std::sync::Arc::new(db_conn.clone()),
    );
    let rebuilder = ProjectionRebuilder::new(storage.as_ref(), &history);
    let resolver = ProjectionResolver::new(db_conn.clone());

    let bundle =
        load_bundle_for_graph(&db_conn, &resolver, &rebuilder, requested_thread_id).await?;
    let rows = load_projection_index_rows(&db_conn, &bundle).await?;
    let object_details =
        load_graph_object_details(&history, storage.as_ref(), &bundle, &rows).await;
    let mut graph = ThreadGraph::from_projection(bundle, rows, object_details);
    // Overlay lookup must use the resolved canonical thread id: callers may
    // pass an intent UUID that projection remaps to its owning thread.
    if let Some((status, transcript_len, pending_interactions)) =
        load_code_ui_overlay_for_thread(storage_root, graph.thread_id)?
    {
        graph = graph.with_code_ui_overlay(status, transcript_len, pending_interactions);
    }
    Ok(graph)
}

/// Fold the session workflow log into Code-UI-equivalent status/transcript
/// overlays for graph JSON/TUI. Missing sessions are non-fatal; unreadable or
/// malformed workflow logs for a session that matches the selected thread are
/// hard errors so operators cannot miss a fence.
///
/// Candidate sessions come from the durable thread→session index maintained by
/// [`SessionStore::save`] / [`SessionStore::session_ids_for_thread`], so graph
/// does not synchronously scan every historical session log.
fn load_code_ui_overlay_for_thread(
    storage_root: &Path,
    thread_id: Uuid,
) -> Result<Option<(CodeUiSessionStatus, usize, usize)>> {
    let session_store = SessionStore::from_storage_path(storage_root);
    let thread_id = thread_id.to_string();
    let candidate_ids = session_store.session_ids_for_thread(&thread_id).with_context(|| {
        format!("failed to resolve Code sessions for thread '{thread_id}' while building graph overlay")
    })?;

    let mut latest: Option<(
        crate::internal::ai::session::SessionState,
        GraphOverlaySessionRank,
    )> = None;
    for session_id in candidate_ids {
        let session = match session_store.load(&session_id) {
            Ok(session) => session,
            Err(error)
                if session_id == thread_id && error.kind() == std::io::ErrorKind::NotFound =>
            {
                // Canonical thread ids often differ from Code session ids.
                continue;
            }
            Err(error) if session_id == thread_id => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to load Code session '{session_id}' while building graph overlay for thread '{thread_id}'"
                    )
                });
            }
            Err(error) => {
                // Indexed candidates must fail closed — skipping could hide a fence.
                return Err(error).with_context(|| {
                    format!(
                        "failed to load indexed Code session '{session_id}' while building graph overlay for thread '{thread_id}'"
                    )
                });
            }
        };
        if !session_matches_thread_for_graph(&session, &thread_id) {
            continue;
        }
        let events_path =
            SessionJsonlStore::new(session_store.session_root(&session.id)).events_path();
        let modified = std::fs::metadata(&events_path)
            .and_then(|meta| meta.modified())
            .or_else(|_| {
                session_store
                    .session_root(&session.id)
                    .metadata()
                    .and_then(|meta| meta.modified())
            })
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let rank = (modified, session.updated_at, session.id.clone());
        if latest
            .as_ref()
            .is_none_or(|(_, best_rank)| rank > *best_rank)
        {
            latest = Some((session, rank));
        }
    }
    let Some((session, _)) = latest else {
        return Ok(None);
    };

    let provider = CodeUiProviderInfo {
        provider: "graph".to_string(),
        model: None,
        mode: Some("graph-fold".to_string()),
        managed: false,
    };
    let capabilities = CodeUiCapabilities {
        message_input: false,
        streaming_text: false,
        plan_updates: true,
        tool_calls: true,
        patchsets: true,
        interactive_approvals: false,
        structured_questions: false,
        provider_session_resume: false,
        command_idempotency: false,
    };
    let projection_cursor = session
        .metadata
        .get("code_ui_projection_cursor")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let mut bootstrap = session
        .metadata
        .get("code_ui_snapshot")
        .and_then(|value| serde_json::from_value::<CodeUiSessionSnapshot>(value.clone()).ok())
        .unwrap_or_else(|| {
            initial_snapshot(
                session.working_dir.clone(),
                provider.clone(),
                capabilities.clone(),
            )
        });
    bootstrap.session_id = session.id.clone();
    bootstrap.thread_id = Some(thread_id.clone());
    bootstrap.working_dir = session.working_dir.clone();
    bootstrap.provider = provider;
    bootstrap.capabilities = capabilities;
    let has_pending_interaction = bootstrap
        .interactions
        .iter()
        .any(|interaction| matches!(interaction.status, CodeUiInteractionStatus::Pending));
    if bootstrap.status != CodeUiSessionStatus::IndeterminateSideEffect
        && !matches!(
            bootstrap.status,
            CodeUiSessionStatus::Thinking
                | CodeUiSessionStatus::ExecutingTool
                | CodeUiSessionStatus::AwaitingInteraction
        )
    {
        bootstrap.status = if has_pending_interaction {
            CodeUiSessionStatus::AwaitingInteraction
        } else {
            CodeUiSessionStatus::Idle
        };
    } else if bootstrap.status != CodeUiSessionStatus::IndeterminateSideEffect
        && has_pending_interaction
    {
        bootstrap.status = CodeUiSessionStatus::AwaitingInteraction;
    }

    let replay = SessionJsonlStore::new(session_store.session_root(&session.id))
        .load_code_workflow_replay_since(
            projection_cursor,
            crate::internal::ai::web::code_ui_projection::MAX_CODE_UI_PROJECTION_EVENTS,
            crate::internal::ai::web::code_ui_projection::MAX_CODE_UI_PROJECTION_REPLAY_BYTES,
        )
        .with_context(|| {
            format!(
                "failed to load Code workflow events for session '{}' while building graph overlay",
                session.id
            )
        })?;
    if !replay.gaps.is_empty() {
        bail!(
            "Code workflow log for session '{}' exceeds the bounded graph overlay window after cursor {projection_cursor}; refusing a partial fold that could hide reconciliation state",
            session.id
        );
    }
    let workflow = SessionJsonlStore::new(session_store.session_root(&session.id));
    let unresolved_mutation = workflow
        .has_unresolved_mutating_reconciliation_bounded(
            crate::internal::ai::web::code_ui_projection::MAX_CODE_UI_PROJECTION_REPLAY_BYTES,
        )
        .with_context(|| {
            format!(
                "failed to inspect durable mutating commands for session '{}' while building graph overlay",
                session.id
            )
        })?;
    if unresolved_mutation {
        // Pending mutations are invisible to the projection fold until a
        // runtime fences them. Graph is an inspection path before restart, so
        // fail closed to the reconciliation status instead of emitting idle.
        bootstrap.status = CodeUiSessionStatus::IndeterminateSideEffect;
    }
    let fold = graph_code_ui_read_model_from_events(bootstrap, &replay).map_err(|error| {
        anyhow::anyhow!(
            "failed to fold Code UI overlay for session '{}': {error}",
            session.id
        )
    })?;
    let status = if unresolved_mutation {
        CodeUiSessionStatus::IndeterminateSideEffect
    } else {
        fold.snapshot.status
    };
    let pending = fold
        .snapshot
        .interactions
        .iter()
        .filter(|interaction| matches!(interaction.status, CodeUiInteractionStatus::Pending))
        .count();
    Ok(Some((status, fold.snapshot.transcript.len(), pending)))
}

fn session_matches_thread_for_graph(
    session: &crate::internal::ai::session::SessionState,
    thread_id: &str,
) -> bool {
    session.id == thread_id
        || session
            .metadata
            .get("thread_id")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value == thread_id)
        || session
            .metadata
            .get("canonical_thread_id")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value == thread_id)
        || session
            .metadata
            .get("threadId")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value == thread_id)
}

async fn load_bundle_for_graph(
    db_conn: &DatabaseConnection,
    resolver: &ProjectionResolver,
    rebuilder: &ProjectionRebuilder<'_>,
    requested_thread_id: Uuid,
) -> Result<ThreadBundle> {
    if let Some(bundle) = resolver
        .load_or_rebuild_thread_bundle(requested_thread_id, rebuilder)
        .await
        .with_context(|| format!("failed to load projection for thread {requested_thread_id}"))?
    {
        return Ok(bundle);
    }

    if let Some(thread_id) =
        resolve_thread_id_from_intent_index(db_conn, requested_thread_id).await?
        && let Some(bundle) = resolver
            .load_or_rebuild_thread_bundle(thread_id, rebuilder)
            .await
            .with_context(|| {
                format!("failed to load projection for thread {thread_id} from intent index")
            })?
    {
        return Ok(bundle);
    }

    if let Some(rebuild) = rebuilder
        .materialize_latest_thread(db_conn)
        .await
        .context("failed to rebuild latest AI thread projection")?
        && (rebuild.thread.thread_id == requested_thread_id
            || rebuild
                .thread
                .intents
                .iter()
                .any(|intent| intent.intent_id == requested_thread_id))
        && let Some(bundle) = resolver
            .load_thread_bundle(rebuild.thread.thread_id)
            .await
            .with_context(|| {
                format!(
                    "failed to load rebuilt projection for thread {}",
                    rebuild.thread.thread_id
                )
            })?
    {
        return Ok(bundle);
    }

    bail!(
        "no thread projection or AI history was found for '{}'",
        requested_thread_id
    )
}

async fn resolve_thread_id_from_intent_index(
    db_conn: &DatabaseConnection,
    intent_id: Uuid,
) -> Result<Option<Uuid>> {
    let Some(row) = ai_thread_intent::Entity::find()
        .filter(ai_thread_intent::Column::IntentId.eq(intent_id.to_string()))
        .one(db_conn)
        .await
        .with_context(|| format!("failed to query thread membership for intent {intent_id}"))?
    else {
        return Ok(None);
    };

    Uuid::parse_str(&row.thread_id)
        .map(Some)
        .with_context(|| format!("invalid thread_id '{}' in ai_thread_intent", row.thread_id))
}

#[derive(Debug, Clone, Default)]
struct ProjectionIndexRows {
    intent_plans: Vec<ai_index_intent_plan::Model>,
    intent_tasks: Vec<ai_index_intent_task::Model>,
    plan_tasks: Vec<ai_index_plan_step_task::Model>,
    task_runs: Vec<ai_index_task_run::Model>,
    run_events: Vec<ai_index_run_event::Model>,
    run_patchsets: Vec<ai_index_run_patchset::Model>,
}

async fn load_projection_index_rows(
    db_conn: &DatabaseConnection,
    bundle: &ThreadBundle,
) -> Result<ProjectionIndexRows> {
    let intent_ids = bundle
        .thread
        .intents
        .iter()
        .map(|intent| intent.intent_id.to_string())
        .collect::<Vec<_>>();
    if intent_ids.is_empty() {
        return Ok(ProjectionIndexRows::default());
    }

    let intent_plans = ai_index_intent_plan::Entity::find()
        .filter(ai_index_intent_plan::Column::IntentId.is_in(intent_ids.clone()))
        .order_by_asc(ai_index_intent_plan::Column::CreatedAt)
        .all(db_conn)
        .await
        .context("failed to load intent -> plan index rows")?;
    let intent_tasks = ai_index_intent_task::Entity::find()
        .filter(ai_index_intent_task::Column::IntentId.is_in(intent_ids))
        .order_by_asc(ai_index_intent_task::Column::CreatedAt)
        .all(db_conn)
        .await
        .context("failed to load intent -> task index rows")?;

    let plan_ids = intent_plans
        .iter()
        .map(|row| row.plan_id.clone())
        .collect::<Vec<_>>();
    let plan_tasks = if plan_ids.is_empty() {
        Vec::new()
    } else {
        ai_index_plan_step_task::Entity::find()
            .filter(ai_index_plan_step_task::Column::PlanId.is_in(plan_ids))
            .order_by_asc(ai_index_plan_step_task::Column::CreatedAt)
            .all(db_conn)
            .await
            .context("failed to load plan step -> task index rows")?
    };

    let task_ids = intent_tasks
        .iter()
        .map(|row| row.task_id.clone())
        .chain(plan_tasks.iter().map(|row| row.task_id.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let task_runs = if task_ids.is_empty() {
        Vec::new()
    } else {
        ai_index_task_run::Entity::find()
            .filter(ai_index_task_run::Column::TaskId.is_in(task_ids))
            .order_by_asc(ai_index_task_run::Column::CreatedAt)
            .all(db_conn)
            .await
            .context("failed to load task -> run index rows")?
    };

    let run_ids = task_runs
        .iter()
        .map(|row| row.run_id.clone())
        .collect::<Vec<_>>();
    let run_events = if run_ids.is_empty() {
        Vec::new()
    } else {
        ai_index_run_event::Entity::find()
            .filter(ai_index_run_event::Column::RunId.is_in(run_ids.clone()))
            .order_by_asc(ai_index_run_event::Column::CreatedAt)
            .all(db_conn)
            .await
            .context("failed to load run -> event index rows")?
    };
    let run_patchsets = if run_ids.is_empty() {
        Vec::new()
    } else {
        ai_index_run_patchset::Entity::find()
            .filter(ai_index_run_patchset::Column::RunId.is_in(run_ids))
            .order_by_asc(ai_index_run_patchset::Column::Sequence)
            .all(db_conn)
            .await
            .context("failed to load run -> patchset index rows")?
    };

    Ok(ProjectionIndexRows {
        intent_plans,
        intent_tasks,
        plan_tasks,
        task_runs,
        run_events,
        run_patchsets,
    })
}

#[derive(Debug, Clone)]
struct ThreadGraph {
    thread_id: Uuid,
    title: Option<String>,
    freshness: String,
    thread_version: i64,
    scheduler_version: i64,
    updated_at: DateTime<Utc>,
    selected_plan_id: Option<Uuid>,
    active_task_id: Option<Uuid>,
    active_run_id: Option<Uuid>,
    /// Code-UI-equivalent session status folded from the session workflow log
    /// (for example `indeterminate_side_effect` after mutation recovery).
    code_ui_status: Option<String>,
    code_ui_transcript_len: usize,
    code_ui_pending_interactions: usize,
    lines: Vec<GraphLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphLine {
    depth: usize,
    kind: GraphNodeKind,
    id: String,
    label: String,
    tags: Vec<String>,
    detail: Vec<(String, String)>,
    object: Option<GraphObjectDetail>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum GraphNodeKind {
    Intent,
    Plan,
    Task,
    Run,
    Patchset,
}

impl GraphNodeKind {
    fn label(self) -> &'static str {
        match self {
            Self::Intent => "Intent",
            Self::Plan => "Plan",
            Self::Task => "Task",
            Self::Run => "Run",
            Self::Patchset => "PatchSet",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Intent => Color::Rgb(255, 166, 87),   //   #ffa657
            Self::Plan => Color::Rgb(121, 192, 255),    //    #79c0ff
            Self::Task => Color::Rgb(210, 168, 255),    //    #d2a8ff
            Self::Run => Color::Rgb(126, 231, 135),     //     #7ee787
            Self::Patchset => Color::Rgb(88, 166, 255), // #58a6ff
        }
    }

    fn history_type(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Plan => "plan",
            Self::Task => "task",
            Self::Run => "run",
            Self::Patchset => "patchset",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphObjectDetail {
    object_type: String,
    hash: Option<String>,
    git_object_type: Option<String>,
    summary: Vec<(String, String)>,
    raw_json_lines: Vec<String>,
}

impl GraphObjectDetail {
    fn from_json(
        kind: GraphNodeKind,
        hash: Option<String>,
        git_object_type: Option<String>,
        value: serde_json::Value,
    ) -> Self {
        Self {
            object_type: kind.history_type().to_string(),
            hash,
            git_object_type,
            summary: summarize_object_fields(kind, &value),
            raw_json_lines: pretty_json_lines(&value),
        }
    }

    fn unavailable(kind: GraphNodeKind, reason: impl Into<String>) -> Self {
        Self {
            object_type: kind.history_type().to_string(),
            hash: None,
            git_object_type: None,
            summary: vec![
                ("object_status".to_string(), "unavailable".to_string()),
                ("reason".to_string(), reason.into()),
            ],
            raw_json_lines: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct GraphObjectDetails {
    by_node: BTreeMap<(GraphNodeKind, String), GraphObjectDetail>,
}

impl GraphObjectDetails {
    fn get(&self, kind: GraphNodeKind, id: &str) -> Option<GraphObjectDetail> {
        self.by_node.get(&(kind, id.to_string())).cloned()
    }

    fn insert(&mut self, kind: GraphNodeKind, id: String, detail: GraphObjectDetail) {
        self.by_node.insert((kind, id), detail);
    }
}

impl ThreadGraph {
    /// Build a structured JSON representation of the graph for `--json` /
    /// `--machine` output (the agent-friendly alternative to the TUI). Each
    /// `GraphLine` becomes a node with its kind, hierarchy depth, label, tags,
    /// key/value detail, and (when present) the underlying object's summary.
    fn to_json(&self) -> serde_json::Value {
        use serde_json::{Map, Value, json};

        let pairs_to_object = |pairs: &[(String, String)]| -> Value {
            let map: Map<String, Value> = pairs
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect();
            Value::Object(map)
        };

        let nodes: Vec<Value> = self
            .lines
            .iter()
            .map(|line| {
                let object = line.object.as_ref().map(|object| {
                    json!({
                        "object_type": object.object_type,
                        "hash": object.hash,
                        "git_object_type": object.git_object_type,
                        "summary": pairs_to_object(&object.summary),
                    })
                });
                json!({
                    "depth": line.depth,
                    "kind": line.kind.history_type(),
                    "id": line.id,
                    "label": line.label,
                    "tags": line.tags,
                    "detail": pairs_to_object(&line.detail),
                    "object": object,
                })
            })
            .collect();

        json!({
            "thread_id": self.thread_id.to_string(),
            "title": self.title,
            "freshness": self.freshness,
            "thread_version": self.thread_version,
            "scheduler_version": self.scheduler_version,
            "updated_at": self.updated_at.to_rfc3339(),
            "selected_plan_id": self.selected_plan_id.map(|id| id.to_string()),
            "active_task_id": self.active_task_id.map(|id| id.to_string()),
            "active_run_id": self.active_run_id.map(|id| id.to_string()),
            "code_ui_status": self.code_ui_status,
            "code_ui_transcript_len": self.code_ui_transcript_len,
            "code_ui_pending_interactions": self.code_ui_pending_interactions,
            "nodes": nodes,
        })
    }

    fn from_projection(
        bundle: ThreadBundle,
        rows: ProjectionIndexRows,
        object_details: GraphObjectDetails,
    ) -> Self {
        let mut graph_rows = Vec::new();

        let selected_plan_ids = bundle
            .scheduler
            .selected_plan_ids
            .iter()
            .map(|plan| plan.plan_id.to_string())
            .collect::<BTreeSet<_>>();
        let head_plan_ids = bundle
            .scheduler
            .current_plan_heads
            .iter()
            .map(|plan| plan.plan_id.to_string())
            .collect::<BTreeSet<_>>();

        let plans_by_intent = group_values_by_key(rows.intent_plans.iter().map(|row| {
            (
                row.intent_id.clone(),
                TimedValue {
                    value: row.plan_id.clone(),
                    sort: row.created_at,
                },
            )
        }));
        let tasks_by_intent = group_values_by_key(rows.intent_tasks.iter().map(|row| {
            (
                row.intent_id.clone(),
                TimedValue {
                    value: row.task_id.clone(),
                    sort: row.created_at,
                },
            )
        }));
        let tasks_by_plan = group_values_by_key(rows.plan_tasks.iter().map(|row| {
            (
                row.plan_id.clone(),
                TimedValue {
                    value: row.task_id.clone(),
                    sort: row.created_at,
                },
            )
        }));
        let runs_by_task = group_values_by_key(rows.task_runs.iter().map(|row| {
            (
                row.task_id.clone(),
                TimedValue {
                    value: row.run_id.clone(),
                    sort: row.created_at,
                },
            )
        }));
        let patchsets_by_run = group_values_by_key(rows.run_patchsets.iter().map(|row| {
            (
                row.run_id.clone(),
                TimedValue {
                    value: row.patchset_id.clone(),
                    sort: row.sequence,
                },
            )
        }));
        let latest_run_events = rows
            .run_events
            .iter()
            .filter(|row| row.is_latest)
            .map(|row| (row.run_id.clone(), row.event_kind.clone()))
            .collect::<BTreeMap<_, _>>();
        let latest_patchsets = rows
            .run_patchsets
            .iter()
            .filter(|row| row.is_latest)
            .map(|row| row.patchset_id.clone())
            .collect::<BTreeSet<_>>();
        let latest_runs = rows
            .task_runs
            .iter()
            .filter(|row| row.is_latest)
            .map(|row| row.run_id.clone())
            .collect::<BTreeSet<_>>();

        let mut intents = bundle.thread.intents.clone();
        intents.sort_by_key(|intent| intent.ordinal);
        for intent in intents {
            let intent_id = intent.intent_id.to_string();
            let mut tags = vec![format!("{:?}", intent.link_reason)];
            if intent.is_head {
                tags.push("head".to_string());
            }
            if bundle.thread.current_intent_id == Some(intent.intent_id) {
                tags.push("current".to_string());
            }
            if bundle.thread.latest_intent_id == Some(intent.intent_id) {
                tags.push("latest".to_string());
            }

            graph_rows.push(GraphLine {
                depth: 0,
                kind: GraphNodeKind::Intent,
                id: intent_id.clone(),
                label: format!("#{} {}", intent.ordinal, short_id(&intent_id)),
                tags,
                detail: vec![
                    ("intent_id".to_string(), intent_id.clone()),
                    ("ordinal".to_string(), intent.ordinal.to_string()),
                    (
                        "link_reason".to_string(),
                        format!("{:?}", intent.link_reason),
                    ),
                    ("is_head".to_string(), intent.is_head.to_string()),
                    ("linked_at".to_string(), format_timestamp(intent.linked_at)),
                ],
                object: object_details.get(GraphNodeKind::Intent, &intent_id),
            });

            let mut displayed_tasks = BTreeSet::new();
            for plan_id in plans_by_intent.get(&intent_id).cloned().unwrap_or_default() {
                let mut plan_tags = Vec::new();
                if selected_plan_ids.contains(&plan_id) {
                    plan_tags.push("selected".to_string());
                }
                if head_plan_ids.contains(&plan_id) {
                    plan_tags.push("head".to_string());
                }

                graph_rows.push(GraphLine {
                    depth: 1,
                    kind: GraphNodeKind::Plan,
                    id: plan_id.clone(),
                    label: short_id(&plan_id),
                    tags: plan_tags,
                    detail: vec![
                        ("plan_id".to_string(), plan_id.clone()),
                        (
                            "selected".to_string(),
                            selected_plan_ids.contains(&plan_id).to_string(),
                        ),
                        (
                            "plan_head".to_string(),
                            head_plan_ids.contains(&plan_id).to_string(),
                        ),
                    ],
                    object: object_details.get(GraphNodeKind::Plan, &plan_id),
                });

                for task_id in tasks_by_plan.get(&plan_id).cloned().unwrap_or_default() {
                    displayed_tasks.insert(task_id.clone());
                    push_task_subgraph(
                        &mut graph_rows,
                        &task_id,
                        2,
                        &runs_by_task,
                        &patchsets_by_run,
                        &latest_runs,
                        &latest_run_events,
                        &latest_patchsets,
                        bundle.scheduler.active_task_id,
                        bundle.scheduler.active_run_id,
                        &object_details,
                    );
                }
            }

            for task_id in tasks_by_intent.get(&intent_id).cloned().unwrap_or_default() {
                if displayed_tasks.insert(task_id.clone()) {
                    push_task_subgraph(
                        &mut graph_rows,
                        &task_id,
                        1,
                        &runs_by_task,
                        &patchsets_by_run,
                        &latest_runs,
                        &latest_run_events,
                        &latest_patchsets,
                        bundle.scheduler.active_task_id,
                        bundle.scheduler.active_run_id,
                        &object_details,
                    );
                }
            }
        }

        ThreadGraph {
            thread_id: bundle.thread.thread_id,
            title: bundle.thread.title,
            freshness: format!("{:?}", bundle.freshness),
            thread_version: bundle.thread.version,
            scheduler_version: bundle.scheduler.version,
            updated_at: bundle.thread.updated_at.max(bundle.scheduler.updated_at),
            selected_plan_id: bundle.scheduler.selected_plan_id,
            active_task_id: bundle.scheduler.active_task_id,
            active_run_id: bundle.scheduler.active_run_id,
            code_ui_status: None,
            code_ui_transcript_len: 0,
            code_ui_pending_interactions: 0,
            lines: graph_rows,
        }
    }

    fn with_code_ui_overlay(
        mut self,
        status: CodeUiSessionStatus,
        transcript_len: usize,
        pending_interactions: usize,
    ) -> Self {
        self.code_ui_status = serde_json::to_value(status)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned));
        self.code_ui_transcript_len = transcript_len;
        self.code_ui_pending_interactions = pending_interactions;
        self
    }
}

async fn load_graph_object_details<S>(
    history: &HistoryManager,
    storage: &S,
    bundle: &ThreadBundle,
    rows: &ProjectionIndexRows,
) -> GraphObjectDetails
where
    S: Storage + ?Sized,
{
    let mut details = GraphObjectDetails::default();
    for (kind, id) in graph_object_refs(bundle, rows) {
        let detail = load_graph_object_detail(history, storage, kind, &id).await;
        details.insert(kind, id, detail);
    }
    details
}

async fn load_graph_object_detail<S>(
    history: &HistoryManager,
    storage: &S,
    kind: GraphNodeKind,
    object_id: &str,
) -> GraphObjectDetail
where
    S: Storage + ?Sized,
{
    let hash = match history
        .get_object_hash(kind.history_type(), object_id)
        .await
    {
        Ok(Some(hash)) => hash,
        Ok(None) => {
            return GraphObjectDetail::unavailable(
                kind,
                format!("{} object was not found in AI history", kind.history_type()),
            );
        }
        Err(error) => {
            return GraphObjectDetail::unavailable(
                kind,
                format!("failed to look up object in AI history: {error:#}"),
            );
        }
    };

    let (data, git_object_type) = match storage.get(&hash).await {
        Ok(found) => found,
        Err(error) => {
            return GraphObjectDetail::unavailable(
                kind,
                format!("failed to read object blob {hash}: {error}"),
            );
        }
    };

    let value = serde_json::from_slice::<serde_json::Value>(&data)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&data).to_string()));
    GraphObjectDetail::from_json(
        kind,
        Some(hash.to_string()),
        Some(format!("{git_object_type:?}")),
        value,
    )
}

fn graph_object_refs(
    bundle: &ThreadBundle,
    rows: &ProjectionIndexRows,
) -> BTreeSet<(GraphNodeKind, String)> {
    let mut refs = BTreeSet::new();

    for intent in &bundle.thread.intents {
        refs.insert((GraphNodeKind::Intent, intent.intent_id.to_string()));
    }
    if let Some(intent_id) = bundle.thread.current_intent_id {
        refs.insert((GraphNodeKind::Intent, intent_id.to_string()));
    }
    if let Some(intent_id) = bundle.thread.latest_intent_id {
        refs.insert((GraphNodeKind::Intent, intent_id.to_string()));
    }

    for plan in &bundle.scheduler.selected_plan_ids {
        refs.insert((GraphNodeKind::Plan, plan.plan_id.to_string()));
    }
    for plan in &bundle.scheduler.current_plan_heads {
        refs.insert((GraphNodeKind::Plan, plan.plan_id.to_string()));
    }
    if let Some(plan_id) = bundle.scheduler.selected_plan_id {
        refs.insert((GraphNodeKind::Plan, plan_id.to_string()));
    }
    if let Some(task_id) = bundle.scheduler.active_task_id {
        refs.insert((GraphNodeKind::Task, task_id.to_string()));
    }
    if let Some(run_id) = bundle.scheduler.active_run_id {
        refs.insert((GraphNodeKind::Run, run_id.to_string()));
    }

    for row in &rows.intent_plans {
        refs.insert((GraphNodeKind::Plan, row.plan_id.clone()));
    }
    for row in &rows.intent_tasks {
        refs.insert((GraphNodeKind::Task, row.task_id.clone()));
    }
    for row in &rows.plan_tasks {
        refs.insert((GraphNodeKind::Task, row.task_id.clone()));
    }
    for row in &rows.task_runs {
        refs.insert((GraphNodeKind::Run, row.run_id.clone()));
    }
    for row in &rows.run_patchsets {
        refs.insert((GraphNodeKind::Patchset, row.patchset_id.clone()));
    }

    refs
}

#[derive(Debug, Clone)]
struct TimedValue {
    value: String,
    sort: i64,
}

fn group_values_by_key(
    values: impl Iterator<Item = (String, TimedValue)>,
) -> BTreeMap<String, Vec<String>> {
    let mut grouped = BTreeMap::<String, Vec<TimedValue>>::new();
    for (key, value) in values {
        grouped.entry(key).or_default().push(value);
    }

    grouped
        .into_iter()
        .map(|(key, mut values)| {
            values.sort_by(|left, right| {
                left.sort
                    .cmp(&right.sort)
                    .then_with(|| left.value.cmp(&right.value))
            });
            values.dedup_by(|left, right| left.value == right.value);
            (key, values.into_iter().map(|value| value.value).collect())
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn push_task_subgraph(
    graph_rows: &mut Vec<GraphLine>,
    task_id: &str,
    depth: usize,
    runs_by_task: &BTreeMap<String, Vec<String>>,
    patchsets_by_run: &BTreeMap<String, Vec<String>>,
    latest_runs: &BTreeSet<String>,
    latest_run_events: &BTreeMap<String, String>,
    latest_patchsets: &BTreeSet<String>,
    active_task_id: Option<Uuid>,
    active_run_id: Option<Uuid>,
    object_details: &GraphObjectDetails,
) {
    let active_task = active_task_id
        .map(|id| id.to_string())
        .is_some_and(|id| id == task_id);
    let mut task_tags = Vec::new();
    if active_task {
        task_tags.push("active".to_string());
    }

    graph_rows.push(GraphLine {
        depth,
        kind: GraphNodeKind::Task,
        id: task_id.to_string(),
        label: short_id(task_id),
        tags: task_tags,
        detail: vec![
            ("task_id".to_string(), task_id.to_string()),
            ("active".to_string(), active_task.to_string()),
        ],
        object: object_details.get(GraphNodeKind::Task, task_id),
    });

    for run_id in runs_by_task.get(task_id).cloned().unwrap_or_default() {
        let active_run = active_run_id
            .map(|id| id.to_string())
            .is_some_and(|id| id == run_id);
        let mut run_tags = Vec::new();
        if latest_runs.contains(&run_id) {
            run_tags.push("latest".to_string());
        }
        if active_run {
            run_tags.push("active".to_string());
        }
        if let Some(event_kind) = latest_run_events.get(&run_id) {
            run_tags.push(event_kind.clone());
        }

        graph_rows.push(GraphLine {
            depth: depth + 1,
            kind: GraphNodeKind::Run,
            id: run_id.clone(),
            label: short_id(&run_id),
            tags: run_tags,
            detail: vec![
                ("run_id".to_string(), run_id.clone()),
                ("task_id".to_string(), task_id.to_string()),
                (
                    "latest_event".to_string(),
                    latest_run_events
                        .get(&run_id)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string()),
                ),
                ("active".to_string(), active_run.to_string()),
            ],
            object: object_details.get(GraphNodeKind::Run, &run_id),
        });

        for patchset_id in patchsets_by_run.get(&run_id).cloned().unwrap_or_default() {
            let mut patchset_tags = Vec::new();
            if latest_patchsets.contains(&patchset_id) {
                patchset_tags.push("latest".to_string());
            }
            graph_rows.push(GraphLine {
                depth: depth + 2,
                kind: GraphNodeKind::Patchset,
                id: patchset_id.clone(),
                label: short_id(&patchset_id),
                tags: patchset_tags,
                detail: vec![
                    ("patchset_id".to_string(), patchset_id.clone()),
                    ("run_id".to_string(), run_id.clone()),
                ],
                object: object_details.get(GraphNodeKind::Patchset, &patchset_id),
            });
        }
    }
}

fn summarize_object_fields(
    kind: GraphNodeKind,
    value: &serde_json::Value,
) -> Vec<(String, String)> {
    let keys = match kind {
        GraphNodeKind::Intent => [
            "object_id",
            "created_at",
            "created_by",
            "prompt",
            "parents",
            "spec",
            "analysis_context_frames",
        ]
        .as_slice(),
        GraphNodeKind::Plan => [
            "object_id",
            "created_at",
            "created_by",
            "intent",
            "parents",
            "context_frames",
            "steps",
        ]
        .as_slice(),
        GraphNodeKind::Task => [
            "object_id",
            "created_at",
            "created_by",
            "title",
            "description",
            "goal",
            "constraints",
            "acceptance_criteria",
            "requester",
            "parent",
            "intent",
            "origin_step_id",
            "dependencies",
        ]
        .as_slice(),
        GraphNodeKind::Run => [
            "object_id",
            "created_at",
            "created_by",
            "task",
            "plan",
            "commit",
            "snapshot",
            "environment",
        ]
        .as_slice(),
        GraphNodeKind::Patchset => [
            "object_id",
            "created_at",
            "created_by",
            "run",
            "sequence",
            "commit",
            "format",
            "artifact",
            "touched",
            "rationale",
        ]
        .as_slice(),
    };

    let Some(object) = value.as_object() else {
        return vec![("value".to_string(), summarize_json_value(value))];
    };

    keys.iter()
        .filter_map(|key| {
            object
                .get(*key)
                .map(|value| ((*key).to_string(), summarize_json_value(value)))
        })
        .collect()
}

fn summarize_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => truncate_chars(value, MAX_OBJECT_DETAIL_LINE_CHARS),
        serde_json::Value::Array(values) => {
            if values.is_empty() {
                "[]".to_string()
            } else {
                format!("array[{}]", values.len())
            }
        }
        serde_json::Value::Object(values) => {
            if values.is_empty() {
                "{}".to_string()
            } else {
                format!("object{{{} keys}}", values.len())
            }
        }
    }
}

fn pretty_json_lines(value: &serde_json::Value) -> Vec<String> {
    let rendered = serde_json::to_string_pretty(value)
        .unwrap_or_else(|error| format!("failed to render object JSON: {error}"));
    let mut lines = Vec::new();
    for (index, line) in rendered.lines().enumerate() {
        if index >= MAX_OBJECT_DETAIL_LINES {
            lines.push(format!(
                "... truncated after {MAX_OBJECT_DETAIL_LINES} object lines"
            ));
            break;
        }
        lines.push(truncate_chars(line, MAX_OBJECT_DETAIL_LINE_CHARS));
    }
    lines
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

// ── TUI runtime entry: ratatui draw loop + crossterm key router ──────────────
fn run_graph_tui(graph: ThreadGraph) -> std::io::Result<()> {
    let terminal = tui_init()?;
    let _guard = scopeguard::guard((), |_| {
        let _ = tui_restore();
    });
    let mut tui = Tui::new(terminal);
    tui.enter_alt_screen()?;
    let mut app = GraphTuiApp::new(graph);

    loop {
        tui.draw(|frame| render_graph(frame, &mut app))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Esc => {
                    if app.focus == FocusPane::Tree {
                        break;
                    } else {
                        app.focus_left();
                    }
                }
                KeyCode::Char('h') | KeyCode::Left => app.focus_left(),
                KeyCode::Char('l') | KeyCode::Right => app.focus_right(),
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::Char('g') | KeyCode::Home => app.select_first(),
                KeyCode::Char('G') | KeyCode::End => app.select_last(),
                KeyCode::Char(' ') => app.toggle_expand(),
                KeyCode::Enter => app.drill_in(),
                KeyCode::PageUp => app.scroll_details_page_up(),
                KeyCode::PageDown => app.scroll_details_page_down(),
                KeyCode::Char('[') => app.scroll_details_up(),
                KeyCode::Char(']') => app.scroll_details_down(),
                _ => {}
            }
        }
    }

    tui.leave_alt_screen()?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    Tree,
    List,
    Detail,
}

#[derive(Debug, Clone)]
struct GraphTuiApp {
    graph: ThreadGraph,
    /// Index into `graph.lines` of the currently selected tree row.
    selected: usize,
    /// Index of the first visible row at the top of the tree pane.
    scroll: usize,
    page_size: usize,
    /// Index into the children list for the selected node.
    list_selected: usize,
    list_scroll: usize,
    /// Top of the visible window in the detail pane.
    detail_scroll: usize,
    detail_page_size: usize,
    /// Set of (kind, id) pairs that are collapsed. Defaults to all expanded.
    collapsed: HashSet<(GraphNodeKind, String)>,
    focus: FocusPane,
    /// Cached tree shape (branch glyphs / vertical bars), computed once.
    tree_shape: Vec<TreeRowShape>,
}

#[derive(Debug, Clone)]
struct TreeRowShape {
    /// Vertical bars representing ancestor links (e.g. "│  │  ").
    prefix: String,
    /// Own glyph at this depth ("├─ ", "└─ ", or "" for roots).
    glyph: &'static str,
}

impl GraphTuiApp {
    fn new(graph: ThreadGraph) -> Self {
        let tree_shape = compute_tree_shape(&graph.lines);
        let mut app = Self {
            graph,
            selected: 0,
            scroll: 0,
            page_size: 1,
            list_selected: 0,
            list_scroll: 0,
            detail_scroll: 0,
            detail_page_size: 1,
            collapsed: HashSet::new(),
            focus: FocusPane::Tree,
            tree_shape,
        };
        // Prefer landing on the active run/task when present, then fall back
        // to the first head intent. Mirrors the design's "running" cursor.
        app.selected = app.preferred_initial_selection();
        app
    }

    fn preferred_initial_selection(&self) -> usize {
        let candidates = [
            (
                GraphNodeKind::Run,
                self.graph.active_run_id.map(|id| id.to_string()),
            ),
            (
                GraphNodeKind::Task,
                self.graph.active_task_id.map(|id| id.to_string()),
            ),
            (
                GraphNodeKind::Plan,
                self.graph.selected_plan_id.map(|id| id.to_string()),
            ),
        ];
        for (kind, id) in candidates.iter() {
            if let Some(id) = id
                && let Some(idx) = self
                    .graph
                    .lines
                    .iter()
                    .position(|line| line.kind == *kind && &line.id == id)
            {
                return idx;
            }
        }
        0
    }

    fn focus_left(&mut self) {
        self.focus = match self.focus {
            FocusPane::Tree => FocusPane::Tree,
            FocusPane::List => FocusPane::Tree,
            FocusPane::Detail => FocusPane::List,
        };
    }

    fn focus_right(&mut self) {
        self.focus = match self.focus {
            FocusPane::Tree => FocusPane::List,
            FocusPane::List => FocusPane::Detail,
            FocusPane::Detail => FocusPane::Detail,
        };
    }

    fn drill_in(&mut self) {
        match self.focus {
            FocusPane::Tree => {
                let selected = self.selected;
                if has_children(&self.graph.lines, selected) && self.is_collapsed(selected) {
                    self.set_collapsed(selected, false);
                } else {
                    self.focus = FocusPane::List;
                    self.list_selected = 0;
                    self.list_scroll = 0;
                }
            }
            FocusPane::List => {
                if let Some(child_idx) = self.children_indices().get(self.list_selected).copied() {
                    self.set_selected(child_idx);
                }
                self.focus = FocusPane::Detail;
            }
            FocusPane::Detail => {}
        }
    }

    fn move_up(&mut self) {
        match self.focus {
            FocusPane::Tree => self.select_previous(),
            FocusPane::List => self.list_selected = self.list_selected.saturating_sub(1),
            FocusPane::Detail => self.scroll_details_up(),
        }
    }

    fn move_down(&mut self) {
        match self.focus {
            FocusPane::Tree => self.select_next(),
            FocusPane::List => {
                let n = self.children_indices().len();
                if n > 0 && self.list_selected + 1 < n {
                    self.list_selected += 1;
                }
            }
            FocusPane::Detail => self.scroll_details_down(),
        }
    }

    fn toggle_expand(&mut self) {
        if self.focus == FocusPane::Tree && has_children(&self.graph.lines, self.selected) {
            let collapsed = self.is_collapsed(self.selected);
            self.set_collapsed(self.selected, !collapsed);
        }
    }

    fn is_collapsed(&self, idx: usize) -> bool {
        self.graph
            .lines
            .get(idx)
            .map(|line| self.collapsed.contains(&(line.kind, line.id.clone())))
            .unwrap_or(false)
    }

    fn set_collapsed(&mut self, idx: usize, collapsed: bool) {
        if let Some(line) = self.graph.lines.get(idx) {
            let key = (line.kind, line.id.clone());
            if collapsed {
                self.collapsed.insert(key);
            } else {
                self.collapsed.remove(&key);
            }
        }
    }

    fn set_selected(&mut self, selected: usize) {
        if self.selected != selected {
            self.selected = selected;
            self.detail_scroll = 0;
            self.list_selected = 0;
            self.list_scroll = 0;
        }
    }

    fn select_previous(&mut self) {
        let visible = self.visible_indices();
        if let Some(pos) = visible.iter().position(|&i| i == self.selected)
            && pos > 0
        {
            self.set_selected(visible[pos - 1]);
        }
    }

    fn select_next(&mut self) {
        let visible = self.visible_indices();
        if let Some(pos) = visible.iter().position(|&i| i == self.selected)
            && pos + 1 < visible.len()
        {
            self.set_selected(visible[pos + 1]);
        }
    }

    fn select_first(&mut self) {
        if let Some(&first) = self.visible_indices().first() {
            self.set_selected(first);
        }
    }

    fn select_last(&mut self) {
        if let Some(&last) = self.visible_indices().last() {
            self.set_selected(last);
        }
    }

    fn scroll_details_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(1);
    }

    fn scroll_details_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(1);
    }

    fn scroll_details_page_up(&mut self) {
        self.detail_scroll = self
            .detail_scroll
            .saturating_sub(self.detail_page_size.max(1));
    }

    fn scroll_details_page_down(&mut self) {
        self.detail_scroll = self
            .detail_scroll
            .saturating_add(self.detail_page_size.max(1));
    }

    /// Indices of `graph.lines` that are currently visible (respecting
    /// collapsed nodes). Returned in tree DFS order.
    fn visible_indices(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.graph.lines.len());
        let mut hide_below: Option<usize> = None;
        for (i, line) in self.graph.lines.iter().enumerate() {
            if let Some(d) = hide_below {
                if line.depth > d {
                    continue;
                } else {
                    hide_below = None;
                }
            }
            out.push(i);
            if has_children(&self.graph.lines, i) && self.is_collapsed(i) {
                hide_below = Some(line.depth);
            }
        }
        out
    }

    /// Indices of the direct children of the currently selected node.
    fn children_indices(&self) -> Vec<usize> {
        children_of(&self.graph.lines, self.selected)
    }

    fn keep_selection_visible(&mut self, visible: &[usize], height: usize) {
        self.page_size = height.max(1);
        let pos = visible
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        let mut top_pos = visible.iter().position(|&i| i == self.scroll).unwrap_or(0);
        if pos < top_pos {
            top_pos = pos;
        }
        if pos >= top_pos + height.max(1) {
            top_pos = pos.saturating_sub(height.saturating_sub(1));
        }
        let max_top = visible.len().saturating_sub(height.max(1));
        top_pos = top_pos.min(max_top);
        self.scroll = visible.get(top_pos).copied().unwrap_or(0);
    }

    fn keep_list_selection_visible(&mut self, height: usize) {
        let n = self.children_indices().len();
        if n == 0 {
            self.list_selected = 0;
            self.list_scroll = 0;
            return;
        }
        if self.list_selected >= n {
            self.list_selected = n - 1;
        }
        if self.list_selected < self.list_scroll {
            self.list_scroll = self.list_selected;
        }
        let h = height.max(1);
        let bottom = self.list_scroll.saturating_add(h);
        if self.list_selected >= bottom {
            self.list_scroll = self.list_selected.saturating_sub(h.saturating_sub(1));
        }
    }

    fn keep_detail_scroll_bounded(&mut self, line_count: usize, height: usize) {
        self.detail_page_size = height.max(1);
        let max_scroll = line_count.saturating_sub(height.max(1));
        self.detail_scroll = self.detail_scroll.min(max_scroll);
    }
}

/// `true` if `lines[idx]` has at least one direct child in DFS order.
fn has_children(lines: &[GraphLine], idx: usize) -> bool {
    let Some(line) = lines.get(idx) else {
        return false;
    };
    lines
        .get(idx + 1)
        .map(|next| next.depth > line.depth)
        .unwrap_or(false)
}

/// Indices of direct children of `lines[idx]` in DFS order.
fn children_of(lines: &[GraphLine], idx: usize) -> Vec<usize> {
    let Some(parent) = lines.get(idx) else {
        return Vec::new();
    };
    let parent_depth = parent.depth;
    let mut out = Vec::new();
    for (offset, line) in lines.iter().enumerate().skip(idx + 1) {
        if line.depth <= parent_depth {
            break;
        }
        if line.depth == parent_depth + 1 {
            out.push(offset);
        }
    }
    out
}

/// Pre-compute branch glyphs (`├─`, `└─`, vertical bars) for every line.
/// Visual style mirrors `app.jsx`'s `flattenTree`.
fn compute_tree_shape(lines: &[GraphLine]) -> Vec<TreeRowShape> {
    let n = lines.len();
    if n == 0 {
        return Vec::new();
    }

    // is_last[i] = true when no later sibling exists at lines[i].depth
    // before depth drops below lines[i].depth.
    let mut is_last = vec![true; n];
    for (i, line) in lines.iter().enumerate() {
        let d = line.depth;
        for later in lines.iter().skip(i + 1) {
            if later.depth < d {
                break;
            }
            if later.depth == d {
                is_last[i] = false;
                break;
            }
        }
    }

    // For each line, find its immediate parent index in DFS order.
    let mut parent: Vec<Option<usize>> = vec![None; n];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..n {
        let d = lines[i].depth;
        while let Some(&top) = stack.last() {
            if lines[top].depth < d {
                break;
            }
            stack.pop();
        }
        parent[i] = stack.last().copied();
        stack.push(i);
    }

    let mut shapes = Vec::with_capacity(n);
    for i in 0..n {
        let d = lines[i].depth;
        // Walk up to populate is_last per ancestor depth (0..d-1).
        let mut ancestor_is_last = vec![false; d];
        let mut cur = parent[i];
        while let Some(p) = cur {
            let pd = lines[p].depth;
            if pd < d {
                ancestor_is_last[pd] = is_last[p];
            }
            cur = parent[p];
        }
        let mut prefix = String::new();
        for last in &ancestor_is_last {
            prefix.push_str(if *last { "   " } else { "│  " });
        }
        let glyph: &'static str = if d == 0 {
            ""
        } else if is_last[i] {
            "└─ "
        } else {
            "├─ "
        };
        shapes.push(TreeRowShape { prefix, glyph });
    }
    shapes
}

/// Map a graph line to a status (k9s palette). Looks at tags first, then
/// falls back to a per-kind default.
fn line_status(line: &GraphLine) -> StatusInfo {
    for tag in &line.tags {
        if let Some(st) = status_for_event_kind(tag) {
            return st;
        }
    }
    if line.tags.iter().any(|t| t == "active") {
        return STATUS_RUNNING;
    }
    if line.tags.iter().any(|t| t == "head" || t == "latest") {
        return STATUS_SUCCEEDED;
    }
    match line.kind {
        GraphNodeKind::Patchset => STATUS_SUCCEEDED,
        _ => STATUS_NEUTRAL,
    }
}

// ── Top-level draw: top bar + three-pane body + help bar ────────────────────
fn render_graph(frame: &mut Frame, app: &mut GraphTuiApp) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(COLOR_BG)), area);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(area);

    render_topbar(frame, vertical[0], &app.graph);

    let body = split_graph_body(vertical[1]);

    let visible = app.visible_indices();
    let tree_inner_h = body[0].height.saturating_sub(3) as usize; // borders + foot
    app.keep_selection_visible(&visible, tree_inner_h);
    let list_inner_h = body[1].height.saturating_sub(3) as usize; // borders + header
    app.keep_list_selection_visible(list_inner_h);

    render_tree_pane(frame, body[0], app, &visible);
    render_list_pane(frame, body[1], app);
    render_detail_pane(frame, body[2], app);

    render_helpbar(frame, vertical[2], app);
}

/// Compute layout for the three-pane body: tree | list | detail.
fn split_graph_body(area: Rect) -> [Rect; 3] {
    let total = TREE_PANE_WEIGHT + LIST_PANE_WEIGHT + DETAIL_PANE_WEIGHT;
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(u32::from(TREE_PANE_WEIGHT), u32::from(total)),
            Constraint::Ratio(u32::from(LIST_PANE_WEIGHT), u32::from(total)),
            Constraint::Ratio(u32::from(DETAIL_PANE_WEIGHT), u32::from(total)),
        ])
        .split(area);
    [body[0], body[1], body[2]]
}

/// Pane block with border tinted by focus state.
fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let border_color = if focused { COLOR_ACCENT } else { COLOR_BORDER };
    let title_color = if focused { COLOR_ACCENT } else { COLOR_HINT };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!("┤{title}├"),
            Style::default()
                .fg(title_color)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(COLOR_BG))
}

// ── Top bar: brand · thread metadata · freshness · timestamps ────────────────
fn render_topbar(frame: &mut Frame, area: Rect, graph: &ThreadGraph) {
    let title = graph.title.as_deref().unwrap_or("Untitled thread");

    let mut left: Vec<Span<'static>> = Vec::new();
    left.push(Span::styled(
        " libra",
        Style::default()
            .fg(COLOR_BRAND)
            .add_modifier(Modifier::BOLD),
    ));
    left.push(Span::styled(" · ", Style::default().fg(COLOR_FG_MUTED)));
    left.push(Span::styled(
        "graph",
        Style::default().fg(COLOR_FG).add_modifier(Modifier::BOLD),
    ));
    left.push(Span::styled(" │ ", Style::default().fg(COLOR_FG_MUTED)));
    left.push(Span::styled("thread:", Style::default().fg(COLOR_FG_MUTED)));
    left.push(Span::styled(
        format!("{} ", short_id(&graph.thread_id.to_string())),
        Style::default().fg(GraphNodeKind::Plan.color()),
    ));
    left.push(Span::styled(
        format!("v{}/{}", graph.thread_version, graph.scheduler_version),
        Style::default().fg(STATUS_SUCCEEDED.color),
    ));
    left.push(Span::styled(" · ", Style::default().fg(COLOR_FG_MUTED)));
    left.push(Span::styled(
        format!("{:?}", graph.freshness).to_ascii_lowercase(),
        Style::default().fg(STATUS_SUCCEEDED.color),
    ));
    left.push(Span::styled(" │ ", Style::default().fg(COLOR_FG_MUTED)));
    left.push(Span::styled(
        format!("\"{}\"", truncate_chars(title, 60)),
        Style::default().fg(COLOR_FG),
    ));

    let right: Vec<Span<'static>> = vec![
        Span::styled("plan:", Style::default().fg(COLOR_FG_MUTED)),
        Span::styled(
            graph
                .selected_plan_id
                .map(|id| short_id(&id.to_string()))
                .unwrap_or_else(|| "—".to_string()),
            Style::default().fg(GraphNodeKind::Plan.color()),
        ),
        Span::raw("  "),
        Span::styled("task:", Style::default().fg(COLOR_FG_MUTED)),
        Span::styled(
            graph
                .active_task_id
                .map(|id| short_id(&id.to_string()))
                .unwrap_or_else(|| "—".to_string()),
            Style::default().fg(GraphNodeKind::Task.color()),
        ),
        Span::raw("  "),
        Span::styled("run:", Style::default().fg(COLOR_FG_MUTED)),
        Span::styled(
            graph
                .active_run_id
                .map(|id| short_id(&id.to_string()))
                .unwrap_or_else(|| "—".to_string()),
            Style::default().fg(GraphNodeKind::Run.color()),
        ),
        Span::raw("  "),
        Span::styled(
            format_timestamp(graph.updated_at),
            Style::default().fg(COLOR_FG_MUTED),
        ),
        Span::raw(" "),
    ];

    let right_w = total_span_width(&right) as u16;
    let row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_w)])
        .split(area);

    let topbar_style = Style::default().bg(COLOR_BG_PANEL);
    frame.render_widget(Paragraph::new(Line::from(left)).style(topbar_style), row[0]);
    frame.render_widget(
        Paragraph::new(Line::from(right)).style(topbar_style),
        row[1],
    );
}

fn total_span_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum()
}

// ── Pane A: TASK DAG (tree view) ────────────────────────────────────────────
fn render_tree_pane(frame: &mut Frame, area: Rect, app: &GraphTuiApp, visible: &[usize]) {
    let focused = app.focus == FocusPane::Tree;
    let block = pane_block(" TASK DAG ", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if visible.is_empty() {
        let empty = Paragraph::new(Line::styled(
            "  No version nodes are available for this thread.",
            Style::default().fg(COLOR_FG_MUTED),
        ));
        frame.render_widget(empty, inner);
        return;
    }

    // Reserve last row for status counts.
    let foot_h = if inner.height >= 2 { 1 } else { 0 };
    let body_h = inner.height.saturating_sub(foot_h) as usize;
    let body_area = Rect::new(inner.x, inner.y, inner.width, body_h as u16);

    let scroll_pos = visible.iter().position(|&i| i == app.scroll).unwrap_or(0);
    let end = scroll_pos.saturating_add(body_h).min(visible.len());

    let mut rows: Vec<Line<'static>> = Vec::new();
    for &line_idx in &visible[scroll_pos..end] {
        rows.push(render_tree_row(app, line_idx, focused));
    }
    frame.render_widget(Paragraph::new(Text::from(rows)), body_area);

    if foot_h == 1 {
        let foot_y = inner.y.saturating_add(body_h as u16);
        let foot_area = Rect::new(inner.x, foot_y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(tree_foot_line(&app.graph, visible.len()))
                .style(Style::default().bg(COLOR_BG_PANEL)),
            foot_area,
        );
    }
}

fn render_tree_row(app: &GraphTuiApp, line_idx: usize, focused: bool) -> Line<'static> {
    let line = &app.graph.lines[line_idx];
    let shape = &app.tree_shape[line_idx];
    let selected = line_idx == app.selected;

    let row_bg = if selected {
        Some(if focused {
            COLOR_BG_SEL
        } else {
            COLOR_BG_SEL_DIM
        })
    } else {
        None
    };
    let base_style = match row_bg {
        Some(bg) if focused => Style::default().fg(COLOR_SEL_FG).bg(bg),
        Some(bg) => Style::default().fg(COLOR_FG).bg(bg),
        None => Style::default().fg(COLOR_FG),
    };
    let dim_color = if selected && focused {
        COLOR_SEL_HINT
    } else {
        COLOR_FG_MUTED
    };
    let stat = line_status(line);
    let kind_color = if selected && focused {
        COLOR_SEL_FG
    } else {
        line.kind.color()
    };
    let stat_color = if selected && focused {
        COLOR_SEL_FG
    } else {
        stat.color
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    // Tree branch glyphs
    spans.push(Span::styled(
        format!("{}{}", shape.prefix, shape.glyph),
        base_style.patch(Style::default().fg(dim_color)),
    ));
    // Expand/collapse caret
    let caret = if has_children(&app.graph.lines, line_idx) {
        if app.is_collapsed(line_idx) {
            "▸ "
        } else {
            "▾ "
        }
    } else {
        "  "
    };
    spans.push(Span::styled(
        caret.to_string(),
        base_style.patch(Style::default().fg(stat_color)),
    ));
    // Status dot
    spans.push(Span::styled(
        format!("{} ", stat.glyph),
        base_style.patch(Style::default().fg(stat_color)),
    ));
    // Kind tag (padded fixed width: matches the "[task ]" style from the design)
    spans.push(Span::styled(
        format!("[{:8}] ", line.kind.history_type()),
        base_style.patch(Style::default().fg(kind_color).add_modifier(Modifier::BOLD)),
    ));
    // Marker + label
    spans.push(Span::styled(format!("{} ", line.label), base_style));
    // Tags rendered as ‹active› ‹head› etc.
    for tag in &line.tags {
        spans.push(Span::styled(
            format!("‹{tag}› "),
            base_style.patch(Style::default().fg(dim_color)),
        ));
    }
    Line::from(spans)
}

fn tree_foot_line(graph: &ThreadGraph, visible_count: usize) -> Line<'static> {
    let mut counts = BTreeMap::<&'static str, usize>::new();
    for line in &graph.lines {
        let st = line_status(line);
        let key = if st.glyph == STATUS_SUCCEEDED.glyph {
            "ok"
        } else if st.glyph == STATUS_RUNNING.glyph {
            "run"
        } else if st.glyph == STATUS_FAILED.glyph {
            "fail"
        } else if st.glyph == STATUS_BLOCKED.glyph {
            "block"
        } else if st.glyph == STATUS_QUEUED.glyph {
            "queue"
        } else {
            "neutral"
        };
        *counts.entry(key).or_insert(0) += 1;
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(
        format!(" {} visible · {} nodes  ", visible_count, graph.lines.len()),
        Style::default().fg(COLOR_FG_MUTED),
    ));
    let push_count = |spans: &mut Vec<Span<'static>>, key: &str, st: StatusInfo| {
        let n = counts.get(key).copied().unwrap_or(0);
        spans.push(Span::styled(
            format!("{} {}  ", st.glyph, n),
            Style::default().fg(st.color),
        ));
    };
    push_count(&mut spans, "ok", STATUS_SUCCEEDED);
    push_count(&mut spans, "run", STATUS_RUNNING);
    push_count(&mut spans, "block", STATUS_BLOCKED);
    push_count(&mut spans, "fail", STATUS_FAILED);
    push_count(&mut spans, "queue", STATUS_QUEUED);
    Line::from(spans)
}

// ── Pane B: CHILDREN list ───────────────────────────────────────────────────
fn render_list_pane(frame: &mut Frame, area: Rect, app: &GraphTuiApp) {
    let focused = app.focus == FocusPane::List;
    let title = match app.graph.lines.get(app.selected) {
        Some(line) => format!(" CHILDREN · {} ", short_id(&line.id)),
        None => " CHILDREN ".to_string(),
    };
    let block = pane_block(&title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let parent_kind = match app.graph.lines.get(app.selected) {
        Some(line) => line.kind,
        None => {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "  No node selected.",
                    Style::default().fg(COLOR_FG_MUTED),
                )),
                inner,
            );
            return;
        }
    };

    let children = app.children_indices();
    if children.is_empty() {
        let label = match parent_kind {
            GraphNodeKind::Patchset => "  — patchset is a leaf —",
            GraphNodeKind::Run => "  — no patchsets recorded —",
            GraphNodeKind::Task => "  — no runs yet —",
            GraphNodeKind::Plan => "  — no tasks linked —",
            GraphNodeKind::Intent => "  — no plans or tasks linked —",
        };
        frame.render_widget(
            Paragraph::new(Line::styled(label, Style::default().fg(COLOR_FG_MUTED))),
            inner,
        );
        return;
    }

    let header = match parent_kind {
        GraphNodeKind::Task => {
            list_header_row(&[("#", 4), ("STATUS", 8), ("RUN", 12), ("LATEST EVENT", 24)])
        }
        GraphNodeKind::Run => list_header_row(&[("#", 4), ("PATCHSET", 14), ("TAGS", 24)]),
        GraphNodeKind::Plan | GraphNodeKind::Intent => {
            list_header_row(&[("#", 4), ("KIND", 9), ("ID", 12), ("TAGS", 24)])
        }
        GraphNodeKind::Patchset => list_header_row(&[("#", 4), ("ID", 12)]),
    };

    let body_h = inner.height.saturating_sub(1) as usize;
    let scroll = app.list_scroll.min(children.len().saturating_sub(1));
    let end = scroll.saturating_add(body_h).min(children.len());

    let mut rows: Vec<Line<'static>> = Vec::with_capacity(1 + (end - scroll));
    rows.push(header);
    for (offset, &child_idx) in children[scroll..end].iter().enumerate() {
        let absolute = scroll + offset;
        let selected = absolute == app.list_selected;
        rows.push(render_list_row(
            &app.graph.lines[child_idx],
            absolute,
            selected,
            focused,
            parent_kind,
        ));
    }
    frame.render_widget(Paragraph::new(Text::from(rows)), inner);
}

fn list_header_row(cols: &[(&'static str, usize)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));
    for (label, width) in cols {
        spans.push(Span::styled(
            pad_right(label, *width),
            Style::default()
                .fg(COLOR_FG_MUTED)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn render_list_row(
    line: &GraphLine,
    index: usize,
    selected: bool,
    focused: bool,
    parent_kind: GraphNodeKind,
) -> Line<'static> {
    let row_bg = if selected {
        Some(if focused {
            COLOR_BG_SEL
        } else {
            COLOR_BG_SEL_DIM
        })
    } else {
        None
    };
    let base = match row_bg {
        Some(bg) if focused => Style::default().fg(COLOR_SEL_FG).bg(bg),
        Some(bg) => Style::default().fg(COLOR_FG).bg(bg),
        None => Style::default().fg(COLOR_FG),
    };
    let dim_color = if selected && focused {
        COLOR_SEL_HINT
    } else {
        COLOR_FG_MUTED
    };
    let stat = line_status(line);

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled(
        format!("{:>2}. ", index + 1),
        base.patch(Style::default().fg(dim_color)),
    ));

    match parent_kind {
        GraphNodeKind::Task => {
            spans.push(Span::styled(
                format!("{} ", stat.glyph),
                base.patch(Style::default().fg(stat.color)),
            ));
            spans.push(Span::styled(
                pad_right(stat.label, 6),
                base.patch(Style::default().fg(stat.color).add_modifier(Modifier::BOLD)),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                pad_right(&short_id(&line.id), 12),
                base.patch(Style::default().fg(line.kind.color())),
            ));
            let event_tag = line
                .tags
                .iter()
                .find(|tag| status_for_event_kind(tag).is_some())
                .cloned()
                .or_else(|| line.tags.first().cloned())
                .unwrap_or_else(|| "—".to_string());
            spans.push(Span::styled(
                pad_right(&event_tag, 24),
                base.patch(Style::default().fg(dim_color)),
            ));
        }
        GraphNodeKind::Run => {
            spans.push(Span::styled(
                "◆ ",
                base.patch(Style::default().fg(line.kind.color())),
            ));
            spans.push(Span::styled(
                pad_right(&short_id(&line.id), 14),
                base.patch(Style::default().fg(line.kind.color())),
            ));
            spans.push(Span::styled(
                pad_right(&line.tags.join(", "), 24),
                base.patch(Style::default().fg(dim_color)),
            ));
        }
        GraphNodeKind::Plan | GraphNodeKind::Intent => {
            spans.push(Span::styled(
                pad_right(&format!("[{}]", line.kind.history_type()), 9),
                base.patch(
                    Style::default()
                        .fg(line.kind.color())
                        .add_modifier(Modifier::BOLD),
                ),
            ));
            spans.push(Span::styled(pad_right(&short_id(&line.id), 12), base));
            spans.push(Span::styled(
                pad_right(&line.tags.join(", "), 24),
                base.patch(Style::default().fg(dim_color)),
            ));
        }
        GraphNodeKind::Patchset => {
            spans.push(Span::styled(
                pad_right(&short_id(&line.id), 12),
                base.patch(Style::default().fg(line.kind.color())),
            ));
        }
    }

    Line::from(spans)
}

fn pad_right(value: &str, width: usize) -> String {
    let w = display_width(value);
    if w >= width {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + (width - w));
    out.push_str(value);
    for _ in 0..(width - w) {
        out.push(' ');
    }
    out
}

// ── Pane C: DETAIL ──────────────────────────────────────────────────────────
fn render_detail_pane(frame: &mut Frame, area: Rect, app: &mut GraphTuiApp) {
    let focused = app.focus == FocusPane::Detail;
    let block = pane_block(" DETAIL ", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible = inner.height as usize;
    let content_width = inner.width as usize;
    let lines = detail_lines_for_width(app.graph.lines.get(app.selected), content_width);
    app.keep_detail_scroll_bounded(lines.len(), visible);
    let scroll = app.detail_scroll as u16;
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        inner,
    );
}

fn detail_lines_for_width(
    selected: Option<&GraphLine>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let Some(line) = selected else {
        lines.push(Line::styled(
            "No node selected.",
            Style::default().fg(COLOR_FG_MUTED),
        ));
        return lines;
    };

    let stat = line_status(line);

    // Header: status badge + kind + short id
    let mut header: Vec<Span<'static>> = Vec::new();
    header.push(Span::styled(
        format!(" {} ", stat.label),
        Style::default()
            .fg(COLOR_BG)
            .bg(stat.color)
            .add_modifier(Modifier::BOLD),
    ));
    header.push(Span::raw("  "));
    header.push(Span::styled(
        line.kind.label().to_string(),
        Style::default()
            .fg(line.kind.color())
            .add_modifier(Modifier::BOLD),
    ));
    header.push(Span::raw(" "));
    header.push(Span::styled(
        short_id(&line.id),
        Style::default().fg(COLOR_FG),
    ));
    lines.push(Line::from(header));
    lines.push(Line::raw(""));

    // KV detail block
    for (key, value) in &line.detail {
        push_detail_kv(&mut lines, key, value, content_width);
    }

    // Tags rendered as colored pills
    if !line.tags.is_empty() {
        section_header(&mut lines, "TAGS", COLOR_HINT, content_width);
        let mut spans: Vec<Span<'static>> = Vec::new();
        for tag in &line.tags {
            let color = status_for_event_kind(tag)
                .map(|st| st.color)
                .unwrap_or(line.kind.color());
            spans.push(Span::styled(
                format!(" {tag} "),
                Style::default()
                    .fg(COLOR_BG)
                    .bg(color)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }

    // Object details from on-disk history
    if let Some(object) = &line.object {
        section_header(&mut lines, "OBJECT", COLOR_ACCENT, content_width);
        push_detail_kv(
            &mut lines,
            "object_type",
            &object.object_type,
            content_width,
        );
        if let Some(hash) = &object.hash {
            push_detail_kv(&mut lines, "object_hash", hash, content_width);
        }
        if let Some(git_object_type) = &object.git_object_type {
            push_detail_kv(
                &mut lines,
                "git_object_type",
                git_object_type,
                content_width,
            );
        }
        for (key, value) in &object.summary {
            push_detail_kv(&mut lines, key, value, content_width);
        }
        if !object.raw_json_lines.is_empty() {
            section_header(&mut lines, "OBJECT JSON", COLOR_HINT, content_width);
            for raw_line in &object.raw_json_lines {
                push_detail_wrapped_line(&mut lines, raw_line, content_width);
            }
        }
    }

    lines
}

fn section_header(lines: &mut Vec<Line<'static>>, title: &str, tone: Color, content_width: usize) {
    // Cap the trailing rule so a usize::MAX content_width (used in tests for
    // unwrapped detail rendering) cannot blow the allocator.
    const MAX_DASH_COUNT: usize = 80;
    lines.push(Line::raw(""));
    let prefix = "── ";
    let label = format!("{title} ");
    let consumed = display_width(prefix) + display_width(&label);
    let dash_count = content_width
        .saturating_sub(consumed)
        .clamp(2, MAX_DASH_COUNT);
    let trail: String = "─".repeat(dash_count);
    lines.push(Line::from(vec![
        Span::styled(prefix, Style::default().fg(tone)),
        Span::styled(
            label,
            Style::default().fg(tone).add_modifier(Modifier::BOLD),
        ),
        Span::styled(trail, Style::default().fg(tone)),
    ]));
}

fn detail_label_style() -> Style {
    Style::default()
        .fg(COLOR_FG_MUTED)
        .add_modifier(Modifier::BOLD)
}

fn detail_value_style() -> Style {
    Style::default().fg(COLOR_FG)
}

fn push_detail_kv(lines: &mut Vec<Line<'static>>, key: &str, value: &str, content_width: usize) {
    let key_text = format!("{key}: ");
    let key_width = display_width(&key_text);
    if content_width != usize::MAX
        && key_width.saturating_add(MIN_DETAIL_VALUE_WIDTH) > content_width
    {
        lines.push(Line::styled(
            key_text.trim_end().to_string(),
            detail_label_style(),
        ));
        let value_width = content_width.saturating_sub(2).max(1);
        for chunk in wrap_display_width(value, value_width) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(chunk, detail_value_style()),
            ]));
        }
        return;
    }

    let value_width = if content_width == usize::MAX {
        usize::MAX
    } else {
        content_width
            .saturating_sub(key_width)
            .max(MIN_DETAIL_VALUE_WIDTH)
    };
    let chunks = wrap_display_width(value, value_width);
    let mut iter = chunks.into_iter();
    let first = iter.next().unwrap_or_default();
    lines.push(Line::from(vec![
        Span::styled(key_text.clone(), detail_label_style()),
        Span::styled(first, detail_value_style()),
    ]));

    let indent = if content_width == usize::MAX {
        " ".repeat(key_width)
    } else {
        " ".repeat(key_width.min(content_width.saturating_sub(1)))
    };
    for chunk in iter {
        lines.push(Line::from(vec![
            Span::raw(indent.clone()),
            Span::styled(chunk, detail_value_style()),
        ]));
    }
}

fn push_detail_wrapped_line(lines: &mut Vec<Line<'static>>, value: &str, content_width: usize) {
    for chunk in wrap_preformatted_display_width(value, content_width.max(1)) {
        lines.push(Line::styled(chunk, detail_value_style()));
    }
}

fn wrap_preformatted_display_width(value: &str, max_width: usize) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }
    if max_width == usize::MAX {
        return value.lines().map(ToString::to_string).collect();
    }

    let width = max_width.max(1);
    let mut lines = Vec::new();
    for source_line in value.lines() {
        let mut current = String::new();
        let mut current_width = 0usize;
        for ch in source_line.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > 0 && current_width.saturating_add(ch_width) > width {
                lines.push(current);
                current = String::new();
                current_width = 0;
            }
            current.push(ch);
            current_width = current_width.saturating_add(ch_width);
        }
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn wrap_display_width(value: &str, max_width: usize) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }
    if max_width == usize::MAX {
        return value.lines().map(ToString::to_string).collect();
    }

    let width = max_width.max(1);
    let mut lines = Vec::new();
    for source_line in value.lines() {
        let mut current = String::new();
        let mut current_width = 0usize;
        for ch in source_line.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > 0 && current_width.saturating_add(ch_width) > width {
                lines.push(current.trim_end().to_string());
                current = String::new();
                current_width = 0;
            }
            if current_width == 0 && ch.is_whitespace() {
                continue;
            }
            current.push(ch);
            current_width = current_width.saturating_add(ch_width);
        }
        lines.push(current.trim_end().to_string());
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn display_width(value: &str) -> usize {
    value
        .chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

// ── Help bar: keyboard hints + active-pane indicator ────────────────────────
fn render_helpbar(frame: &mut Frame, area: Rect, app: &GraphTuiApp) {
    let hints: &[(&str, &str)] = &[
        ("j/k ↓↑", "move"),
        ("h/l ←→", "pane"),
        ("⏎", "drill"),
        ("␣", "toggle"),
        ("g/G", "top/end"),
        ("[/]", "scroll"),
        ("PgUp/PgDn", "page"),
        ("esc", "back"),
        ("q", "quit"),
    ];
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));
    for (key, label) in hints {
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(COLOR_FG)
                .bg(COLOR_KEYCAP_BG)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}  "),
            Style::default().fg(COLOR_FG_MUTED),
        ));
    }
    let pane_label = match app.focus {
        FocusPane::Tree => "TREE",
        FocusPane::List => "CHILDREN",
        FocusPane::Detail => "DETAIL",
    };
    let right = vec![
        Span::styled("pane ", Style::default().fg(COLOR_FG_MUTED)),
        Span::styled(
            pane_label.to_string(),
            Style::default()
                .fg(COLOR_ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];

    let row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(total_span_width(&right) as u16),
        ])
        .split(area);

    let bar = Style::default().bg(COLOR_BG_PANEL);
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bar), row[0]);
    frame.render_widget(Paragraph::new(Line::from(right)).style(bar), row[1]);
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use git_internal::internal::object::types::ActorRef;

    use super::*;
    use crate::internal::ai::{
        projection::{
            PlanHeadRef, SchedulerState, ThreadIntentLinkReason, ThreadIntentRef,
            ThreadParticipant, ThreadParticipantRole, ThreadProjection,
        },
        runtime::contracts::ProjectionFreshness,
    };

    fn id(value: &str) -> Uuid {
        Uuid::parse_str(value).expect("test UUID should be valid")
    }

    fn ts(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .expect("test timestamp should be valid")
    }

    fn sample_bundle() -> ThreadBundle {
        let thread_id = id("11111111-1111-4111-8111-111111111111");
        let intent_id = id("22222222-2222-4222-8222-222222222222");
        let plan_id = id("33333333-3333-4333-8333-333333333333");
        let task_id = id("44444444-4444-4444-8444-444444444444");
        let run_id = id("55555555-5555-4555-8555-555555555555");
        let owner = ActorRef::human("graph-test").expect("actor");

        ThreadBundle {
            thread: ThreadProjection {
                thread_id,
                title: Some("Graph test".to_string()),
                owner: owner.clone(),
                participants: vec![ThreadParticipant {
                    actor: owner,
                    role: ThreadParticipantRole::Owner,
                    joined_at: ts(1),
                }],
                current_intent_id: Some(intent_id),
                latest_intent_id: Some(intent_id),
                intents: vec![ThreadIntentRef {
                    intent_id,
                    ordinal: 0,
                    is_head: true,
                    linked_at: ts(2),
                    link_reason: ThreadIntentLinkReason::Seed,
                }],
                metadata: None,
                archived: false,
                created_at: ts(1),
                updated_at: ts(10),
                version: 2,
            },
            scheduler: SchedulerState {
                thread_id,
                selected_plan_id: Some(plan_id),
                selected_plan_ids: vec![PlanHeadRef {
                    plan_id,
                    ordinal: 0,
                }],
                current_plan_heads: vec![PlanHeadRef {
                    plan_id,
                    ordinal: 0,
                }],
                active_task_id: Some(task_id),
                active_run_id: Some(run_id),
                live_context_window: Vec::new(),
                metadata: None,
                updated_at: ts(11),
                version: 3,
            },
            freshness: ProjectionFreshness::Fresh,
        }
    }

    #[test]
    fn graph_model_orders_thread_versions_from_projection_indexes() {
        let bundle = sample_bundle();
        let rows = ProjectionIndexRows {
            intent_plans: vec![ai_index_intent_plan::Model {
                intent_id: "22222222-2222-4222-8222-222222222222".to_string(),
                plan_id: "33333333-3333-4333-8333-333333333333".to_string(),
                created_at: 3,
            }],
            intent_tasks: vec![ai_index_intent_task::Model {
                intent_id: "22222222-2222-4222-8222-222222222222".to_string(),
                task_id: "44444444-4444-4444-8444-444444444444".to_string(),
                parent_task_id: None,
                origin_step_id: None,
                created_at: 4,
            }],
            plan_tasks: vec![ai_index_plan_step_task::Model {
                plan_id: "33333333-3333-4333-8333-333333333333".to_string(),
                task_id: "44444444-4444-4444-8444-444444444444".to_string(),
                step_id: "66666666-6666-4666-8666-666666666666".to_string(),
                created_at: 5,
            }],
            task_runs: vec![ai_index_task_run::Model {
                task_id: "44444444-4444-4444-8444-444444444444".to_string(),
                run_id: "55555555-5555-4555-8555-555555555555".to_string(),
                is_latest: true,
                created_at: 6,
            }],
            run_events: vec![ai_index_run_event::Model {
                run_id: "55555555-5555-4555-8555-555555555555".to_string(),
                event_id: "77777777-7777-4777-8777-777777777777".to_string(),
                event_kind: "completed".to_string(),
                is_latest: true,
                created_at: 7,
            }],
            run_patchsets: vec![ai_index_run_patchset::Model {
                run_id: "55555555-5555-4555-8555-555555555555".to_string(),
                patchset_id: "88888888-8888-4888-8888-888888888888".to_string(),
                sequence: 1,
                is_latest: true,
                created_at: 8,
            }],
        };

        let graph = ThreadGraph::from_projection(bundle, rows, GraphObjectDetails::default());
        let kinds = graph.lines.iter().map(|line| line.kind).collect::<Vec<_>>();

        assert_eq!(
            kinds,
            vec![
                GraphNodeKind::Intent,
                GraphNodeKind::Plan,
                GraphNodeKind::Task,
                GraphNodeKind::Run,
                GraphNodeKind::Patchset,
            ]
        );
        assert!(graph.lines[1].tags.contains(&"selected".to_string()));
        assert!(graph.lines[2].tags.contains(&"active".to_string()));
        assert!(graph.lines[3].tags.contains(&"completed".to_string()));
    }

    #[test]
    fn to_json_serializes_metadata_and_nodes() {
        let graph = ThreadGraph {
            thread_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            title: Some("demo".into()),
            freshness: "fresh".into(),
            thread_version: 3,
            scheduler_version: 2,
            updated_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            selected_plan_id: Some(
                Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap(),
            ),
            active_task_id: None,
            active_run_id: None,
            code_ui_status: None,
            code_ui_transcript_len: 0,
            code_ui_pending_interactions: 0,
            lines: vec![
                GraphLine {
                    depth: 0,
                    kind: GraphNodeKind::Intent,
                    id: "i1".into(),
                    label: "Intent one".into(),
                    tags: vec!["root".into()],
                    detail: vec![("status".into(), "open".into())],
                    object: Some(GraphObjectDetail {
                        object_type: "intent".into(),
                        hash: Some("abc123".into()),
                        git_object_type: Some("blob".into()),
                        summary: vec![("kind".into(), "intent".into())],
                        raw_json_lines: Vec::new(),
                    }),
                },
                GraphLine {
                    depth: 1,
                    kind: GraphNodeKind::Plan,
                    id: "p1".into(),
                    label: "Plan".into(),
                    tags: Vec::new(),
                    detail: Vec::new(),
                    object: None,
                },
            ],
        };

        let json = graph.to_json();
        assert_eq!(json["thread_id"], "11111111-1111-4111-8111-111111111111");
        assert_eq!(json["title"], "demo");
        assert_eq!(json["thread_version"], 3);
        assert_eq!(
            json["selected_plan_id"],
            "33333333-3333-4333-8333-333333333333"
        );
        assert_eq!(json["active_task_id"], serde_json::Value::Null);

        let nodes = json["nodes"].as_array().expect("nodes is an array");
        assert_eq!(nodes.len(), 2);
        // Node kinds use the lowercase history-type names.
        assert_eq!(nodes[0]["kind"], "intent");
        assert_eq!(nodes[0]["label"], "Intent one");
        assert_eq!(nodes[0]["tags"][0], "root");
        assert_eq!(nodes[0]["detail"]["status"], "open");
        assert_eq!(nodes[0]["object"]["hash"], "abc123");
        assert_eq!(nodes[0]["object"]["summary"]["kind"], "intent");
        // A node with no underlying object serializes `object` as null.
        assert_eq!(nodes[1]["kind"], "plan");
        assert_eq!(nodes[1]["object"], serde_json::Value::Null);
    }

    #[test]
    fn tree_shape_uses_unicode_branch_glyphs() {
        // Lines:
        //  depth 0: Intent (root, only one)         → no prefix, no glyph
        //  depth 1: Plan A (not last)               → "├─ "
        //  depth 2: Task A1 (last among A's tasks)  → "│  └─ "
        //  depth 1: Plan B (last)                   → "└─ "
        let lines = vec![
            GraphLine {
                depth: 0,
                kind: GraphNodeKind::Intent,
                id: "intent".into(),
                label: "intent".into(),
                tags: Vec::new(),
                detail: Vec::new(),
                object: None,
            },
            GraphLine {
                depth: 1,
                kind: GraphNodeKind::Plan,
                id: "plan-a".into(),
                label: "plan-a".into(),
                tags: Vec::new(),
                detail: Vec::new(),
                object: None,
            },
            GraphLine {
                depth: 2,
                kind: GraphNodeKind::Task,
                id: "task-a1".into(),
                label: "task-a1".into(),
                tags: Vec::new(),
                detail: Vec::new(),
                object: None,
            },
            GraphLine {
                depth: 1,
                kind: GraphNodeKind::Plan,
                id: "plan-b".into(),
                label: "plan-b".into(),
                tags: Vec::new(),
                detail: Vec::new(),
                object: None,
            },
        ];

        let shapes = compute_tree_shape(&lines);
        // Single root → ancestor_is_last[0] = true, so depth-1 children render
        // with three-space gap (no spine drawn at depth 0).
        assert_eq!(shapes[0].prefix, "");
        assert_eq!(shapes[0].glyph, "");
        assert_eq!(shapes[1].prefix, "   ");
        assert_eq!(shapes[1].glyph, "├─ ");
        // Plan A is NOT the last sibling, so depth-2 children render with a
        // vertical spine "│  " under it.
        assert_eq!(shapes[2].prefix, "   │  ");
        assert_eq!(shapes[2].glyph, "└─ ");
        assert_eq!(shapes[3].prefix, "   ");
        assert_eq!(shapes[3].glyph, "└─ ");
    }

    #[test]
    fn graph_body_layout_splits_into_three_panes() {
        let [tree, list, detail] = split_graph_body(Rect::new(0, 0, 120, 20));

        // Each pane gets a non-trivial slice and they tile horizontally.
        assert!(tree.width >= 30);
        assert!(list.width >= 28);
        assert!(detail.width >= 40);
        assert_eq!(tree.x, 0);
        assert_eq!(list.x, tree.x + tree.width);
        assert_eq!(detail.x, list.x + list.width);
        assert_eq!(detail.x + detail.width, 120);
    }

    #[test]
    fn render_tree_row_includes_branch_glyph_caret_status_kind_and_tags() {
        let lines = vec![
            GraphLine {
                depth: 0,
                kind: GraphNodeKind::Intent,
                id: "intent".into(),
                label: "intent-label".into(),
                tags: Vec::new(),
                detail: Vec::new(),
                object: None,
            },
            GraphLine {
                depth: 1,
                kind: GraphNodeKind::Run,
                id: "run".into(),
                label: "run-label".into(),
                tags: vec!["latest".into(), "completed".into()],
                detail: Vec::new(),
                object: None,
            },
        ];

        let graph = ThreadGraph {
            thread_id: id("11111111-1111-4111-8111-111111111111"),
            title: None,
            freshness: "Fresh".into(),
            thread_version: 1,
            scheduler_version: 1,
            updated_at: ts(1),
            selected_plan_id: None,
            active_task_id: None,
            active_run_id: None,
            code_ui_status: None,
            code_ui_transcript_len: 0,
            code_ui_pending_interactions: 0,
            lines,
        };
        let app = GraphTuiApp::new(graph);

        let rendered = render_tree_row(&app, 1, true);
        let text = rendered
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(
            text.contains("└─ "),
            "expected last-sibling glyph in {text:?}"
        );
        // Caret for leaf is two spaces (no children); status dot is the
        // succeeded glyph because the line carries the "completed" event tag.
        assert!(text.contains(STATUS_SUCCEEDED.glyph), "expected ok dot");
        assert!(text.contains("[run     ]"), "expected padded kind tag");
        assert!(text.contains("‹latest›"), "expected latest tag pill");
        assert!(text.contains("‹completed›"), "expected event tag pill");
    }

    #[test]
    fn graph_details_include_persisted_object_content() {
        let detail = GraphObjectDetail::from_json(
            GraphNodeKind::Task,
            Some("abc123".to_string()),
            Some("Blob".to_string()),
            serde_json::json!({
                "object_id": "44444444-4444-4444-8444-444444444444",
                "object_type": "task",
                "title": "Render graph object details",
                "description": "Show the stored task object, not just projection links",
                "constraints": ["keep graph responsive"],
                "acceptance_criteria": ["details panel includes object_json"]
            }),
        );
        let line = GraphLine {
            depth: 1,
            kind: GraphNodeKind::Task,
            id: "44444444-4444-4444-8444-444444444444".to_string(),
            label: "44444444".to_string(),
            tags: Vec::new(),
            detail: vec![(
                "task_id".to_string(),
                "44444444-4444-4444-8444-444444444444".to_string(),
            )],
            object: Some(detail),
        };

        let rendered = detail_lines_for_width(Some(&line), usize::MAX)
            .into_iter()
            .flat_map(|line| line.spans.into_iter())
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("object_hash: "));
        assert!(rendered.contains("abc123"));
        assert!(rendered.contains("title: "));
        assert!(rendered.contains("Render graph object details"));
        // Section headers in the new design use centered banners
        // ("── OBJECT JSON ──") instead of `object_json:`.
        assert!(rendered.contains("OBJECT JSON"));
        assert!(rendered.contains("Show the stored task object"));
    }

    #[test]
    fn graph_object_json_detail_preserves_pretty_indentation() {
        let detail = GraphObjectDetail::from_json(
            GraphNodeKind::Patchset,
            Some("abc123".to_string()),
            Some("Blob".to_string()),
            serde_json::json!({
                "object_id": "88888888-8888-4888-8888-888888888888",
                "artifact": {
                    "key": "9e0414b625957df8834d25dc612959b3851ac4e",
                    "store": "libra"
                },
                "touched": [
                    {
                        "change_type": "modify",
                        "path": "Cargo.lock"
                    }
                ]
            }),
        );
        let line = GraphLine {
            depth: 1,
            kind: GraphNodeKind::Patchset,
            id: "88888888-8888-4888-8888-888888888888".to_string(),
            label: "88888888".to_string(),
            tags: Vec::new(),
            detail: vec![(
                "patchset_id".to_string(),
                "88888888-8888-4888-8888-888888888888".to_string(),
            )],
            object: Some(detail),
        };

        let rendered = detail_lines_for_width(Some(&line), 80)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert!(
            rendered.iter().any(|line| line == "  \"artifact\": {"),
            "expected nested object to keep two-space JSON indentation: {rendered:#?}"
        );
        assert!(
            rendered
                .iter()
                .any(|line| line == "    \"key\": \"9e0414b625957df8834d25dc612959b3851ac4e\","),
            "expected nested object field to keep four-space JSON indentation: {rendered:#?}"
        );
        assert!(
            rendered
                .iter()
                .any(|line| line == "      \"change_type\": \"modify\","),
            "expected nested array object field to keep six-space JSON indentation: {rendered:#?}"
        );
    }

    #[test]
    fn graph_detail_lines_wrap_long_and_wide_text_to_panel_width() {
        let line = GraphLine {
            depth: 1,
            kind: GraphNodeKind::Task,
            id: "44444444-4444-4444-8444-444444444444".to_string(),
            label: "44444444".to_string(),
            tags: Vec::new(),
            detail: vec![
                (
                    "object_hash".to_string(),
                    "5ed4uc979f5d0b64126a4c1209b5d5d14824297".to_string(),
                ),
                (
                    "title".to_string(),
                    "初始化 Rust 项目并实现 CLI 子命令".to_string(),
                ),
                ("acceptance_criteria".to_string(), "array[3]".to_string()),
            ],
            object: None,
        };

        let texts = detail_lines_for_width(Some(&line), 32)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert!(texts.iter().any(|line| line.contains("object_hash:")));
        assert!(texts.iter().any(|line| line.contains("初始化 Rust 项目")));
        assert!(texts.iter().any(|line| line == "acceptance_criteria:"));
        for text in texts {
            assert!(
                display_width(&text) <= 32,
                "detail line exceeded panel width: {text:?}"
            );
        }
    }

    #[test]
    fn page_up_down_scroll_details_without_changing_selection() {
        let graph = ThreadGraph {
            thread_id: id("11111111-1111-4111-8111-111111111111"),
            title: None,
            freshness: "Fresh".to_string(),
            thread_version: 1,
            scheduler_version: 1,
            updated_at: ts(1),
            selected_plan_id: None,
            active_task_id: None,
            active_run_id: None,
            code_ui_status: None,
            code_ui_transcript_len: 0,
            code_ui_pending_interactions: 0,
            lines: vec![
                GraphLine {
                    depth: 0,
                    kind: GraphNodeKind::Intent,
                    id: "22222222-2222-4222-8222-222222222222".to_string(),
                    label: "22222222".to_string(),
                    tags: Vec::new(),
                    detail: Vec::new(),
                    object: None,
                },
                GraphLine {
                    depth: 1,
                    kind: GraphNodeKind::Task,
                    id: "44444444-4444-4444-8444-444444444444".to_string(),
                    label: "44444444".to_string(),
                    tags: Vec::new(),
                    detail: Vec::new(),
                    object: None,
                },
            ],
        };
        let mut app = GraphTuiApp::new(graph);
        app.select_next();
        app.detail_page_size = 5;

        app.scroll_details_page_down();
        assert_eq!(app.selected, 1);
        assert_eq!(app.detail_scroll, 5);

        app.scroll_details_page_up();
        assert_eq!(app.selected, 1);
        assert_eq!(app.detail_scroll, 0);
    }

    #[test]
    fn graph_overlay_selects_newest_matching_session_and_emits_wire_status() {
        use crate::internal::ai::{
            session::{
                CodeCommandIdentity, CodeCommandIntent, CodeWorkflowEventKind, SessionJsonlStore,
                SessionState, SessionStore,
            },
            web::code_ui::CodeUiSessionStatus,
        };

        let temp = tempfile::TempDir::new().expect("temp storage");
        let storage_root = temp.path();
        let session_store = SessionStore::from_storage_path(storage_root);
        let thread_id = id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");

        let mut stale = SessionState::new("/repo");
        stale.metadata.insert(
            "thread_id".to_string(),
            serde_json::json!(thread_id.to_string()),
        );
        session_store.save(&stale).expect("save stale session");
        SessionJsonlStore::new(session_store.session_root(&stale.id))
            .append_code_workflow(CodeWorkflowEventKind::CommandAccepted {
                command_id: "stale".to_string(),
                workflow: "idle".to_string(),
            })
            .expect("append stale event");

        // Ensure the newer session's event log has a later mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));

        let mut fresh = SessionState::new("/repo");
        fresh.metadata.insert(
            "thread_id".to_string(),
            serde_json::json!(thread_id.to_string()),
        );
        session_store.save(&fresh).expect("save fresh session");
        let fresh_store = SessionJsonlStore::new(session_store.session_root(&fresh.id));
        let intent = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", &fresh.id, "principal", "interrupted"),
            "agent_runtime_turn",
            "sha256:request",
            true,
        );
        fresh_store
            .admit_code_command(intent)
            .expect("admit mutating intent");
        // Do not recover/fence here: graph must surface Pending mutations as
        // indeterminate_side_effect before a runtime restart writes the fence.

        let (status, _transcript_len, _pending) =
            load_code_ui_overlay_for_thread(storage_root, thread_id)
                .expect("overlay load")
                .expect("matching session must produce an overlay");
        assert_eq!(status, CodeUiSessionStatus::IndeterminateSideEffect);

        let mut graph = ThreadGraph {
            thread_id,
            title: None,
            freshness: "Fresh".into(),
            thread_version: 1,
            scheduler_version: 1,
            updated_at: ts(1),
            selected_plan_id: None,
            active_task_id: None,
            active_run_id: None,
            code_ui_status: None,
            code_ui_transcript_len: 0,
            code_ui_pending_interactions: 0,
            lines: Vec::new(),
        };
        graph = graph.with_code_ui_overlay(status, 0, 0);
        let json = graph.to_json();
        assert_eq!(
            json.get("code_ui_status").and_then(|value| value.as_str()),
            Some("indeterminate_side_effect")
        );
    }
}
