//! `libra agent checkpoint …` subcommands. V1 ships read-only `list` /
//! `show`; `rewind --apply` restores the worktree. Provider transcript
//! rewinding is intentionally disabled until it has an atomic,
//! identity-checked replacement primitive.

use std::path::Path;

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

use super::{
    CheckpointListArgs, CheckpointRewindArgs, CheckpointShowArgs, CheckpointSubcommand,
    capture_source::{DerivedTranscriptSource, resolve_derived_transcript_source},
};
use crate::{
    command::load_object,
    internal::db::get_db_conn_instance,
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

/// The deliberately narrow default representation returned by `checkpoint
/// show`.  The catalog row contains object identifiers and the metadata blob
/// can contain provider-derived, redaction, or recovery details; neither is a
/// safe default-display contract.  Keep this as an explicit whitelist rather
/// than serializing a `CheckpointRow` and trying to redact fields afterwards.
#[derive(Debug, Serialize)]
struct CheckpointShowSummary {
    checkpoint_id: String,
    scope: CheckpointShowScope,
    created_at: i64,
    parent_snapshot_recorded: bool,
}

/// `agent_checkpoint.scope` is constrained by the current schema, but old or
/// damaged local databases must not turn an arbitrary stored string into
/// default CLI output.  Preserve only the closed public vocabulary.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckpointShowScope {
    Temporary,
    Committed,
    Subagent,
    Unknown,
}

impl CheckpointShowScope {
    fn from_catalog(value: Option<&str>) -> Self {
        match value {
            Some("temporary") => Self::Temporary,
            Some("committed") => Self::Committed,
            Some("subagent") => Self::Subagent,
            _ => Self::Unknown,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Temporary => "temporary",
            Self::Committed => "committed",
            Self::Subagent => "subagent",
            Self::Unknown => "unknown",
        }
    }
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
        .query_all_raw(Statement::from_sql_and_values(backend, &sql, values))
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
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT scope, parent_commit, created_at \
             FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
            [args.checkpoint_id.clone().into()],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_checkpoint: {e}")))?;
    match row {
        Some(row) => {
            let parent_commit: Option<String> = row
                .try_get_by("parent_commit")
                .map_err(|_| checkpoint_show_store_inconsistent())?;
            let created_at: i64 = row
                .try_get_by("created_at")
                .map_err(|_| checkpoint_show_store_inconsistent())?;
            let scope: String = row
                .try_get_by("scope")
                .map_err(|_| checkpoint_show_store_inconsistent())?;
            let summary = CheckpointShowSummary {
                // The command's positional identifier is the only identity
                // deliberately echoed by this read operation.  Do not take
                // additional arbitrary identity strings from the catalog.
                checkpoint_id: args.checkpoint_id,
                // An unknown *textual* scope is deliberately projected into
                // the closed `unknown` vocabulary; a failed DB decode is a
                // store inconsistency and must not look like that safe case.
                scope: CheckpointShowScope::from_catalog(Some(&scope)),
                created_at,
                parent_snapshot_recorded: parent_commit.is_some_and(|value| !value.is_empty()),
            };
            emit_one(&summary, output)
        }
        None => Err(CliError::fatal(format!(
            "no checkpoint matches id '{}'",
            args.checkpoint_id
        ))),
    }
}

fn checkpoint_show_store_inconsistent() -> CliError {
    CliError::fatal(
        "checkpoint catalog row is inconsistent; run `libra agent doctor` to inspect the store",
    )
    .with_stable_code(StableErrorCode::AgentCheckpointStoreInconsistent)
}

/// `libra agent checkpoint rewind <id> [--dry-run|--apply]`.
///
/// `dry-run` (the default when neither flag is set) lists the files the
/// checkpoint's `parent_commit` snapshot would restore, without touching the
/// worktree. `--apply` actually runs the worktree restore (delegating to the
/// existing `restore --source <parent_commit>` path), leaves provider
/// transcripts untouched, and leaves HEAD plus `refs/heads/*` untouched per
/// `docs/development/commands/_general.md` §7.3.
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
        .query_one_raw(Statement::from_sql_and_values(
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
    let parent_oid =
        crate::internal::object_format::parse_repo_oid(&parent_commit).map_err(|e| {
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

    // Report whether `--apply` can safely rewrite the provider transcript.
    // The answer is deliberately false until an identity-checked atomic
    // replacement primitive exists; dry-run must mirror that apply policy.
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
            "Re-run with --apply to restore the working tree. Provider \
             transcripts will remain untouched because secure, atomic \
             identity-checked rewinding is not available."
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

    // Preserve an explicit transcript outcome alongside the successful
    // worktree restore. The source check is read-only: provider transcript
    // rewinding remains disabled until a secure atomic replacement exists.
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
            TranscriptTruncationOutcome::SkippedNoDerivedSource => {
                println!(
                    "Note: no verified provider transcript source is available for this \
                     captured session; the agent's local transcript was left untouched."
                );
            }
            TranscriptTruncationOutcome::SkippedUnsafeMutation => {
                println!(
                    "Note: verified provider transcript rewinding is disabled because this \
                     platform has no atomic identity-checked replacement primitive; the \
                     agent's local transcript was left untouched."
                );
            }
            TranscriptTruncationOutcome::SkippedUnsupportedKind => {
                println!(
                    "Note: the captured agent kind has no TranscriptTruncator adapter yet; \
                     the agent's local transcript was left untouched.",
                );
            }
            TranscriptTruncationOutcome::Failed => {
                eprintln!(
                    "warning: transcript truncation could not inspect the captured source safely. \
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
    SkippedNoDerivedSource,
    SkippedUnsafeMutation,
    SkippedUnsupportedKind,
    Failed,
}

impl TranscriptTruncationOutcome {
    fn as_json(&self) -> serde_json::Value {
        // Codex round-4 follow-up: align `supported` semantics across
        // dry-run and apply outputs. `supported` here means "did the
        // truncator actually run end-to-end on this checkpoint?" — same
        // contract as `lookup_truncation_support` in the dry-run path.
        // Skipped paths therefore report `supported: false`. This build
        // never writes a provider transcript without a true
        // identity-checked atomic replacement primitive, so all normal
        // outcomes report `supported: false`. `Failed` means catalog/source
        // derivation could not be completed safely.
        match self {
            Self::SkippedNoDerivedSource => serde_json::json!({
                "supported": false,
                "applied": false,
                "reason": "no verified provider transcript source is available for this captured session",
            }),
            Self::SkippedUnsafeMutation => serde_json::json!({
                "supported": false,
                "applied": false,
                "reason": "verified provider transcript rewinding is disabled: no atomic identity-checked replacement primitive is available",
            }),
            Self::SkippedUnsupportedKind => serde_json::json!({
                "supported": false,
                "applied": false,
                "reason": "no TranscriptTruncator adapter for the captured agent kind",
            }),
            Self::Failed => serde_json::json!({
                "supported": false,
                "applied": false,
                "error": "captured session source could not be inspected safely",
            }),
        }
    }
}

/// Look up the `agent_session` row paired with `checkpoint_id` and determine
/// whether its source can be safely considered for a rewind. Returns an
/// outcome rather than a hard error because worktree restoration remains the
/// load-bearing operation.
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
/// Provider transcript rewinding is deliberately disabled until the platform
/// exposes an atomic replacement primitive that can bind the expected target
/// identity.  Returning false here keeps dry-run faithful to `--apply` and
/// avoids ever treating `metadata_json.transcript_path` as authority.
async fn lookup_truncation_support(
    _conn: &sea_orm::DatabaseConnection,
    _checkpoint_id: &str,
) -> Result<bool, sea_orm::DbErr> {
    Ok(false)
}

/// Connection-bound core of [`truncate_agent_transcript_for_checkpoint`].
/// Extracted so fixture tests can run against an in-memory SQLite without
/// the process-wide `get_db_conn_instance` singleton.
async fn truncate_agent_transcript_for_checkpoint_with_conn(
    conn: &sea_orm::DatabaseConnection,
    checkpoint_id: &str,
) -> TranscriptTruncationOutcome {
    let backend = conn.get_database_backend();

    // Pull the session join for this checkpoint.  Source authority comes from
    // the durable identity fields written after ingress scope validation; do
    // not read a raw transcript locator out of metadata_json here.
    let row = match conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT s.session_id AS session_id, s.agent_kind AS agent_kind, \
                    s.provider_session_id AS provider_session_id, \
                    s.working_dir AS working_dir \
             FROM agent_checkpoint cp \
             JOIN agent_session s ON s.session_id = cp.session_id \
             WHERE cp.checkpoint_id = ? LIMIT 1",
            [checkpoint_id.into()],
        ))
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return TranscriptTruncationOutcome::Failed;
        }
        Err(_) => {
            return TranscriptTruncationOutcome::Failed;
        }
    };
    // A failed decode here means the durable identity is corrupt or the
    // selected schema does not match the catalog.  Never silently substitute
    // an empty component: that could make a malformed row look like a safe
    // no-source/unsupported case and hide an operator-visible catalog fault.
    let session_id: String = match row.try_get_by("session_id") {
        Ok(value) => value,
        Err(_) => {
            return TranscriptTruncationOutcome::Failed;
        }
    };
    let agent_kind: String = match row.try_get_by("agent_kind") {
        Ok(value) => value,
        Err(_) => {
            return TranscriptTruncationOutcome::Failed;
        }
    };
    let provider_session_id: String = match row.try_get_by("provider_session_id") {
        Ok(value) => value,
        Err(_) => {
            return TranscriptTruncationOutcome::Failed;
        }
    };
    let working_dir: String = match row.try_get_by("working_dir") {
        Ok(value) => value,
        Err(_) => {
            return TranscriptTruncationOutcome::Failed;
        }
    };

    // Retain the capability registry for accurate reporting rather than a
    // hard-coded `kind == "claude_code"` literal. The registry handles three
    // failure shapes:
    //   * `AgentKind::from_db_str` fails for unknown tags (schema
    //     mismatch — unsupported kind for this row).
    //   * `truncator_for` returns `None` for kinds whose adapter does not
    //     implement `TranscriptTruncator`; those remain explicitly
    //     unsupported even if atomic rewriting becomes available later.
    use crate::internal::ai::observed_agents::{AgentKind, truncator_for};
    let Some(parsed_kind) = AgentKind::from_db_str(&agent_kind) else {
        return TranscriptTruncationOutcome::SkippedUnsupportedKind;
    };
    let Some(_) = truncator_for(parsed_kind) else {
        return TranscriptTruncationOutcome::SkippedUnsupportedKind;
    };
    match resolve_derived_transcript_source(
        &agent_kind,
        &session_id,
        &working_dir,
        &provider_session_id,
    ) {
        // A safe descriptor-pinned read exists, but POSIX does not expose a
        // target-identity compare-and-swap for an atomic replacement. Do not
        // trade transcript integrity for rewind convenience: leave it alone.
        Ok(DerivedTranscriptSource::Available(_)) => {
            TranscriptTruncationOutcome::SkippedUnsafeMutation
        }
        Ok(DerivedTranscriptSource::Unavailable) => {
            TranscriptTruncationOutcome::SkippedNoDerivedSource
        }
        // This should be unreachable because `parsed_kind` has a truncator,
        // but preserve the explicit unsupported result if a future adapter
        // gains one before it gains an independently-verifiable layout.
        Ok(DerivedTranscriptSource::UnsupportedKind) => {
            TranscriptTruncationOutcome::SkippedUnsupportedKind
        }
        Err(_) => TranscriptTruncationOutcome::Failed,
    }
}

/// Files affected by a `rewind --apply`, broken down by side. `restore`
/// = present in the target commit (will be written to the worktree
/// after `--apply`); `delete` = tracked by the index but absent from the
/// target commit's tree (will be removed from the worktree by the
/// underlying restore's deleted-files pass — see
/// `command::restore::restore_worktree_tracked`).
pub(crate) struct RewindPlan {
    pub(crate) restore: Vec<String>,
    pub(crate) delete: Vec<String>,
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

pub(crate) fn build_rewind_plan(commit_oid: &ObjectHash) -> Result<RewindPlan, anyhow::Error> {
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

/// Upper bound on the inflated size of checkpoint metadata objects (trees
/// and `manifest.json`). Real checkpoint trees/manifests are KB-scale; this
/// generous cap never trips on legitimate data but stops a corrupt/hostile
/// object from forcing an unbounded decompression + allocation on the
/// show/export paths (AG-24a; codex review R2).
const CHECKPOINT_METADATA_READ_MAX_BYTES: u64 = 16 * 1024 * 1024;

fn read_tree_object(storage: &Path, oid_str: &str) -> Result<Tree, String> {
    let oid = crate::internal::object_format::parse_repo_oid(oid_str)
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

/// PD-02: resolve `--checkpoint <id>` (review/investigate scoped input)
/// into a validated materialization spec. Every failure — unknown id,
/// malformed tree, non-local blob — fails closed HERE, before the caller
/// creates any run state, so an invalid checkpoint never leaves run
/// residue. The returned spec lists the checkpoint's ENTIRE inner tree
/// (metadata, manifest, transcript parts), each blob verified locally
/// present.
pub(crate) async fn resolve_checkpoint_input_spec(
    checkpoint_id: &str,
) -> CliResult<crate::internal::ai::checkpoint_input::CheckpointInputSpec> {
    use crate::internal::ai::checkpoint_input::{CheckpointInputFile, CheckpointInputSpec};

    let conn = get_db_conn_instance().await;
    if !table_exists(&conn, "agent_checkpoint").await? {
        return Err(CliError::fatal(format!(
            "no checkpoint matches '{checkpoint_id}': agent_checkpoint table not yet present \
             (run `libra init`?)"
        )));
    }
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id, session_id, scope, parent_commit, tree_oid, \
                    metadata_blob_oid, traces_commit, created_at \
             FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
            [checkpoint_id.into()],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to query agent_checkpoint: {e}")))?;
    let Some(row) = row else {
        return Err(CliError::fatal(format!(
            "no checkpoint matches id '{checkpoint_id}'; list captured checkpoints with \
             `libra agent checkpoint list`"
        )));
    };
    let tree_oid: String = row.try_get_by("tree_oid").unwrap_or_default();
    let storage = util::try_get_storage_path(None)
        .map_err(|e| CliError::fatal(format!("not in a libra repository: {e}")))?;
    let scoped = |reason: String| {
        CliError::fatal(format!(
            "checkpoint '{checkpoint_id}' cannot be materialized as a scoped input: {reason}"
        ))
    };
    let root = read_tree_object(&storage, &tree_oid).map_err(scoped)?;
    let checkpoint_tree = subtree(&storage, &root, "checkpoint").map_err(scoped)?;
    let prefix = checkpoint_id
        .get(..2)
        .ok_or_else(|| scoped(format!("checkpoint id '{checkpoint_id}' is too short")))?;
    let prefix_tree = subtree(&storage, &checkpoint_tree, prefix).map_err(scoped)?;
    let inner = subtree(&storage, &prefix_tree, &checkpoint_id[2..]).map_err(scoped)?;

    // Walk the inner tree breadth-first, collecting every blob. Blob
    // presence is verified with the same loose-object stat the layout
    // summary uses — a checkpoint whose content is not locally present
    // must fail before any run exists, not midway through a run.
    let mut files: Vec<CheckpointInputFile> = Vec::new();
    let mut pending: Vec<(String, Tree)> = vec![(String::new(), inner)];
    while let Some((prefix, tree)) = pending.pop() {
        for item in &tree.tree_items {
            let rel_path = if prefix.is_empty() {
                item.name.clone()
            } else {
                format!("{prefix}/{}", item.name)
            };
            if item.mode == TreeItemMode::Tree {
                let child = read_tree_object(&storage, &item.id.to_string()).map_err(scoped)?;
                pending.push((rel_path, child));
            } else {
                // A gitlink has no content to materialize; a checkpoint
                // tree carrying one is malformed, not a submodule.
                if item.mode == TreeItemMode::Commit {
                    return Err(scoped(format!(
                        "entry {rel_path} is a gitlink, which has no content to materialize"
                    )));
                }
                // Re-validate the path HERE, not only in the materializer:
                // the acceptance criterion is that a malformed checkpoint
                // fails before any run exists, and a path the materializer
                // would refuse must not first cost the caller an error run.
                crate::internal::ai::checkpoint_input::sanitize_rel_path(&rel_path)
                    .map_err(scoped)?;
                let oid = item.id.to_string();
                let object_path = storage.join("objects").join(&oid[..2]).join(&oid[2..]);
                if !object_path.exists() {
                    return Err(scoped(format!(
                        "blob {oid} ({rel_path}) is not present in the local object store"
                    )));
                }
                files.push(CheckpointInputFile { rel_path, oid });
            }
        }
    }
    if files.is_empty() {
        return Err(scoped("the checkpoint tree carries no files".to_string()));
    }
    // Presence is not readability. Decode every blob under the SAME caps
    // the materializer enforces, so a corrupt, wrong-typed, or oversized
    // checkpoint is refused here — before a run row exists — instead of
    // failing halfway through materialization and leaving an error run
    // behind for the user to clean up.
    let mut total: u64 = 0;
    for file in &files {
        let oid = crate::internal::object_format::parse_repo_oid(&file.oid)
            .map_err(|e| scoped(format!("invalid blob oid '{}': {e}", file.oid)))?;
        let (bytes, truncated) = read_git_object_bounded(
            &storage,
            &oid,
            crate::internal::ai::checkpoint_input::CHECKPOINT_INPUT_MAX_FILE_BYTES,
        )
        .map_err(|e| {
            scoped(format!(
                "blob {} ({}) is not readable from the local object store: {e}",
                file.oid, file.rel_path
            ))
        })?;
        if truncated {
            return Err(scoped(format!(
                "blob {} ({}) exceeds the {}-byte per-file cap",
                file.oid,
                file.rel_path,
                crate::internal::ai::checkpoint_input::CHECKPOINT_INPUT_MAX_FILE_BYTES
            )));
        }
        total = total.saturating_add(bytes.len() as u64);
        if total > crate::internal::ai::checkpoint_input::CHECKPOINT_INPUT_MAX_TOTAL_BYTES {
            return Err(scoped(format!(
                "the checkpoint's files exceed the {}-byte total cap",
                crate::internal::ai::checkpoint_input::CHECKPOINT_INPUT_MAX_TOTAL_BYTES
            )));
        }
    }
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(CheckpointInputSpec {
        checkpoint_id: checkpoint_id.to_string(),
        files,
    })
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
        .query_one_raw(Statement::from_sql_and_values(
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
    load_checkpoint_transcript_bytes_from_storage(&storage, &row.checkpoint_id, &row.tree_oid, cap)
}

/// Read a bounded checkpoint transcript using an already-resolved repository
/// storage path. Keeping the object traversal here ensures checkpoint
/// preview/export paths agree on the E4-libra manifest/chunk rules.
pub(super) fn load_checkpoint_transcript_bytes_from_storage(
    storage: &Path,
    checkpoint_id: &str,
    tree_oid: &str,
    cap: u64,
) -> Result<(Vec<u8>, bool), CliError> {
    let root = read_tree_object(storage, tree_oid).map_err(CliError::fatal)?;
    let checkpoint_tree = subtree(storage, &root, "checkpoint").map_err(CliError::fatal)?;
    let prefix = checkpoint_id
        .get(..2)
        .ok_or_else(|| CliError::fatal(format!("checkpoint id '{checkpoint_id}' too short")))?;
    let prefix_tree = subtree(storage, &checkpoint_tree, prefix).map_err(CliError::fatal)?;
    let inner = subtree(storage, &prefix_tree, &checkpoint_id[2..]).map_err(CliError::fatal)?;

    let manifest_item = tree_entry(&inner, "manifest.json").ok_or_else(|| {
        CliError::fatal(
            "checkpoint has no manifest.json (legacy layout not exportable)".to_string(),
        )
    })?;
    // Bounded read: manifest.json is small JSON; refuse an oversized
    // (corrupt/hostile) one rather than inflate it unbounded.
    let (manifest_bytes, manifest_truncated) = read_git_object_bounded(
        storage,
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
        let hash = crate::internal::object_format::parse_repo_oid(&oid)
            .map_err(|e| CliError::fatal(format!("invalid transcript oid '{oid}': {e}")))?;
        // Bounded read: never decompress more than `remaining` content
        // bytes into memory, so a hostile/corrupt blob whose inflated size
        // dwarfs the cap cannot force an unbounded allocation.
        let (part, part_truncated) = read_git_object_bounded(storage, &hash, remaining)
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
    let hash = crate::internal::object_format::parse_repo_oid(oid)
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

fn emit_one(summary: &CheckpointShowSummary, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        let payload = serde_json::json!({
            "checkpoint": summary,
        });
        return emit_json_data("agent_checkpoint", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("checkpoint_id             : {}", summary.checkpoint_id);
    println!("scope                     : {}", summary.scope.as_str());
    println!("created_at                : {}", summary.created_at);
    println!(
        "parent_snapshot_recorded  : {}",
        if summary.parent_snapshot_recorded {
            "yes"
        } else {
            "no"
        }
    );
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
    conn.query_one_raw(stmt)
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

    /// Isolate Claude's provider-root lookup for tests that exercise the
    /// durable-identity source resolver.  The production resolver consults
    /// `LIBRA_TEST_HOME` only in tests; the named env lane prevents another
    /// test from observing this temporary provider root.
    struct TestHomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl TestHomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let prior = std::env::var_os("LIBRA_TEST_HOME");
            // SAFETY: test-only process environment mutation, restored by
            // Drop; each caller holds the serial `env` lane.
            unsafe { std::env::set_var("LIBRA_TEST_HOME", path) };
            Self { prior }
        }
    }

    impl Drop for TestHomeGuard {
        fn drop(&mut self) {
            // SAFETY: paired with `set`; this test-only guard restores the
            // prior process environment value before releasing the env lane.
            unsafe {
                match &self.prior {
                    Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                    None => std::env::remove_var("LIBRA_TEST_HOME"),
                }
            }
        }
    }

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
                .execute_raw(Statement::from_string(backend, trimmed.to_string()))
                .await
                .unwrap_or_else(|e| panic!("legacy bootstrap stmt failed: {trimmed}\n{e}"));
        }
        ensure_ai_runtime_contract_schema(&conn).await.unwrap();
        run_builtin_migrations(&conn).await.unwrap();
        (dir, conn)
    }

    #[test]
    fn unsafe_provider_rewind_is_explicitly_unsupported_in_json() {
        let outcome = super::TranscriptTruncationOutcome::SkippedUnsafeMutation;
        let json = outcome.as_json();
        assert_eq!(json["supported"], false);
        assert_eq!(json["applied"], false);
        assert!(
            json["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("identity-checked replacement")),
            "the output names why the provider transcript was left untouched: {json}"
        );
    }

    /// Rewind must derive the Claude source from durable identity, ignoring a
    /// conflicting legacy metadata pointer, then fail closed because no safe
    /// atomic identity-checked provider rewrite primitive is available.
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn rewind_truncate_refuses_derived_claude_source_without_safe_rewrite() {
        let (dir, conn) = fresh_db().await;
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        let _home = TestHomeGuard::set(&home);
        let working_dir = dir.path().join("workspace");
        fs::create_dir(&working_dir).unwrap();
        // Create an on-disk transcript whose later line would have been
        // dropped by the retired rewrite path. It must remain intact.
        let transcript_dir = crate::internal::ai::observed_agents::claude_session_dir(&working_dir)
            .expect("LIBRA_TEST_HOME provides a Claude session directory");
        fs::create_dir_all(&transcript_dir).unwrap();
        let transcript_path = transcript_dir.join("p-1.jsonl");
        fs::write(
            &transcript_path,
            b"{\"timestamp\":\"2026-05-05T10:00:00Z\",\"text\":\"keep\"}\n\
              {\"timestamp\":\"2026-05-05T11:00:00Z\",\"text\":\"drop\"}\n",
        )
        .unwrap();
        let forged_pointer = dir.path().join("unrelated.jsonl");
        fs::write(
            &forged_pointer,
            b"{\"timestamp\":\"2099-01-01T00:00:00Z\",\"text\":\"forged\"}\n",
        )
        .unwrap();
        let metadata_json = serde_json::json!({
            "transcript_path": forged_pointer,
        })
        .to_string();
        let created_at = 0i64;

        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES ('s-1', 'claude_code', 'p-1', 'stopped', ?, ?, '{}', 0, 0)",
            [
                working_dir.to_string_lossy().to_string().into(),
                metadata_json.into(),
            ],
        ))
        .await
        .unwrap();
        conn.execute_raw(Statement::from_sql_and_values(
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
        assert!(matches!(
            outcome,
            super::TranscriptTruncationOutcome::SkippedUnsafeMutation
        ));

        let after = fs::read_to_string(&transcript_path).unwrap();
        assert!(after.contains("\"keep\""));
        assert!(after.contains("\"drop\""));
        assert!(
            fs::read_to_string(&forged_pointer)
                .unwrap()
                .contains("\"forged\""),
            "the untrusted metadata pointer must never be used or mutated"
        );
    }

    /// A valid durable session whose provider file is absent must surface a
    /// no-source outcome rather than reviving a metadata pointer fallback.
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn rewind_truncate_skips_when_no_derived_source_is_available() {
        let (dir, conn) = fresh_db().await;
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        let _home = TestHomeGuard::set(&home);
        let working_dir = dir.path().join("workspace");
        fs::create_dir(&working_dir).unwrap();
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES ('s-2', 'claude_code', 'p-2', 'stopped', ?, '{}', '{}', 0, 0)",
            [working_dir.to_string_lossy().to_string().into()],
        ))
        .await
        .unwrap();
        conn.execute_raw(Statement::from_sql_and_values(
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
            super::TranscriptTruncationOutcome::SkippedNoDerivedSource
        ));
    }

    /// Dry-run must remain faithful to the fail-closed apply policy even when
    /// a derived Claude source exists and legacy metadata names another file.
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn lookup_truncation_support_is_false_without_safe_mutation_primitive() {
        let (dir, conn) = fresh_db().await;
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        let _home = TestHomeGuard::set(&home);
        let working_dir = dir.path().join("workspace");
        fs::create_dir(&working_dir).unwrap();
        let backend = conn.get_database_backend();

        let transcript_dir = crate::internal::ai::observed_agents::claude_session_dir(&working_dir)
            .expect("LIBRA_TEST_HOME provides a Claude session directory");
        fs::create_dir_all(&transcript_dir).unwrap();
        let transcript_path = transcript_dir.join("p-0.jsonl");
        fs::write(&transcript_path, b"").unwrap();
        let path_meta = serde_json::json!({
            "transcript_path": dir.path().join("forged.jsonl"),
        })
        .to_string();

        // Even Claude Code plus a valid derived source cannot report support
        // until an atomic identity-checked rewrite primitive exists.
        for (idx, (kind, meta)) in [
            ("claude_code", path_meta.as_str()), // derived source exists
            ("claude_code", "{}"),               // skipped (no derived file)
            ("cursor", path_meta.as_str()),      // skipped (kind)
            ("cursor", "{}"),                    // skipped (both)
        ]
        .iter()
        .enumerate()
        {
            let session_id = format!("s-{idx}");
            let provider_session_id = format!("p-{idx}");
            let checkpoint_id = format!("cp-{idx}");
            conn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO agent_session (
                    session_id, agent_kind, provider_session_id, state, working_dir,
                    metadata_json, redaction_report, started_at, last_event_at
                 ) VALUES (?, ?, ?, 'stopped', ?, ?, '{}', 0, 0)",
                [
                    session_id.clone().into(),
                    (*kind).into(),
                    provider_session_id.into(),
                    working_dir.to_string_lossy().to_string().into(),
                    (*meta).into(),
                ],
            ))
            .await
            .unwrap();
            conn.execute_raw(Statement::from_sql_and_values(
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
            let expected = false;
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
        let (_dir, conn) = fresh_db().await;

        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES ('s-3', 'cursor', 'p-3', 'stopped', '/tmp', '{}', '{}', 0, 0)",
            [],
        ))
        .await
        .unwrap();
        conn.execute_raw(Statement::from_sql_and_values(
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
            super::TranscriptTruncationOutcome::SkippedUnsupportedKind => {}
            other => panic!("expected SkippedUnsupportedKind, got {:?}", other.as_json()),
        }
    }

    /// A corrupt catalog identity must be surfaced as a failed truncation,
    /// not silently decoded as an empty path/source and reported as a safe
    /// skip. SQLite permits a BLOB in a TEXT-affinity column, which models a
    /// malformed durable row from a damaged or manually edited catalog.
    #[tokio::test]
    async fn rewind_truncate_reports_corrupt_durable_identity_field() {
        let (_dir, conn) = fresh_db().await;
        let backend = conn.get_database_backend();

        conn.execute_raw(Statement::from_string(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at
             ) VALUES (
                's-corrupt', 'claude_code', 'p-corrupt', 'stopped', X'FF',
                '{}', '{}', 0, 0
             )"
            .to_string(),
        ))
        .await
        .unwrap();
        conn.execute_raw(Statement::from_string(
            backend,
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES (
                'cp-corrupt', 's-corrupt', 'committed', NULL, 't', 'm', 'c', 0
             )"
            .to_string(),
        ))
        .await
        .unwrap();

        let outcome =
            super::truncate_agent_transcript_for_checkpoint_with_conn(&conn, "cp-corrupt").await;
        match outcome {
            super::TranscriptTruncationOutcome::Failed => {}
            other => panic!(
                "expected corrupt catalog failure, got {:?}",
                other.as_json()
            ),
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
                .query_all_raw(Statement::from_sql_and_values(
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
