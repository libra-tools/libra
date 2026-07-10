//! `libra agent checkpoint …` subcommands. V1 ships read-only `list` /
//! `show`; `rewind --apply` restores the worktree and dispatches optional
//! transcript truncation for agent kinds that implement `TranscriptTruncator`.

use std::{path::Path, str::FromStr};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        commit::Commit,
        tree::{Tree, TreeItem, TreeItemMode},
    },
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde::Serialize;

use super::{CheckpointListArgs, CheckpointRewindArgs, CheckpointShowArgs, CheckpointSubcommand};
use crate::{
    command::load_object,
    internal::{ai::history::parse_content_hash, db::get_db_conn_instance},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        object::read_git_object_bounded,
        object_ext::TreeExt,
        output::{OutputConfig, emit_json_data},
        util,
    },
};

pub async fn execute_safe(cmd: CheckpointSubcommand, output: &OutputConfig) -> CliResult<()> {
    match cmd {
        CheckpointSubcommand::List(args) => list(args, output).await,
        CheckpointSubcommand::Show(args) => show(args, output).await,
        CheckpointSubcommand::Rewind(args) => rewind(args, output).await,
        CheckpointSubcommand::Export(args) => export(args, output).await,
    }
}

#[derive(Debug, Serialize)]
struct CheckpointRow {
    checkpoint_id: String,
    session_id: String,
    scope: String,
    /// Nullable in the schema since the `2026050501` follow-up — stays
    /// `Option<String>` end-to-end so JSON consumers can distinguish a
    /// missing parent from an empty string.
    parent_commit: Option<String>,
    tree_oid: String,
    metadata_blob_oid: String,
    traces_commit: String,
    created_at: i64,
}

// ---------------------------------------------------------------------------
// AG-20 keyset pagination (shared by `checkpoint list` and `session list`)
// ---------------------------------------------------------------------------

/// Default page size for `agent checkpoint list` / `agent session list`.
pub(super) const PAGE_LIMIT_DEFAULT: u64 = 50;

/// Hard cap for `--limit`. Larger requests clamp (with a stderr note) so a
/// stray `--limit 1000000` cannot regress the metadata-first listing into
/// an unbounded scan.
pub(super) const PAGE_LIMIT_MAX: u64 = 500;

/// `schema_version` of the paged list JSON `data` payload (additive
/// evolution only — mirrors the `agent list --json` precedent).
pub(super) const PAGE_SCHEMA_VERSION: u32 = 1;

/// Resolve the effective page size: default 50, hard cap 500, `--limit 0`
/// treated as 1 so the smallest page is still a page. Returns the limit
/// plus an optional clamp note the caller prints to stderr (kept out of
/// this helper so unit tests can assert on it).
pub(super) fn resolve_page_limit(requested: Option<u64>) -> (u64, Option<String>) {
    match requested {
        None => (PAGE_LIMIT_DEFAULT, None),
        Some(0) => (1, None),
        Some(n) if n > PAGE_LIMIT_MAX => (
            PAGE_LIMIT_MAX,
            Some(format!(
                "note: --limit {n} exceeds the maximum page size of {PAGE_LIMIT_MAX}; \
                 clamping to {PAGE_LIMIT_MAX}"
            )),
        ),
        Some(n) => (n, None),
    }
}

/// Encode a keyset cursor: opaque base64 of `v1:<timestamp>:<row_id>`.
///
/// The page order is `(timestamp DESC, id ASC)` — exactly the column shape
/// of the `2026070802_agent_checkpoint_paging` indexes
/// (`agent_session(started_at DESC, session_id)` /
/// `agent_checkpoint(created_at DESC, checkpoint_id)`), so every cursored
/// page is a pure index SEARCH with no sort step (see the EXPLAIN QUERY
/// PLAN assertions in `tests/agent_checkpoint_reader_test.rs`). The id is
/// the unique tiebreaker for rows sharing a timestamp; consumers must
/// treat the cursor as opaque and round-trip it verbatim.
pub(super) fn encode_page_cursor(timestamp: i64, id: &str) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.encode(format!("v1:{timestamp}:{id}"))
}

/// Decode an opaque `--cursor` value back into `(timestamp, id)`. Any
/// malformation (bad base64, non-UTF-8, wrong version tag, non-numeric
/// timestamp, empty id) fails closed with one actionable usage error —
/// a corrupted cursor must never silently restart the listing.
pub(super) fn decode_page_cursor(cursor: &str) -> CliResult<(i64, String)> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let malformed = || {
        CliError::command_usage(format!(
            "invalid --cursor '{cursor}': pass the opaque next_cursor value from the \
             previous page's output unmodified (cursors cannot be hand-built)"
        ))
    };
    let decoded = STANDARD.decode(cursor.trim()).map_err(|_| malformed())?;
    let text = String::from_utf8(decoded).map_err(|_| malformed())?;
    let rest = text.strip_prefix("v1:").ok_or_else(malformed)?;
    let (timestamp, id) = rest.split_once(':').ok_or_else(malformed)?;
    let timestamp: i64 = timestamp.parse().map_err(|_| malformed())?;
    if id.is_empty() {
        return Err(malformed());
    }
    Ok((timestamp, id.to_string()))
}

/// Build the paginated `checkpoint list` SQL. Extracted so the in-file
/// EXPLAIN QUERY PLAN test runs the exact production statement against
/// the `idx_agent_checkpoint_created_paging` index (never a table SCAN,
/// never a temp B-tree). Placeholder order: `[session_id,] [created_at,
/// created_at, checkpoint_id,] limit`.
pub(super) fn checkpoint_page_sql(with_session_filter: bool, with_cursor: bool) -> String {
    let mut sql = String::from(
        "SELECT checkpoint_id, session_id, scope, parent_commit, tree_oid, \
                metadata_blob_oid, traces_commit, created_at \
         FROM agent_checkpoint WHERE 1=1",
    );
    if with_session_filter {
        sql.push_str(" AND session_id = ?");
    }
    if with_cursor {
        sql.push_str(" AND (created_at < ? OR (created_at = ? AND checkpoint_id > ?))");
    }
    sql.push_str(" ORDER BY created_at DESC, checkpoint_id ASC LIMIT ?");
    sql
}

/// One page of `checkpoint list` output. The JSON `data` payload carries
/// the rows under `checkpoints` (per-row schema unchanged from the
/// pre-pagination output) plus `next_cursor` — the opaque `--cursor`
/// token for the next page, `null` once the listing is exhausted.
#[derive(Debug, Serialize)]
struct CheckpointListPage {
    schema_version: u32,
    checkpoints: Vec<CheckpointRow>,
    next_cursor: Option<String>,
}

async fn list(args: CheckpointListArgs, output: &OutputConfig) -> CliResult<()> {
    let (limit, clamp_note) = resolve_page_limit(args.limit);
    if let Some(note) = &clamp_note {
        eprintln!("{note}");
    }
    // Decode the cursor before touching the database so a malformed value
    // is a pure usage error.
    let cursor = args.cursor.as_deref().map(decode_page_cursor).transpose()?;

    let conn = get_db_conn_instance().await;
    if !table_exists(&conn, "agent_checkpoint").await? {
        return emit_list(
            &CheckpointListPage {
                schema_version: PAGE_SCHEMA_VERSION,
                checkpoints: Vec::new(),
                next_cursor: None,
            },
            output,
        );
    }
    let backend = conn.get_database_backend();

    let sql = checkpoint_page_sql(args.session.is_some(), cursor.is_some());
    let mut values: Vec<sea_orm::Value> = Vec::new();
    if let Some(session) = &args.session {
        values.push(session.clone().into());
    }
    if let Some((timestamp, id)) = &cursor {
        values.push((*timestamp).into());
        values.push((*timestamp).into());
        values.push(id.clone().into());
    }
    // Fetch one row beyond the page to learn whether another page exists
    // without a second COUNT query.
    values.push((limit as i64 + 1).into());

    let rows = conn
        .query_all(Statement::from_sql_and_values(backend, &sql, values))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_checkpoint: {e}")))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(CheckpointRow {
            checkpoint_id: row.try_get_by("checkpoint_id").unwrap_or_default(),
            session_id: row.try_get_by("session_id").unwrap_or_default(),
            scope: row.try_get_by("scope").unwrap_or_default(),
            parent_commit: row.try_get_by("parent_commit").ok().flatten(),
            tree_oid: row.try_get_by("tree_oid").unwrap_or_default(),
            metadata_blob_oid: row.try_get_by("metadata_blob_oid").unwrap_or_default(),
            traces_commit: row.try_get_by("traces_commit").unwrap_or_default(),
            created_at: row.try_get_by("created_at").unwrap_or_default(),
        });
    }
    let next_cursor = if out.len() as u64 > limit {
        out.truncate(limit as usize);
        out.last()
            .map(|row| encode_page_cursor(row.created_at, &row.checkpoint_id))
    } else {
        None
    };
    emit_list(
        &CheckpointListPage {
            schema_version: PAGE_SCHEMA_VERSION,
            checkpoints: out,
            next_cursor,
        },
        output,
    )
}

async fn show(args: CheckpointShowArgs, output: &OutputConfig) -> CliResult<()> {
    let conn = get_db_conn_instance().await;
    if !table_exists(&conn, "agent_checkpoint").await? {
        return Err(CliError::fatal(format!(
            "no checkpoint matches '{}': agent_checkpoint table not yet present (run `libra init`?)",
            args.checkpoint_id
        )));
    }
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id, session_id, scope, parent_commit, tree_oid, \
                    metadata_blob_oid, traces_commit, created_at \
             FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
            [args.checkpoint_id.clone().into()],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_checkpoint: {e}")))?;
    match row {
        Some(row) => {
            let payload = CheckpointRow {
                checkpoint_id: row.try_get_by("checkpoint_id").unwrap_or_default(),
                session_id: row.try_get_by("session_id").unwrap_or_default(),
                scope: row.try_get_by("scope").unwrap_or_default(),
                parent_commit: row.try_get_by("parent_commit").ok().flatten(),
                tree_oid: row.try_get_by("tree_oid").unwrap_or_default(),
                metadata_blob_oid: row.try_get_by("metadata_blob_oid").unwrap_or_default(),
                traces_commit: row.try_get_by("traces_commit").unwrap_or_default(),
                created_at: row.try_get_by("created_at").unwrap_or_default(),
            };
            // Best-effort metadata blob load: if the user is in a libra
            // workspace, read the metadata.json blob and surface it; if
            // path resolution fails (e.g. running from outside any libra
            // repo), fall back to the row-only render rather than erroring.
            let metadata = load_metadata_blob(&payload.metadata_blob_oid).ok();
            // Metadata-first layout classification (AG-20): walk the
            // checkpoint tree + manifest only — transcript blob bodies are
            // NEVER read here. Any resolution failure degrades to layout
            // "unknown" instead of failing the show.
            let layout = summarize_checkpoint_layout(&payload);
            emit_one(&payload, metadata.as_deref(), &layout, output)
        }
        None => Err(CliError::fatal(format!(
            "no checkpoint matches id '{}'",
            args.checkpoint_id
        ))),
    }
}

/// `libra agent checkpoint rewind <id> [--dry-run|--apply]`.
///
/// `dry-run` (the default when neither flag is set) lists the files the
/// checkpoint's `parent_commit` snapshot would restore, without touching the
/// worktree. `--apply` actually runs the worktree restore (delegating to the
/// existing `restore --source <parent_commit>` path), truncates supported
/// agent transcripts when possible, and leaves HEAD plus `refs/heads/*`
/// untouched per `docs/development/commands/_general.md` §7.3.
async fn rewind(args: CheckpointRewindArgs, output: &OutputConfig) -> CliResult<()> {
    let conn = get_db_conn_instance().await;
    if !table_exists(&conn, "agent_checkpoint").await? {
        return Err(CliError::fatal(format!(
            "no checkpoint matches '{}': agent_checkpoint table not yet present (run `libra init`?)",
            args.checkpoint_id
        )));
    }
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT parent_commit, traces_commit FROM agent_checkpoint \
             WHERE checkpoint_id = ? LIMIT 1",
            [args.checkpoint_id.clone().into()],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_checkpoint: {e}")))?
        .ok_or_else(|| {
            CliError::fatal(format!("no checkpoint matches id '{}'", args.checkpoint_id))
        })?;

    let parent_commit: Option<String> = row.try_get_by("parent_commit").ok().flatten();
    let traces_commit: String = row.try_get_by("traces_commit").unwrap_or_default();

    // Without a parent_commit (unborn HEAD at ingest time) there is nothing
    // to restore the worktree to. Surface a clear diagnostic rather than a
    // silent no-op.
    let parent_commit = match parent_commit {
        Some(c) if !c.is_empty() => c,
        _ => {
            return Err(CliError::fatal(format!(
                "checkpoint '{}' has no recorded parent_commit (unborn HEAD or pre-commit ingest); \
                 nothing to rewind to. checkpoint commit: {traces_commit}",
                args.checkpoint_id
            )));
        }
    };

    // Resolve the parent commit's tree and enumerate files that would be
    // restored. We use this both for dry-run output and for a "summary
    // before apply" line.
    let parent_oid = ObjectHash::from_str(&parent_commit).map_err(|e| {
        // A0-03: a malformed parent_commit in the catalog is a checkpoint
        // store inconsistency (the writer only records valid OIDs).
        CliError::fatal(format!(
            "checkpoint '{}' has invalid parent_commit '{parent_commit}': {e}",
            args.checkpoint_id
        ))
        .with_stable_code(StableErrorCode::AgentCheckpointStoreInconsistent)
    })?;
    // Codex Phase-2-followups round-1 P1 #2: dry-run was previously
    // emitting only the additions/modifications side, leaving users
    // surprised when `--apply` also DELETED tracked files that were absent
    // from the target commit. The plan now surfaces both sides:
    //   restore = files in the target commit's tree (will be written)
    //   delete  = files tracked by the index but absent from the target
    //             tree (will be removed by the worktree-restore pass)
    let plan = build_rewind_plan(&parent_oid).map_err(|e| {
        // A0-03: rewind cannot resolve the parent commit / tree — the
        // referenced objects are missing from the store, a checkpoint-store
        // inconsistency (recovery failure), not a user input error.
        CliError::fatal(format!("failed to enumerate files for rewind preview: {e}"))
            .with_stable_code(StableErrorCode::AgentCheckpointStoreInconsistent)
    })?;

    // Codex round-2 follow-up: report `transcript_truncation_supported`
    // based on the actual `agent_kind` for this checkpoint, not a flat
    // `true`. Only `claude_code` has a TranscriptTruncator adapter today;
    // other kinds dispatch to `SkippedUnsupportedKind` at apply time, so
    // dry-run should mirror that.
    let truncation_supported = lookup_truncation_support(&conn, &args.checkpoint_id)
        .await
        .unwrap_or(false);

    if !args.apply {
        // dry-run path. We arrived here because either `--dry-run` was
        // explicit or neither flag was passed.
        if output.is_json() {
            let payload = serde_json::json!({
                "checkpoint_id": args.checkpoint_id,
                "parent_commit": parent_commit,
                "traces_commit": traces_commit,
                "would_restore_paths": plan.restore,
                "would_delete_paths": plan.delete,
                "applied": false,
                "transcript_truncation_supported": truncation_supported,
            });
            return emit_json_data("agent_checkpoint_rewind", &payload, output);
        }
        if output.quiet {
            return Ok(());
        }
        println!("Dry run — no files modified.");
        println!("checkpoint_id : {}", args.checkpoint_id);
        println!("parent_commit : {parent_commit}");
        println!("traces_commit : {traces_commit}");
        println!("would restore {} path(s):", plan.restore.len());
        for path in &plan.restore {
            println!("  + {path}");
        }
        println!("would delete  {} path(s):", plan.delete.len());
        for path in &plan.delete {
            println!("  - {path}");
        }
        println!(
            "Re-run with --apply to restore the working tree. For Claude \
             Code sessions the agent's transcript will be truncated to \
             the checkpoint boundary; other agent kinds keep the transcript \
             untouched."
        );
        return Ok(());
    }

    // --apply path: drive the typed restore for working-tree only,
    // matching the dry-run preview's file set. Re-using `restore` keeps
    // the LFS / index / pathspec semantics consistent with the rest of
    // the CLI.
    use crate::command::restore::{RestoreArgs, execute_checked_typed};
    let restore_args = RestoreArgs {
        overlay: false,
        no_overlay: false,
        ours: false,
        theirs: false,
        ignore_unmerged: false,
        merge: false,
        conflict: None,
        pathspec: vec![".".to_string()],
        source: Some(parent_commit.clone()),
        worktree: true,
        staged: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_progress: false,
    };
    execute_checked_typed(restore_args)
        .await
        .map_err(|e| CliError::fatal(format!("rewind --apply failed: {e}")))?;

    // Phase 4.1 (entire.md §14.4 item 1): if the captured agent has a
    // `TranscriptTruncator` adapter, call it to drop transcript lines
    // whose timestamp is strictly after the checkpoint boundary. This
    // closes the v1 caveat that the agent's local transcript was left
    // dangling after a worktree rewind.
    let truncation_outcome = truncate_agent_transcript_for_checkpoint(&args.checkpoint_id).await;

    if output.is_json() {
        let payload = serde_json::json!({
            "checkpoint_id": args.checkpoint_id,
            "parent_commit": parent_commit,
            "traces_commit": traces_commit,
            "restored_paths": plan.restore,
            "deleted_paths": plan.delete,
            "applied": true,
            "transcript_truncation": truncation_outcome.as_json(),
        });
        return emit_json_data("agent_checkpoint_rewind", &payload, output);
    }
    if !output.quiet {
        println!(
            "Restored {} path(s), deleted {} path(s) from {parent_commit}.",
            plan.restore.len(),
            plan.delete.len()
        );
        match &truncation_outcome {
            TranscriptTruncationOutcome::Truncated {
                path,
                lines_dropped,
            } => {
                println!(
                    "Truncated transcript {}: {} line(s) past the checkpoint dropped.",
                    path, lines_dropped
                );
            }
            TranscriptTruncationOutcome::NoChange { path } => {
                println!(
                    "Transcript {} already aligned with the checkpoint — no changes.",
                    path
                );
            }
            TranscriptTruncationOutcome::SkippedNoPath => {
                println!(
                    "Note: agent_session.metadata_json has no transcript_path; \
                     the agent's local transcript was left untouched."
                );
            }
            TranscriptTruncationOutcome::SkippedUnsupportedKind { agent_kind } => {
                println!(
                    "Note: agent_kind '{}' has no TranscriptTruncator adapter yet; \
                     the agent's local transcript was left untouched.",
                    agent_kind
                );
            }
            TranscriptTruncationOutcome::Failed { reason } => {
                eprintln!(
                    "warning: transcript truncation failed: {reason}. \
                     The worktree restore succeeded; the agent's transcript file \
                     was left as-is."
                );
            }
        }
    }
    Ok(())
}

/// Outcome of attempting transcript truncation alongside `rewind --apply`.
/// We never propagate these as hard errors — the worktree restore is the
/// load-bearing operation; transcript truncation is informational and a
/// failure here should not roll back the user's tree.
enum TranscriptTruncationOutcome {
    Truncated { path: String, lines_dropped: usize },
    NoChange { path: String },
    SkippedNoPath,
    SkippedUnsupportedKind { agent_kind: String },
    Failed { reason: String },
}

impl TranscriptTruncationOutcome {
    fn as_json(&self) -> serde_json::Value {
        // Codex round-4 follow-up: align `supported` semantics across
        // dry-run and apply outputs. `supported` here means "did the
        // truncator actually run end-to-end on this checkpoint?" — same
        // contract as `lookup_truncation_support` in the dry-run path.
        // Skipped paths therefore report `supported: false`; only
        // Truncated/NoChange (which exercised the adapter) and Failed
        // (which started the adapter) report `supported: true`.
        match self {
            Self::Truncated {
                path,
                lines_dropped,
            } => serde_json::json!({
                "supported": true,
                "applied": true,
                "transcript_path": path,
                "lines_dropped": lines_dropped,
            }),
            Self::NoChange { path } => serde_json::json!({
                "supported": true,
                "applied": false,
                "transcript_path": path,
                "reason": "transcript already aligned with checkpoint boundary",
            }),
            Self::SkippedNoPath => serde_json::json!({
                "supported": false,
                "applied": false,
                "reason": "agent_session.metadata_json has no transcript_path",
            }),
            Self::SkippedUnsupportedKind { agent_kind } => serde_json::json!({
                "supported": false,
                "applied": false,
                "agent_kind": agent_kind,
                "reason": "no TranscriptTruncator adapter for this agent_kind",
            }),
            Self::Failed { reason } => serde_json::json!({
                // Adapter was selected and started running but failed
                // mid-stream (e.g. concurrent writer, bad created_at).
                // Adapter IS supported; the apply just did not
                // succeed.
                "supported": true,
                "applied": false,
                "error": reason,
            }),
        }
    }
}

/// Look up the `agent_session` row paired with `checkpoint_id`, decide
/// whether we have an adapter for its `agent_kind`, then invoke the
/// truncator with a boundary derived from `agent_checkpoint.created_at`.
/// Returns the outcome rather than an error so the caller can surface a
/// uniform message no matter the path taken.
async fn truncate_agent_transcript_for_checkpoint(
    checkpoint_id: &str,
) -> TranscriptTruncationOutcome {
    let conn = get_db_conn_instance().await;
    truncate_agent_transcript_for_checkpoint_with_conn(&conn, checkpoint_id).await
}

/// Cheap "will `--apply` actually run a TranscriptTruncator for this
/// checkpoint?" probe used by the dry-run path so its
/// `transcript_truncation_supported` flag matches what `--apply` will
/// actually do.
///
/// Codex round-3 follow-up: this now considers BOTH conditions —
/// `agent_kind == "claude_code"` AND a non-empty `transcript_path`
/// in `metadata_json`. Previously a Claude Code session whose
/// `metadata_json` lacked `transcript_path` would report `supported:
/// true` but apply would short-circuit to `SkippedNoPath`,
/// contradicting the dry-run preview.
async fn lookup_truncation_support(
    conn: &sea_orm::DatabaseConnection,
    checkpoint_id: &str,
) -> Result<bool, sea_orm::DbErr> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT s.agent_kind AS agent_kind, \
                    COALESCE(s.metadata_json, '{}') AS metadata_json \
             FROM agent_checkpoint cp \
             JOIN agent_session s ON s.session_id = cp.session_id \
             WHERE cp.checkpoint_id = ? LIMIT 1",
            [checkpoint_id.into()],
        ))
        .await?;
    let Some(r) = row else {
        return Ok(false);
    };
    let kind: String = r.try_get_by("agent_kind").unwrap_or_default();
    let metadata_json: String = r.try_get_by("metadata_json").unwrap_or_default();
    // Dispatch the truncator-support probe through the v0.17.677
    // capability registry instead of a literal "claude_code" match.
    // Mirrors the dispatch path in
    // `truncate_agent_transcript_for_checkpoint_with_conn` — both
    // sites must answer "would the truncator fire?" the same way so
    // the dry-run preview matches what `--apply` actually does.
    use crate::internal::ai::observed_agents::{AgentKind, truncator_for};
    let truncator_available = AgentKind::from_db_str(&kind)
        .and_then(truncator_for)
        .is_some();
    if !truncator_available {
        return Ok(false);
    }
    let has_transcript_path = serde_json::from_str::<serde_json::Value>(&metadata_json)
        .ok()
        .and_then(|v| {
            v.get("transcript_path")
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .is_some_and(|s| !s.is_empty());
    Ok(has_transcript_path)
}

/// Connection-bound core of [`truncate_agent_transcript_for_checkpoint`].
/// Extracted so fixture tests can run against an in-memory SQLite without
/// the process-wide `get_db_conn_instance` singleton.
async fn truncate_agent_transcript_for_checkpoint_with_conn(
    conn: &sea_orm::DatabaseConnection,
    checkpoint_id: &str,
) -> TranscriptTruncationOutcome {
    use crate::internal::ai::observed_agents::{
        rfc3339_boundary_for_unix_seconds, write_truncated_transcript,
    };

    let backend = conn.get_database_backend();

    // Pull the session join for this checkpoint. We need:
    //  - agent_kind (to dispatch),
    //  - metadata_json (to find transcript_path) — coalesced to '{}'
    //    so legacy rows with NULL values don't error the SELECT
    //    (Codex round-1 P4 follow-up),
    //  - created_at on the checkpoint (the boundary).
    let row = match conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT s.agent_kind AS agent_kind, \
                    COALESCE(s.metadata_json, '{}') AS metadata_json, \
                    cp.created_at AS created_at \
             FROM agent_checkpoint cp \
             JOIN agent_session s ON s.session_id = cp.session_id \
             WHERE cp.checkpoint_id = ? LIMIT 1",
            [checkpoint_id.into()],
        ))
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return TranscriptTruncationOutcome::Failed {
                reason: format!(
                    "no agent_session join for checkpoint '{checkpoint_id}' \
                     (catalog row missing or schema mismatch)"
                ),
            };
        }
        Err(err) => {
            return TranscriptTruncationOutcome::Failed {
                reason: format!("agent_session lookup failed: {err}"),
            };
        }
    };
    let agent_kind: String = row.try_get_by("agent_kind").unwrap_or_default();
    let metadata_json: String = row.try_get_by("metadata_json").unwrap_or_default();
    let created_at: i64 = row.try_get_by("created_at").unwrap_or(0);

    let transcript_path: Option<String> = serde_json::from_str::<serde_json::Value>(&metadata_json)
        .ok()
        .and_then(|v| {
            v.get("transcript_path")
                .and_then(|s| s.as_str())
                .map(str::to_string)
        });
    let Some(path_str) = transcript_path else {
        return TranscriptTruncationOutcome::SkippedNoPath;
    };
    let path = std::path::PathBuf::from(&path_str);

    // Dispatch the truncator through the v0.17.677 capability registry
    // instead of a hard-coded `kind == "claude_code"` literal. The
    // registry handles three failure shapes:
    //   * `AgentKind::from_db_str` fails for unknown tags (schema
    //     mismatch — unsupported kind for this row).
    //   * `truncator_for` returns `None` for kinds whose adapter
    //     doesn't implement `TranscriptTruncator` (the six non-Claude
    //     kinds today). Adding a second truncator implementation is a
    //     single-arm change in `observed_agents::mod.rs::truncator_for`
    //     and the new kind is dispatched here automatically.
    use crate::internal::ai::observed_agents::{AgentKind, truncator_for};
    let Some(parsed_kind) = AgentKind::from_db_str(&agent_kind) else {
        return TranscriptTruncationOutcome::SkippedUnsupportedKind { agent_kind };
    };
    let Some(agent) = truncator_for(parsed_kind) else {
        return TranscriptTruncationOutcome::SkippedUnsupportedKind { agent_kind };
    };
    // Capture the file size at read time so `write_truncated_transcript`
    // (and the NoChange early-return below) can detect a concurrent
    // writer that grew the file before our rename. Codex round-1 P2 +
    // round-2 follow-up.
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return TranscriptTruncationOutcome::Failed {
                reason: format!("transcript file '{path_str}' not found"),
            };
        }
        Err(err) => {
            return TranscriptTruncationOutcome::Failed {
                reason: format!("read transcript '{path_str}': {err}"),
            };
        }
    };
    let size_at_read = bytes.len() as u64;
    // Codex round-2 follow-up: invalid `created_at` propagates as a
    // `Failed` outcome rather than silently degrading to the Unix epoch
    // (which would erase the whole transcript next time around).
    let boundary = match rfc3339_boundary_for_unix_seconds(created_at) {
        Ok(b) => b,
        Err(err) => {
            return TranscriptTruncationOutcome::Failed {
                reason: format!("rfc3339_boundary_for_unix_seconds: {err}"),
            };
        }
    };
    let truncated = match agent.truncate_transcript(&bytes, &boundary) {
        Ok(t) => t,
        Err(err) => {
            return TranscriptTruncationOutcome::Failed {
                reason: format!("truncate_transcript: {err}"),
            };
        }
    };
    if truncated == bytes {
        // Codex round-2 follow-up: even on the no-change path, re-stat
        // the original to make sure no concurrent writer appended new
        // bytes between our read and now. If the file grew, we still
        // should not return "already aligned" — those new bytes might
        // be post-boundary and the user expects them dropped.
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() != size_at_read => {
                return TranscriptTruncationOutcome::Failed {
                    reason: format!(
                        "transcript '{path_str}' grew from {} to {} bytes during \
                         truncation (concurrent writer); rerun once the agent is idle",
                        size_at_read,
                        meta.len()
                    ),
                };
            }
            Ok(_) => {}
            Err(err) => {
                return TranscriptTruncationOutcome::Failed {
                    reason: format!("re-stat transcript '{path_str}': {err}"),
                };
            }
        }
        return TranscriptTruncationOutcome::NoChange { path: path_str };
    }
    let lines_before = bytes.iter().filter(|&&b| b == b'\n').count();
    let lines_after = truncated.iter().filter(|&&b| b == b'\n').count();
    let lines_dropped = lines_before.saturating_sub(lines_after);
    if let Err(err) = write_truncated_transcript(&path, &truncated, Some(size_at_read)) {
        return TranscriptTruncationOutcome::Failed {
            reason: format!("write_truncated_transcript: {err}"),
        };
    }
    TranscriptTruncationOutcome::Truncated {
        path: path_str,
        lines_dropped,
    }
}

/// Files affected by a `rewind --apply`, broken down by side. `restore`
/// = present in the target commit (will be written to the worktree
/// after `--apply`); `delete` = tracked by the index but absent from the
/// target commit's tree (will be removed from the worktree by the
/// underlying restore's deleted-files pass — see
/// `command::restore::restore_worktree_tracked`).
struct RewindPlan {
    restore: Vec<String>,
    delete: Vec<String>,
}

/// Fallible recursive expansion of a tree to `(path, oid)` leaves.
///
/// A0-03: `TreeExt::get_plain_items` recurses via `Tree::load`, which
/// **panics** on a missing / corrupt subtree object. A checkpoint rewind must
/// instead fail closed with `LBR-AGENT-009` when the store is inconsistent, so
/// this variant loads every nested tree through the fallible `Tree::try_load`
/// and returns an error (which `build_rewind_plan`'s caller maps to
/// `AgentCheckpointStoreInconsistent`) rather than aborting the process.
/// Gitlink (`160000`) entries are skipped, mirroring `get_plain_items`.
fn try_expand_tree_plain_items(
    tree: &Tree,
) -> Result<Vec<(std::path::PathBuf, ObjectHash)>, anyhow::Error> {
    use std::path::PathBuf;

    let mut items = Vec::new();
    for item in tree.tree_items.iter() {
        match item.mode {
            TreeItemMode::Commit => continue,
            TreeItemMode::Tree => {
                let sub_tree = Tree::try_load(&item.id).ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing or corrupt subtree object {} ('{}')",
                        item.id,
                        item.name
                    )
                })?;
                for (path, hash) in try_expand_tree_plain_items(&sub_tree)? {
                    items.push((PathBuf::from(item.name.clone()).join(path), hash));
                }
            }
            _ => items.push((PathBuf::from(item.name.clone()), item.id)),
        }
    }
    Ok(items)
}

fn build_rewind_plan(commit_oid: &ObjectHash) -> Result<RewindPlan, anyhow::Error> {
    use std::{collections::HashSet, path::PathBuf};

    use git_internal::internal::index::Index;

    let commit: Commit = load_object(commit_oid)
        .map_err(|e| anyhow::anyhow!("failed to load commit {commit_oid}: {e}"))?;
    let tree: Tree = load_object(&commit.tree_id)
        .map_err(|e| anyhow::anyhow!("failed to load tree {}: {e}", commit.tree_id))?;
    let target: Vec<(PathBuf, ObjectHash)> = try_expand_tree_plain_items(&tree)?;
    let target_set: HashSet<PathBuf> = target.iter().map(|(p, _)| p.clone()).collect();

    let mut restore: Vec<String> = target
        .iter()
        .map(|(p, _)| p.display().to_string())
        .collect();
    restore.sort();

    // The index is the authoritative tracked-files view. Any path tracked
    // there but absent from the target tree will be removed by the
    // worktree restore — surface it in the dry-run so users see both
    // sides of the diff.
    let mut delete: Vec<String> = match Index::load(crate::utils::path::index()) {
        Ok(index) => index
            .tracked_entries(0)
            .into_iter()
            .filter_map(|entry| {
                let path = PathBuf::from(&entry.name);
                if target_set.contains(&path) {
                    None
                } else {
                    Some(path.display().to_string())
                }
            })
            .collect(),
        // Index unreadable (e.g. fresh repo with no staged files) — leave
        // the deletion set empty and let the user proceed with --apply.
        Err(_) => Vec::new(),
    };
    delete.sort();

    Ok(RewindPlan { restore, delete })
}

// ---------------------------------------------------------------------------
// AG-20 metadata-first `show` layout summary (E4-libra + legacy-v1 fallback)
// ---------------------------------------------------------------------------

/// AG-20 E4-libra layout: manifest-first summary.
const LAYOUT_E4_LIBRA: &str = "e4-libra";
/// Pre-AG-20 writer layout (`metadata.json` + `transcript/<provider>`, no
/// manifest). A first-class readable layout — NOT an inconsistency.
const LAYOUT_LEGACY_V1: &str = "legacy-v1";
/// Layout could not be resolved from the local object store.
const LAYOUT_UNKNOWN: &str = "unknown";

const TRANSCRIPT_PRESENT: &str = "present";
const TRANSCRIPT_MISSING: &str = "missing";
const TRANSCRIPT_UNKNOWN: &str = "unknown";

/// Metadata-first layout summary for one checkpoint. Serialized additively
/// into the `checkpoint show --json` payload under `layout`; the
/// pre-existing `checkpoint` / `metadata` keys are unchanged.
#[derive(Debug, Serialize)]
struct CheckpointLayoutSummary {
    /// `e4-libra`, `legacy-v1`, or `unknown`.
    kind: &'static str,
    /// Logical roles: manifest entries for E4-libra, tree entries for v1.
    roles: Vec<CheckpointRoleSummary>,
    transcript: TranscriptSummary,
    /// `content_hash.txt` summary (E4-libra only; `null` for legacy-v1).
    content_hash: Option<ContentHashSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl CheckpointLayoutSummary {
    fn unknown(reason: String) -> Self {
        Self {
            kind: LAYOUT_UNKNOWN,
            roles: Vec::new(),
            transcript: TranscriptSummary {
                availability: TRANSCRIPT_UNKNOWN,
                chunked: false,
                parts: Vec::new(),
            },
            content_hash: None,
            note: Some(reason),
        }
    }
}

#[derive(Debug, Serialize)]
struct CheckpointRoleSummary {
    role: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_len: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    redaction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_version: Option<u64>,
}

/// Transcript summary derived without ever opening a transcript blob:
/// part identities come from the manifest (E4-libra) or the tree (v1) and
/// presence is a stat on the loose-object path.
#[derive(Debug, Serialize)]
struct TranscriptSummary {
    /// `present` (every declared part's object file exists locally),
    /// `missing` (at least one is absent), or `unknown` (parts could not
    /// be enumerated).
    availability: &'static str,
    chunked: bool,
    /// Physical transcript files in manifest/tree order (one entry for an
    /// unchunked transcript). `byte_len` is manifest-declared and thus
    /// absent for legacy-v1 parts (reading the blob to size it would
    /// violate the metadata-first contract).
    parts: Vec<TranscriptPartSummary>,
}

#[derive(Debug, Serialize)]
struct TranscriptPartSummary {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_len: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ContentHashSummary {
    /// Raw `content_hash.txt` text (trimmed, bounded) — writer format is
    /// `sha256:<64-lowercase-hex>`.
    value: String,
    /// Whether [`parse_content_hash`] accepted the value (it also
    /// tolerates legacy bare hex).
    format_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
}

/// Classify the checkpoint layout, degrading every failure to
/// `unknown` + note (the catalog row and metadata blob are the
/// load-bearing outputs of `show`; the layout walk is best-effort).
fn summarize_checkpoint_layout(row: &CheckpointRow) -> CheckpointLayoutSummary {
    match try_summarize_checkpoint_layout(row) {
        Ok(summary) => summary,
        Err(reason) => CheckpointLayoutSummary::unknown(reason),
    }
}

fn try_summarize_checkpoint_layout(row: &CheckpointRow) -> Result<CheckpointLayoutSummary, String> {
    let storage = util::try_get_storage_path(None)
        .map_err(|e| format!("not in a libra repository ({e}); layout not classified"))?;
    let root = read_tree_object(&storage, &row.tree_oid)?;
    let checkpoint_tree = subtree(&storage, &root, "checkpoint")?;
    let prefix = row
        .checkpoint_id
        .get(..2)
        .ok_or_else(|| format!("checkpoint id '{}' is too short", row.checkpoint_id))?;
    let prefix_tree = subtree(&storage, &checkpoint_tree, prefix)?;
    let inner = subtree(&storage, &prefix_tree, &row.checkpoint_id[2..])?;

    if let Some(manifest_item) = tree_entry(&inner, "manifest.json") {
        summarize_e4_libra(&storage, &inner, &manifest_item.id.to_string())
    } else if tree_entry(&inner, "metadata.json").is_some() {
        summarize_legacy_v1(&storage, &inner)
    } else {
        Err(format!(
            "checkpoint tree {} carries neither manifest.json (E4-libra) nor \
             metadata.json (legacy-v1); layout not classified",
            row.tree_oid
        ))
    }
}

/// E4-libra: everything comes from `manifest.json` — roles, transcript
/// parts (in manifest order, per the E5 "resolve chunks only through the
/// manifest" rule), and the `content_hash.txt` format check. Transcript
/// blob bodies are never read.
fn summarize_e4_libra(
    storage: &Path,
    inner: &Tree,
    manifest_oid: &str,
) -> Result<CheckpointLayoutSummary, String> {
    let manifest_hash = ObjectHash::from_str(manifest_oid)
        .map_err(|e| format!("invalid manifest.json oid '{manifest_oid}': {e}"))?;
    let (manifest_bytes, manifest_truncated) =
        read_git_object_bounded(storage, &manifest_hash, CHECKPOINT_METADATA_READ_MAX_BYTES)
            .map_err(|e| {
                format!("manifest.json blob {manifest_oid} is not readable locally: {e}")
            })?;
    if manifest_truncated {
        return Err(format!(
            "manifest.json blob {manifest_oid} exceeds the metadata size cap; \
             refusing (corrupt or hostile checkpoint)"
        ));
    }
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("manifest.json blob {manifest_oid} is not valid JSON: {e}"))?;

    let entries = manifest.get("entries").and_then(|v| v.as_object());
    let mut roles = Vec::new();
    if let Some(entries) = entries {
        for (role, declared) in entries {
            roles.push(CheckpointRoleSummary {
                role: role.clone(),
                path: declared
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                oid: declared
                    .get("oid")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                byte_len: declared.get("byte_len").and_then(|v| v.as_u64()),
                media_type: declared
                    .get("media_type")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                redaction: declared
                    .get("redaction")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                schema_version: declared.get("schema_version").and_then(|v| v.as_u64()),
            });
        }
    }

    let transcript_decl = entries.and_then(|entries| entries.get("transcript"));
    let mut chunked = false;
    let mut parts = Vec::new();
    if let Some(declared) = transcript_decl {
        chunked = declared
            .get("chunked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if chunked {
            for part in declared
                .get("parts")
                .and_then(|v| v.as_array())
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                parts.push(TranscriptPartSummary {
                    path: part
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    oid: part.get("oid").and_then(|v| v.as_str()).map(str::to_string),
                    byte_len: part.get("byte_len").and_then(|v| v.as_u64()),
                });
            }
        } else {
            parts.push(TranscriptPartSummary {
                path: declared
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                oid: declared
                    .get("oid")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                byte_len: declared.get("byte_len").and_then(|v| v.as_u64()),
            });
        }
    }
    let availability = transcript_availability(storage, &parts);

    // content_hash.txt is a derived, fixed-size artifact — reading it is
    // part of the metadata surface, not transcript IO.
    let content_hash = tree_entry(inner, "content_hash.txt").map(|item| {
        match read_git_object_bounded(storage, &item.id, CHECKPOINT_METADATA_READ_MAX_BYTES) {
            // A truncated (oversized) content_hash.txt is treated as
            // unreadable — never report a `format_valid` digest parsed from
            // a partial object (a valid prefix + huge padding would
            // otherwise pass `parse_content_hash`).
            Ok((_, true)) => ContentHashSummary {
                value: "(unreadable: exceeds metadata size cap)".to_string(),
                format_valid: false,
                digest: None,
            },
            Ok((bytes, false)) => {
                let text = String::from_utf8_lossy(&bytes);
                let digest = parse_content_hash(&text);
                ContentHashSummary {
                    value: text.trim().chars().take(96).collect(),
                    format_valid: digest.is_some(),
                    digest,
                }
            }
            Err(e) => ContentHashSummary {
                value: format!("(unreadable: {e})"),
                format_valid: false,
                digest: None,
            },
        }
    });

    Ok(CheckpointLayoutSummary {
        kind: LAYOUT_E4_LIBRA,
        roles,
        transcript: TranscriptSummary {
            availability,
            chunked,
            parts,
        },
        content_hash,
        note: None,
    })
}

/// Legacy-v1 (pre-AG-20 writer): no manifest — roles are derived from the
/// tree itself (`metadata.json`, `transcript/<provider>` without
/// extension, optionally `events/<provider>.jsonl`). Byte lengths are
/// unknown by design: sizing them would require reading the blobs.
fn summarize_legacy_v1(storage: &Path, inner: &Tree) -> Result<CheckpointLayoutSummary, String> {
    let mut roles = Vec::new();
    let mut parts = Vec::new();
    for item in &inner.tree_items {
        match (item.name.as_str(), item.mode) {
            ("transcript", TreeItemMode::Tree) => {
                let transcript_tree = read_tree_object(storage, &item.id.to_string())?;
                for file in &transcript_tree.tree_items {
                    let path = format!("transcript/{}", file.name);
                    roles.push(plain_role("transcript", &path, &file.id.to_string()));
                    parts.push(TranscriptPartSummary {
                        path,
                        oid: Some(file.id.to_string()),
                        byte_len: None,
                    });
                }
            }
            ("events", TreeItemMode::Tree) => {
                let events_tree = read_tree_object(storage, &item.id.to_string())?;
                for file in &events_tree.tree_items {
                    let path = format!("events/{}", file.name);
                    roles.push(plain_role("events", &path, &file.id.to_string()));
                }
            }
            (name, _) => {
                let role = if name == "metadata.json" {
                    "metadata"
                } else {
                    name
                };
                roles.push(plain_role(role, name, &item.id.to_string()));
            }
        }
    }
    let availability = transcript_availability(storage, &parts);
    Ok(CheckpointLayoutSummary {
        kind: LAYOUT_LEGACY_V1,
        roles,
        transcript: TranscriptSummary {
            availability,
            chunked: false,
            parts,
        },
        content_hash: None,
        note: Some(
            "pre-AG-20 legacy-v1 layout (no manifest.json); \
             metadata-first fallback parse"
                .to_string(),
        ),
    })
}

fn plain_role(role: &str, path: &str, oid: &str) -> CheckpointRoleSummary {
    CheckpointRoleSummary {
        role: role.to_string(),
        path: path.to_string(),
        oid: Some(oid.to_string()),
        byte_len: None,
        media_type: None,
        redaction: None,
        schema_version: None,
    }
}

/// Stat-only presence probe over the loose-object store: blob bodies are
/// never opened (metadata-first discipline). `unknown` when a part lacks
/// a parseable OID; `missing` when any declared part's object file is
/// absent locally.
fn transcript_availability(storage: &Path, parts: &[TranscriptPartSummary]) -> &'static str {
    if parts.is_empty() {
        return TRANSCRIPT_UNKNOWN;
    }
    let mut all_present = true;
    for part in parts {
        let Some(oid) = part.oid.as_deref() else {
            return TRANSCRIPT_UNKNOWN;
        };
        if ObjectHash::from_str(oid).is_err() {
            return TRANSCRIPT_UNKNOWN;
        }
        let object_path = storage.join("objects").join(&oid[..2]).join(&oid[2..]);
        if !object_path.exists() {
            all_present = false;
        }
    }
    if all_present {
        TRANSCRIPT_PRESENT
    } else {
        TRANSCRIPT_MISSING
    }
}

/// Upper bound on the inflated size of checkpoint metadata objects (trees
/// and `manifest.json`). Real checkpoint trees/manifests are KB-scale; this
/// generous cap never trips on legitimate data but stops a corrupt/hostile
/// object from forcing an unbounded decompression + allocation on the
/// show/export paths (AG-24a; codex review R2).
const CHECKPOINT_METADATA_READ_MAX_BYTES: u64 = 16 * 1024 * 1024;

fn read_tree_object(storage: &Path, oid_str: &str) -> Result<Tree, String> {
    let oid = ObjectHash::from_str(oid_str)
        .map_err(|e| format!("invalid tree oid '{oid_str}' in the checkpoint catalog: {e}"))?;
    let (body, truncated) =
        read_git_object_bounded(storage, &oid, CHECKPOINT_METADATA_READ_MAX_BYTES).map_err(
            |e| {
                format!(
                    "checkpoint tree {oid_str} is not readable from the local object \
                     store ({e}); layout unknown — metadata-first summary only"
                )
            },
        )?;
    if truncated {
        return Err(format!(
            "checkpoint tree {oid_str} exceeds the {CHECKPOINT_METADATA_READ_MAX_BYTES}-byte \
             metadata cap; refusing to load (corrupt or hostile object)"
        ));
    }
    Tree::from_bytes(&body, oid)
        .map_err(|e| format!("object {oid_str} did not parse as a tree: {e:?}"))
}

fn tree_entry<'t>(tree: &'t Tree, name: &str) -> Option<&'t TreeItem> {
    tree.tree_items.iter().find(|item| item.name == name)
}

fn subtree(storage: &Path, tree: &Tree, name: &str) -> Result<Tree, String> {
    let item = tree_entry(tree, name).ok_or_else(|| {
        format!("tree entry '{name}' missing while resolving the checkpoint tree")
    })?;
    read_tree_object(storage, &item.id.to_string())
}

/// `libra agent checkpoint export <id>` (AG-24a). Redacted export is the
/// default and requires no authorization. A RAW (un-redacted) export
/// requires `--allow-raw --raw`; a raw request without `--allow-raw` is
/// refused fail-closed (`LBR-AGENT-013`) and the refusal is audited. Every
/// raw access (grant or deny) appends one row to the append-only
/// `agent_audit_log`.
async fn export(args: super::CheckpointExportArgs, output: &OutputConfig) -> CliResult<()> {
    use crate::internal::ai::observed_agents::compliance::max_transcript_read_bytes;

    let conn = get_db_conn_instance().await;
    let backend = conn.get_database_backend();

    // Fail-closed gate FIRST — before any checkpoint lookup. A raw request
    // without --allow-raw is refused, audited (granted=0), and returns
    // LBR-AGENT-013 regardless of whether the checkpoint exists. Gating
    // before the row load keeps the refusal fail-closed and avoids a
    // checkpoint-existence oracle (the error must not depend on whether
    // the id resolves).
    if args.raw && !args.allow_raw {
        write_export_audit(
            &conn,
            &args.checkpoint_id,
            args.output_path.as_deref(),
            args.justification.as_deref(),
            false,
        )
        .await?;
        return Err(CliError::fatal(
            "raw (un-redacted) checkpoint export requires --allow-raw".to_string(),
        )
        .with_stable_code(StableErrorCode::AgentRawAccessDenied)
        .with_hint("re-run with --allow-raw --raw to authorize (the access is audited)")
        .with_hint("or omit --raw to export the redacted transcript (no authorization needed)"));
    }

    let row = load_checkpoint_row(&conn, &args.checkpoint_id).await?;

    // Raw export only when BOTH the request (--raw) and authorization
    // (--allow-raw) are present. `--allow-raw` alone does NOT force a raw
    // export — it falls through to the redacted path — matching the
    // documented `--allow-raw --raw` contract.
    let wants_raw = args.raw && args.allow_raw;

    let cap = max_transcript_read_bytes()
        .await
        .map_err(|e| CliError::fatal(format!("read max_transcript_read_bytes config: {e:#}")))?;
    let (bytes, truncated) = load_checkpoint_transcript_bytes(&row, cap)?;

    let emitted = if wants_raw {
        // Grant: audit the raw access, then emit the un-redacted bytes.
        write_export_audit(
            &conn,
            &args.checkpoint_id,
            args.output_path.as_deref(),
            args.justification.as_deref(),
            true,
        )
        .await?;
        bytes
    } else {
        // Default redacted path — no --allow-raw, no audit; just scrub.
        let (redacted, _report) =
            crate::internal::ai::observed_agents::Redactor::new_default().redact(&bytes);
        redacted.as_ref().to_vec()
    };
    let _ = backend; // backend used inside helpers

    if let Some(path) = &args.output_path {
        std::fs::write(path, &emitted)
            .map_err(|e| CliError::fatal(format!("failed to write export to {path}: {e}")))?;
        if output.is_json() {
            emit_json_data(
                "agent_checkpoint_export",
                &serde_json::json!({
                    "checkpoint_id": args.checkpoint_id,
                    "raw": wants_raw,
                    "bytes": emitted.len(),
                    "truncated": truncated,
                    "output_path": path,
                }),
                output,
            )?;
        } else if !output.quiet {
            let kind = if wants_raw { "raw" } else { "redacted" };
            println!(
                "Exported {} {kind} transcript byte(s) to {path}{}",
                emitted.len(),
                if truncated { " (truncated at cap)" } else { "" }
            );
        }
    } else {
        use std::io::Write;
        std::io::stdout()
            .write_all(&emitted)
            .map_err(|e| CliError::fatal(format!("failed to write export to stdout: {e}")))?;
    }
    Ok(())
}

/// Load one checkpoint catalog row or fail with an actionable message.
async fn load_checkpoint_row(
    conn: &(impl ConnectionTrait + ?Sized),
    checkpoint_id: &str,
) -> Result<CheckpointRow, CliError> {
    if !table_exists(conn, "agent_checkpoint").await? {
        return Err(CliError::fatal(format!(
            "no checkpoint matches '{checkpoint_id}': agent_checkpoint table not present (run `libra init`?)"
        )));
    }
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id, session_id, scope, parent_commit, tree_oid, \
                    metadata_blob_oid, traces_commit, created_at \
             FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
            [checkpoint_id.into()],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_checkpoint: {e}")))?
        .ok_or_else(|| CliError::fatal(format!("no checkpoint matches '{checkpoint_id}'")))?;
    Ok(CheckpointRow {
        checkpoint_id: row.try_get_by("checkpoint_id").unwrap_or_default(),
        session_id: row.try_get_by("session_id").unwrap_or_default(),
        scope: row.try_get_by("scope").unwrap_or_default(),
        parent_commit: row.try_get_by("parent_commit").ok().flatten(),
        tree_oid: row.try_get_by("tree_oid").unwrap_or_default(),
        metadata_blob_oid: row.try_get_by("metadata_blob_oid").unwrap_or_default(),
        traces_commit: row.try_get_by("traces_commit").unwrap_or_default(),
        created_at: row.try_get_by("created_at").unwrap_or_default(),
    })
}

/// Read the checkpoint's stored transcript blob(s) from the E4-libra tree,
/// enforcing the `max_transcript_read_bytes` cap. Returns `(bytes,
/// truncated)`. Chunked transcripts are concatenated in manifest `parts`
/// order (never by globbing tree names).
fn load_checkpoint_transcript_bytes(
    row: &CheckpointRow,
    cap: u64,
) -> Result<(Vec<u8>, bool), CliError> {
    let storage = util::try_get_storage_path(None)
        .map_err(|e| CliError::fatal(format!("not in a libra repository: {e}")))?;
    let root = read_tree_object(&storage, &row.tree_oid).map_err(CliError::fatal)?;
    let checkpoint_tree = subtree(&storage, &root, "checkpoint").map_err(CliError::fatal)?;
    let prefix = row.checkpoint_id.get(..2).ok_or_else(|| {
        CliError::fatal(format!("checkpoint id '{}' too short", row.checkpoint_id))
    })?;
    let prefix_tree = subtree(&storage, &checkpoint_tree, prefix).map_err(CliError::fatal)?;
    let inner =
        subtree(&storage, &prefix_tree, &row.checkpoint_id[2..]).map_err(CliError::fatal)?;

    let manifest_item = tree_entry(&inner, "manifest.json").ok_or_else(|| {
        CliError::fatal(
            "checkpoint has no manifest.json (legacy layout not exportable)".to_string(),
        )
    })?;
    // Bounded read: manifest.json is small JSON; refuse an oversized
    // (corrupt/hostile) one rather than inflate it unbounded.
    let (manifest_bytes, manifest_truncated) = read_git_object_bounded(
        &storage,
        &manifest_item.id,
        CHECKPOINT_METADATA_READ_MAX_BYTES,
    )
    .map_err(|e| CliError::fatal(format!("read manifest.json: {e}")))?;
    if manifest_truncated {
        return Err(CliError::fatal(
            "manifest.json exceeds the metadata size cap; refusing (corrupt or hostile checkpoint)"
                .to_string(),
        ));
    }
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| CliError::fatal(format!("manifest.json invalid JSON: {e}")))?;
    let transcript = manifest
        .get("entries")
        .and_then(|e| e.get("transcript"))
        .ok_or_else(|| CliError::fatal("manifest has no transcript entry".to_string()))?;

    // Collect the ordered list of blob OIDs (single or chunked).
    let mut oids: Vec<String> = Vec::new();
    if transcript
        .get("chunked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        for part in transcript
            .get("parts")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if let Some(oid) = part.get("oid").and_then(|v| v.as_str()) {
                oids.push(oid.to_string());
            }
        }
    } else if let Some(oid) = transcript.get("oid").and_then(|v| v.as_str()) {
        oids.push(oid.to_string());
    }
    if oids.is_empty() {
        return Err(CliError::fatal(
            "manifest transcript entry declares no blob oid".to_string(),
        ));
    }

    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    for oid in oids {
        let remaining = cap.saturating_sub(bytes.len() as u64);
        if remaining == 0 {
            truncated = true;
            break;
        }
        let hash = ObjectHash::from_str(&oid)
            .map_err(|e| CliError::fatal(format!("invalid transcript oid '{oid}': {e}")))?;
        // Bounded read: never decompress more than `remaining` content
        // bytes into memory, so a hostile/corrupt blob whose inflated size
        // dwarfs the cap cannot force an unbounded allocation.
        let (part, part_truncated) = read_git_object_bounded(&storage, &hash, remaining)
            .map_err(|e| CliError::fatal(format!("read transcript blob {oid}: {e}")))?;
        bytes.extend_from_slice(&part);
        if part_truncated {
            truncated = true;
            break;
        }
    }
    Ok((bytes, truncated))
}

/// Append one `agent_audit_log` row for a raw checkpoint export (or its
/// fail-closed refusal). Actor identity is resolved from the committer
/// env vars (never the checkpoint's hardcoded `Libra <ai@libra>`).
async fn write_export_audit(
    conn: &DatabaseConnection,
    checkpoint_id: &str,
    export_path: Option<&str>,
    justification: Option<&str>,
    granted: bool,
) -> Result<(), CliError> {
    use crate::internal::ai::observed_agents::compliance::{
        AuditRecord, AuditScope, write_audit_record,
    };
    let user_name = std::env::var("GIT_COMMITTER_NAME")
        .ok()
        .or_else(|| std::env::var("GIT_AUTHOR_NAME").ok())
        .or_else(|| std::env::var("LIBRA_COMMITTER_NAME").ok())
        .filter(|s| !s.is_empty());
    let user_email = std::env::var("GIT_COMMITTER_EMAIL")
        .ok()
        .or_else(|| std::env::var("EMAIL").ok())
        .or_else(|| std::env::var("LIBRA_COMMITTER_EMAIL").ok())
        .filter(|s| !s.is_empty());
    let record = AuditRecord::new(
        uuid::Uuid::new_v4().to_string(),
        chrono::Utc::now().to_rfc3339(),
        (user_email, user_name),
        "raw_export",
        checkpoint_id,
        AuditScope::Transcript,
        export_path.map(str::to_string),
        justification.map(str::to_string),
        granted,
    );
    write_audit_record(conn, &record)
        .await
        .map_err(|e| CliError::fatal(format!("append audit record: {e:#}")))
}

pub(super) fn load_metadata_blob(oid: &str) -> Result<String, CliError> {
    let hash = ObjectHash::from_str(oid)
        .map_err(|e| CliError::fatal(format!("invalid metadata_blob_oid '{oid}': {e}")))?;
    let storage = util::try_get_storage_path(None)
        .map_err(|e| CliError::fatal(format!("not in a libra repository: {e}")))?;
    let (raw, truncated) =
        read_git_object_bounded(&storage, &hash, CHECKPOINT_METADATA_READ_MAX_BYTES).map_err(
            |e| {
                CliError::fatal(format!(
                    "failed to read metadata blob {oid} from object store: {e}"
                ))
            },
        )?;
    if truncated {
        return Err(CliError::fatal(format!(
            "metadata blob {oid} exceeds the metadata size cap; refusing (corrupt or hostile checkpoint)"
        )));
    }
    String::from_utf8(raw)
        .map_err(|e| CliError::fatal(format!("metadata blob {oid} is not UTF-8: {e}")))
}

fn emit_list(page: &CheckpointListPage, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("agent_checkpoints", page, output);
    }
    if output.quiet {
        return Ok(());
    }
    if page.checkpoints.is_empty() {
        println!("(no captured checkpoints)");
        return Ok(());
    }
    println!(
        "{:<37}  {:<37}  {:<10}  {:<20}",
        "checkpoint_id", "session_id", "scope", "created_at"
    );
    for r in &page.checkpoints {
        println!(
            "{:<37}  {:<37}  {:<10}  {:<20}",
            r.checkpoint_id, r.session_id, r.scope, r.created_at
        );
    }
    if let Some(cursor) = &page.next_cursor {
        println!("(more rows available — next page: --cursor {cursor})");
    }
    Ok(())
}

fn emit_one(
    row: &CheckpointRow,
    metadata_blob: Option<&str>,
    layout: &CheckpointLayoutSummary,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        // Inline the metadata content as parsed JSON so JSON consumers can
        // join on it without doing a second blob fetch.
        let metadata_json = metadata_blob
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let payload = serde_json::json!({
            "checkpoint": row,
            "metadata": metadata_json,
            "layout": layout,
        });
        return emit_json_data("agent_checkpoint", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("checkpoint_id     : {}", row.checkpoint_id);
    println!("session_id        : {}", row.session_id);
    println!("scope             : {}", row.scope);
    let parent_display = match row.parent_commit.as_deref() {
        Some(commit) if !commit.is_empty() => commit,
        _ => "(none — unborn HEAD or pre-commit ingest)",
    };
    println!("parent_commit     : {parent_display}");
    println!("tree_oid          : {}", row.tree_oid);
    println!("metadata_blob_oid : {}", row.metadata_blob_oid);
    println!("traces_commit     : {}", row.traces_commit);
    println!("created_at        : {}", row.created_at);
    println!("layout            : {}", layout.kind);
    if let Some(note) = &layout.note {
        println!("layout_note       : {note}");
    }
    let transcript = &layout.transcript;
    match (transcript.chunked, transcript.parts.as_slice()) {
        (false, [only]) => {
            println!(
                "transcript        : {} ({}{})",
                transcript.availability,
                only.path,
                only.byte_len
                    .map(|n| format!(", {n} bytes"))
                    .unwrap_or_default()
            );
        }
        (_, []) => println!("transcript        : {}", transcript.availability),
        (_, parts) => {
            println!(
                "transcript        : {} (chunked, {} parts in manifest order)",
                transcript.availability,
                parts.len()
            );
            for part in parts {
                println!(
                    "  - {}{}",
                    part.path,
                    part.byte_len
                        .map(|n| format!(" ({n} bytes)"))
                        .unwrap_or_default()
                );
            }
        }
    }
    if let Some(hash) = &layout.content_hash {
        println!(
            "content_hash      : {} ({})",
            hash.value,
            if hash.format_valid {
                "well-formed"
            } else {
                "MALFORMED"
            }
        );
    }
    if !layout.roles.is_empty() {
        let mut role_names: Vec<&str> = layout.roles.iter().map(|r| r.role.as_str()).collect();
        role_names.dedup();
        println!("roles             : {}", role_names.join(", "));
    }
    if let Some(metadata) = metadata_blob {
        println!("---");
        println!("metadata.json:");
        println!("{metadata}");
    }
    Ok(())
}

pub(super) async fn table_exists(
    conn: &(impl ConnectionTrait + ?Sized),
    name: &str,
) -> CliResult<bool> {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use sea_orm::{ConnectOptions, Database, ExecResult};
    use tempfile::TempDir;

    use super::*;
    use crate::internal::db::{
        ensure_ai_runtime_contract_schema, migration::run_builtin_migrations,
    };

    const LEGACY_BOOTSTRAP_SQL: &str = include_str!("../../../sql/sqlite_20260309_init.sql");

    /// Spin up a freshly-migrated SQLite at `<dir>/libra.db`. Mirrors the
    /// fixture used by the hook runtime tests so the schema is identical
    /// to production (legacy bootstrap → AI runtime contract → registered
    /// migrations).
    async fn fresh_db() -> (TempDir, sea_orm::DatabaseConnection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libra.db");
        std::fs::File::create(&path).unwrap();
        let url = format!("sqlite://{}", path.display());
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
        (dir, conn)
    }

    /// Phase 4.1 acceptance: when the fixture has a Claude Code session
    /// with a `transcript_path` in `metadata_json` and a checkpoint
    /// timestamped between two transcript lines, the truncator must
    /// drop the post-boundary lines.
    #[tokio::test]
    async fn rewind_truncate_drops_post_boundary_lines_for_claude_code() {
        let (dir, conn) = fresh_db().await;
        // Create the on-disk transcript with two lines straddling the
        // boundary. The checkpoint lives at 10:30; the second line at
        // 11:00 must be dropped.
        let transcript_path = dir.path().join("session.jsonl");
        fs::write(
            &transcript_path,
            b"{\"timestamp\":\"2026-05-05T10:00:00Z\",\"text\":\"keep\"}\n\
              {\"timestamp\":\"2026-05-05T11:00:00Z\",\"text\":\"drop\"}\n",
        )
        .unwrap();
        let metadata_json = serde_json::json!({
            "transcript_path": transcript_path.to_str().unwrap(),
        })
        .to_string();
        // Boundary at 2026-05-05T10:30:00Z so the 10:00 line is kept and
        // the 11:00 line is dropped.
        let created_at: i64 = chrono::DateTime::parse_from_rfc3339("2026-05-05T10:30:00Z")
            .unwrap()
            .timestamp();

        let backend = conn.get_database_backend();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES ('s-1', 'claude_code', 'p-1', 'stopped', '/tmp', ?, '{}', 0, 0)",
            [metadata_json.into()],
        ))
        .await
        .unwrap();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES ('cp-1', 's-1', 'committed', NULL, 'tree', 'meta', 'commit', ?)",
            [created_at.into()],
        ))
        .await
        .unwrap();

        let outcome =
            super::truncate_agent_transcript_for_checkpoint_with_conn(&conn, "cp-1").await;
        match outcome {
            super::TranscriptTruncationOutcome::Truncated { lines_dropped, .. } => {
                assert_eq!(lines_dropped, 1, "exactly one line removed");
            }
            other => panic!("expected Truncated, got {:?}", other.as_json()),
        }

        let after = fs::read_to_string(&transcript_path).unwrap();
        assert!(after.contains("\"keep\""));
        assert!(!after.contains("\"drop\""));
    }

    /// When `metadata_json` lacks a transcript_path, the helper must
    /// surface `SkippedNoPath` rather than failing.
    #[tokio::test]
    async fn rewind_truncate_skips_when_no_transcript_path_in_metadata() {
        let (_dir, conn) = fresh_db().await;
        let backend = conn.get_database_backend();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES ('s-2', 'claude_code', 'p-2', 'stopped', '/tmp', '{}', '{}', 0, 0)",
            [],
        ))
        .await
        .unwrap();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES ('cp-2', 's-2', 'committed', NULL, 't', 'm', 'c', 0)",
            [],
        ))
        .await
        .unwrap();

        let outcome =
            super::truncate_agent_transcript_for_checkpoint_with_conn(&conn, "cp-2").await;
        assert!(matches!(
            outcome,
            super::TranscriptTruncationOutcome::SkippedNoPath
        ));
    }

    /// Codex round-3 follow-up: the dry-run `transcript_truncation_supported`
    /// flag must match the apply path's actual decision. We test all four
    /// quadrants (kind × has_transcript_path) against
    /// `lookup_truncation_support`.
    #[tokio::test]
    async fn lookup_truncation_support_matches_apply_decision() {
        let (dir, conn) = fresh_db().await;
        let backend = conn.get_database_backend();

        let transcript_path = dir.path().join("session.jsonl");
        fs::write(&transcript_path, b"").unwrap();
        let path_meta = serde_json::json!({
            "transcript_path": transcript_path.to_str().unwrap(),
        })
        .to_string();

        // Claude Code + transcript_path → supported.
        for (idx, (kind, meta)) in [
            ("claude_code", path_meta.as_str()), // supported
            ("claude_code", "{}"),               // skipped (no path)
            ("cursor", path_meta.as_str()),      // skipped (kind)
            ("cursor", "{}"),                    // skipped (both)
        ]
        .iter()
        .enumerate()
        {
            let session_id = format!("s-{idx}");
            let provider_session_id = format!("p-{idx}");
            let checkpoint_id = format!("cp-{idx}");
            conn.execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO agent_session (
                    session_id, agent_kind, provider_session_id, state, working_dir,
                    metadata_json, redaction_report, started_at, last_event_at
                 ) VALUES (?, ?, ?, 'stopped', '/tmp', ?, '{}', 0, 0)",
                [
                    session_id.clone().into(),
                    (*kind).into(),
                    provider_session_id.into(),
                    (*meta).into(),
                ],
            ))
            .await
            .unwrap();
            conn.execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO agent_checkpoint (
                    checkpoint_id, session_id, scope, parent_commit, tree_oid,
                    metadata_blob_oid, traces_commit, created_at
                 ) VALUES (?, ?, 'committed', NULL, 't', 'm', 'c', 0)",
                [checkpoint_id.clone().into(), session_id.into()],
            ))
            .await
            .unwrap();

            let supported = super::lookup_truncation_support(&conn, &checkpoint_id)
                .await
                .unwrap();
            let expected = idx == 0;
            assert_eq!(
                supported, expected,
                "case {idx} (kind={kind}, meta={meta}) supported={supported}, expected={expected}"
            );
        }
    }

    /// When `agent_kind` isn't `claude_code` (e.g. preview adapters that
    /// have no truncator yet), the helper must report
    /// `SkippedUnsupportedKind` so the operator knows the transcript
    /// was deliberately not touched.
    #[tokio::test]
    async fn rewind_truncate_skips_unsupported_agent_kind() {
        let (dir, conn) = fresh_db().await;
        let transcript_path = dir.path().join("session.jsonl");
        fs::write(&transcript_path, b"{}\n").unwrap();
        let metadata_json = serde_json::json!({
            "transcript_path": transcript_path.to_str().unwrap(),
        })
        .to_string();

        let backend = conn.get_database_backend();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES ('s-3', 'cursor', 'p-3', 'stopped', '/tmp', ?, '{}', 0, 0)",
            [metadata_json.into()],
        ))
        .await
        .unwrap();
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES ('cp-3', 's-3', 'committed', NULL, 't', 'm', 'c', 0)",
            [],
        ))
        .await
        .unwrap();

        let outcome =
            super::truncate_agent_transcript_for_checkpoint_with_conn(&conn, "cp-3").await;
        match outcome {
            super::TranscriptTruncationOutcome::SkippedUnsupportedKind { agent_kind } => {
                assert_eq!(agent_kind, "cursor");
            }
            other => panic!("expected SkippedUnsupportedKind, got {:?}", other.as_json()),
        }
    }

    // -----------------------------------------------------------------
    // AG-20 keyset pagination helpers
    // -----------------------------------------------------------------

    /// The opaque cursor round-trips `(timestamp, id)` losslessly,
    /// including ids that themselves contain `:` separators.
    #[test]
    fn page_cursor_round_trips() {
        for (timestamp, id) in [
            (0i64, "a"),
            (1_783_206_712, "85ae75d2-4c53-465a-b890-a9f861a50cc7"),
            (-5, "claude__sess:with:colons"),
        ] {
            let cursor = super::encode_page_cursor(timestamp, id);
            let (got_ts, got_id) = super::decode_page_cursor(&cursor).expect("round trip");
            assert_eq!(got_ts, timestamp);
            assert_eq!(got_id, id);
        }
    }

    /// Every malformation class fails closed with one actionable usage
    /// error naming `--cursor` — never a silent restart of the listing.
    #[test]
    fn page_cursor_rejects_malformed_values() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let cases = [
            "not-base64!!".to_string(),               // invalid base64
            STANDARD.encode("v2:1:x"),                // wrong version tag
            STANDARD.encode("v1:notanumber:x"),       // non-numeric timestamp
            STANDARD.encode("v1:12"),                 // missing id separator
            STANDARD.encode("v1:12:"),                // empty id
            STANDARD.encode([0xffu8, 0xfe, 0x00, 1]), // not UTF-8
        ];
        for cursor in cases {
            let err = super::decode_page_cursor(&cursor)
                .expect_err(&format!("cursor '{cursor}' must be rejected"));
            assert!(
                err.to_string().contains("--cursor"),
                "error must name --cursor: {err}"
            );
        }
    }

    /// Limit semantics: default 50, `0` → 1 (no note), `500` accepted
    /// as-is, anything above 500 clamps and produces a stderr note.
    #[test]
    fn page_limit_defaults_clamps_and_floors() {
        assert_eq!(super::resolve_page_limit(None), (50, None));
        assert_eq!(super::resolve_page_limit(Some(0)), (1, None));
        assert_eq!(super::resolve_page_limit(Some(7)), (7, None));
        assert_eq!(super::resolve_page_limit(Some(500)), (500, None));
        let (limit, note) = super::resolve_page_limit(Some(501));
        assert_eq!(limit, 500);
        let note = note.expect("clamp must produce a note");
        assert!(note.contains("501") && note.contains("500"), "{note}");
        let (limit, note) = super::resolve_page_limit(Some(u64::MAX));
        assert_eq!(limit, 500);
        assert!(note.is_some());
    }

    /// AG-20 index-hit guard on the REAL SQL builders (plan.md A5
    /// validation): every cursored page query must be a pure index SEARCH
    /// on the 2026070802 pagination indexes — no `SCAN <table>` without
    /// an index and no temp B-tree sort step.
    #[tokio::test]
    async fn paginated_list_queries_hit_keyset_indexes() {
        let (_dir, conn) = fresh_db().await;
        let backend = conn.get_database_backend();
        let cursor_values = |id: &str| -> Vec<sea_orm::Value> {
            vec![
                100i64.into(),
                100i64.into(),
                id.to_string().into(),
                51i64.into(),
            ]
        };
        let cases: Vec<(String, Vec<sea_orm::Value>, &str, &str)> = vec![
            (
                super::checkpoint_page_sql(false, true),
                cursor_values("cp"),
                "idx_agent_checkpoint_created_paging",
                "agent_checkpoint",
            ),
            (
                super::super::session::session_page_sql(false, false, true),
                cursor_values("sess"),
                "idx_agent_session_started_paging",
                "agent_session",
            ),
        ];
        for (sql, values, index_name, table) in cases {
            let rows = conn
                .query_all(Statement::from_sql_and_values(
                    backend,
                    format!("EXPLAIN QUERY PLAN {sql}"),
                    values,
                ))
                .await
                .expect("explain query plan");
            let plan = rows
                .iter()
                .map(|row| row.try_get_by::<String, _>("detail").unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                plan.contains(index_name),
                "plan for `{sql}` must use {index_name}, got:\n{plan}"
            );
            assert!(
                !plan.contains("TEMP B-TREE"),
                "plan for `{sql}` must not sort via temp B-tree, got:\n{plan}"
            );
            assert!(
                !plan.contains(&format!("SCAN {table}\n"))
                    && !plan.ends_with(&format!("SCAN {table}")),
                "plan for `{sql}` must not full-scan {table}, got:\n{plan}"
            );
        }
    }
}
