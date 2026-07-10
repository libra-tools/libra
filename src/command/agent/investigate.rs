//! Top-level `libra investigate` command family (AG-23 read-only agent
//! investigate; plan.md Task A8, `agent.md` 落地执行补充规格 §5).
//!
//! The CLI face is a **top-level** command (`Commands::Investigate` in
//! `src/cli.rs`); the implementation lives under `command/agent/` so the
//! AG-20 keyset-pagination helpers in [`super::checkpoint`]
//! (`resolve_page_limit` / `encode_page_cursor` / `decode_page_cursor`,
//! all `pub(super)`) are reused verbatim for `investigate list` — the same
//! reuse `libra review` makes.
//!
//! The engine half — round-robin run store, turn loop, quorum/max-turns/
//! pause states, `agent.investigate.run` span — lives in
//! [`crate::internal::ai::investigate`], which itself reuses A7's launcher,
//! sink, redaction, and isolation seam. This module only parses arguments,
//! derives run inputs, wires SIGINT/SIGTERM into the shared cancel path,
//! and renders output through the standard [`OutputConfig`] conventions.
//!
//! Security posture (enforced by the engine, surfaced here):
//! - investigators run in an isolated workspace, never the repo worktree;
//! - the topic is an untrusted seed — redacted + spotlit before any prompt
//!   injection, ANSI-stripped before display;
//! - `investigate fix` fails closed with `LBR-AGENT-010` until the internal
//!   AgentRuntime fix bridge lands; a mutation driven by an untrusted seed
//!   without explicit approval maps to `LBR-AGENT-011`.

use std::time::Duration;

use clap::{Args, Subcommand};
use serde::Serialize;

use super::checkpoint::{
    PAGE_SCHEMA_VERSION, decode_page_cursor, encode_page_cursor, resolve_page_limit,
};
use crate::{
    internal::{
        ai::{
            investigate::{
                DEFAULT_CLAUDE_REVIEW_MAX_BUDGET_USD, DEFAULT_INVESTIGATOR_TIMEOUT,
                InvestigateCancelHandle, InvestigateRunCursor, InvestigateRunError,
                InvestigateRunOutcome, InvestigateRunRequest, InvestigateRunStore,
                InvestigateRunSummary, InvestigateTerminalState, InvestigatorSource, PauseReason,
                is_launchable_investigator, render_untrusted_findings, run_investigate,
            },
            run_admission::{self, RejectedAdmission},
        },
        head::Head,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

/// Default turn budget when `--max-turns` is not given.
const DEFAULT_MAX_TURNS: u32 = 6;

/// `--help` examples for `libra investigate` (cross-cutting EXAMPLES
/// contract; pinned by `tests/compat/help_examples_banner.rs`).
pub const INVESTIGATE_EXAMPLES: &str = "\
EXAMPLES:
    libra investigate start --topic \"why is startup slow\" --agent codex        Start a round-robin investigation with one agent
    libra investigate start --topic \"auth bug\" --agent codex --agent claude-code  Round-robin across two agents (strict, one at a time)
    libra investigate start --topic \"leak\" --agent codex --max-turns 8 --quorum 2  Bound turns and require 2 concluding agents
    libra investigate list                                     List investigate runs, newest first (default 50 per page)
    libra investigate list --limit 10 --cursor <token>        Next keyset page (token = previous page's next_cursor)
    libra investigate show <run_id>                           State, stances and sanitized findings
    libra investigate show <run_id> --json                    The same run record as JSON
    libra investigate continue <run_id>                       Resume a paused (stalled / agent-failure) run
    libra investigate cancel <run_id>                         Cancel a run (same cleanup path as Ctrl-C)
    libra investigate clean --run <run_id>                    Remove one finished run directory
    libra investigate clean --all                             Remove every finished run directory
    libra investigate attach <run_id> <file>                  Attach an external file to a run (provenance=manual)

    `libra investigate fix` is not supported yet: it requires the internal
    AgentRuntime fix bridge and fails with LBR-AGENT-010 until that lands.";

// ---------------------------------------------------------------------------
// Clap surface (agent.md §5 exact)
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
#[command(after_help = INVESTIGATE_EXAMPLES)]
pub struct InvestigateArgs {
    #[command(subcommand)]
    pub command: InvestigateSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum InvestigateSubcommand {
    /// Start a strict round-robin read-only investigation.
    #[command(about = "Start a round-robin read-only investigation")]
    Start(InvestigateStartArgs),
    /// List investigate runs, newest first (keyset pagination).
    #[command(about = "List investigate runs (newest first, keyset pagination)")]
    List(InvestigateListArgs),
    /// Show one run: state, stances and sanitized findings.
    #[command(about = "Show an investigate run's state, stances and findings")]
    Show(InvestigateShowArgs),
    /// Resume a paused run from its pending turn.
    #[command(name = "continue", about = "Resume a paused investigate run")]
    Continue(InvestigateContinueArgs),
    /// Cancel a run (shares the cleanup path with foreground Ctrl-C).
    #[command(about = "Cancel an investigate run")]
    Cancel(InvestigateCancelArgs),
    /// Remove investigate run directories.
    #[command(about = "Remove finished investigate run directories")]
    Clean(InvestigateCleanArgs),
    /// Apply investigation findings via the internal fix bridge (not
    /// available yet — fails with LBR-AGENT-010).
    #[command(about = "Apply investigation findings via the fix bridge (unsupported yet)")]
    Fix(InvestigateFixArgs),
    /// Attach an external file to a run's audit chain (provenance=manual).
    #[command(
        about = "Attach an external transcript/findings/context file to a run (provenance=manual)"
    )]
    Attach(InvestigateAttachArgs),
}

#[derive(Args, Debug)]
pub struct InvestigateAttachArgs {
    /// Run identifier from `libra investigate list`.
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
    /// Path to the external file to attach. Its bytes are redacted, written
    /// to the object store (object_index-tagged), and recorded in the run
    /// manifest's `manual_attach` list with `provenance=manual`.
    #[arg(value_name = "FILE")]
    pub file: std::path::PathBuf,
}

#[derive(Args, Debug)]
pub struct InvestigateStartArgs {
    /// The investigation topic (untrusted seed text — redacted and
    /// spotlit before it ever reaches an agent prompt).
    #[arg(long, value_name = "TEXT")]
    pub topic: String,
    /// Investigator agent slug (repeatable, defines round-robin order).
    /// First-batch launchable agents: claude-code, codex, opencode.
    #[arg(long = "agent", value_name = "SLUG", required = true)]
    pub agents: Vec<String>,
    /// Maximum number of investigator turns (default 6).
    #[arg(long, value_name = "N")]
    pub max_turns: Option<u32>,
    /// Number of distinct agents that must submit a concluding stance for
    /// the run to reach quorum (default: the number of agents).
    #[arg(long, value_name = "N")]
    pub quorum: Option<u32>,
}

#[derive(Args, Debug)]
pub struct InvestigateListArgs {
    /// Maximum rows to return (default 50, capped at 500).
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
    /// Keyset cursor from the previous page's `next_cursor` (opaque).
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
}

#[derive(Args, Debug)]
pub struct InvestigateShowArgs {
    /// Run identifier from `libra investigate list`.
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
}

#[derive(Args, Debug)]
pub struct InvestigateContinueArgs {
    /// Run identifier from `libra investigate list`.
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
}

#[derive(Args, Debug)]
pub struct InvestigateCancelArgs {
    /// Run identifier from `libra investigate list`.
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
}

#[derive(Args, Debug)]
pub struct InvestigateCleanArgs {
    /// Remove one run directory by id.
    #[arg(long, value_name = "RUN_ID", conflicts_with = "all")]
    pub run: Option<String>,
    /// Remove every finished run directory (running runs are skipped).
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct InvestigateFixArgs {
    /// Run identifier from `libra investigate list`.
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
}

pub async fn execute_safe(args: InvestigateArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        InvestigateSubcommand::Start(a) => start(a, output).await,
        InvestigateSubcommand::List(a) => list(a, output).await,
        InvestigateSubcommand::Show(a) => show(a, output).await,
        InvestigateSubcommand::Continue(a) => resume(a, output).await,
        InvestigateSubcommand::Cancel(a) => cancel(a, output).await,
        InvestigateSubcommand::Clean(a) => clean(a, output).await,
        InvestigateSubcommand::Fix(a) => fix(a, output).await,
        InvestigateSubcommand::Attach(a) => attach(a, output).await,
    }
}

/// A0-06: `libra investigate attach <run_id> <file>` — attach an external file
/// to a run's audit chain (`provenance=manual`). The file bytes are redacted,
/// objectized into the object store + `object_index`, and recorded as a
/// `manual_attach` manifest entry. Never mutates findings or run state.
async fn attach(args: InvestigateAttachArgs, output: &OutputConfig) -> CliResult<()> {
    let store = open_store()?;
    store
        .load_state(&args.run_id)
        .map_err(|e| map_store_error("failed to read investigate run state", e))?
        .ok_or_else(|| run_not_found(&args.run_id))?;

    // Sanitize the record/display name FIRST, so a hostile path never reaches
    // the read-error either.
    let name = super::sanitize_attachment_name(&args.file);
    let raw = std::fs::read(&args.file)
        .map_err(|e| CliError::fatal(format!("failed to read attach file '{name}': {e}")))?;
    let (redacted, _report) = crate::internal::ai::review::redact_untrusted(&raw);
    let oid = store
        .objectize_bytes(redacted.as_bytes())
        .map_err(|e| map_store_error("failed to objectize the attachment", e))?;

    let attached_at = crate::internal::ai::review::store::utc_timestamp();
    let entry = serde_json::json!({
        "oid": oid,
        "name": name,
        "provenance": "manual",
        "size": redacted.len(),
        "attached_at": attached_at,
    });

    let mut manifest = store
        .load_manifest(&args.run_id)
        .map_err(|e| map_store_error("failed to read investigate run manifest", e))?
        .ok_or_else(|| run_not_found(&args.run_id))?;
    manifest.manual_attach.push(entry);
    manifest.updated_at = attached_at;
    let attachments = manifest.manual_attach.len();
    store
        .write_manifest(&manifest)
        .map_err(|e| map_store_error("failed to record the attachment in the manifest", e))?;

    if output.is_json() {
        let payload = serde_json::json!({
            "schema_version": PAGE_SCHEMA_VERSION,
            "run_id": args.run_id,
            "oid": oid,
            "name": name,
            "attachments": attachments,
        });
        return emit_json_data("investigate_attach", &payload, output);
    }
    if !output.quiet {
        println!(
            "attached {name} to investigate run {} ({attachments} attachment(s) total)",
            args.run_id
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Error helpers (LBR-AGENT-010 / 011)
// ---------------------------------------------------------------------------

/// The stable `investigate fix` refusal (plan.md:1002 / card A8): the
/// internal serialized fix bridge has no source anchor yet, so the verb
/// fails closed with `LBR-AGENT-010` — never fakes success. Shares the
/// exact semantic `review --fix` uses (A7 owns the code; A8 reuses it).
fn fix_bridge_unavailable_error() -> CliError {
    CliError::fatal(
        "investigate fix requires the internal AgentRuntime fix bridge, which has \
         not landed yet; the investigation stays read-only — its findings remain \
         available via `libra investigate show <run_id>`. `fix` will only be \
         enabled once the internal serialized fix bridge (with approval/sandbox/\
         tool gates) exists.",
    )
    .with_stable_code(StableErrorCode::AgentFixBridgeUnavailable)
}

/// The untrusted-seed-for-mutation refusal (plan.md:1002 / card A8): an
/// investigation topic is always an untrusted seed, so once the fix bridge
/// exists, driving a MUTATING fix from that seed without explicit approval
/// must fail closed with `LBR-AGENT-011` BEFORE the bridge is entered. The
/// bridge being unavailable means `fix` currently returns `LBR-AGENT-010`
/// first (the dominant precondition, matching the read-only acceptance);
/// this helper (and its message) pins the 011 semantic so the gate is ready
/// the moment the bridge lands. Exercised by the mapping unit test.
#[allow(dead_code)]
fn untrusted_seed_for_mutation_error() -> CliError {
    CliError::fatal(
        "the investigation topic is untrusted seed content and cannot drive a \
         mutating fix workflow without explicit approval; the read-only \
         investigation is available (`libra investigate show <run_id>`). A future \
         mutating fix must be authorized explicitly (provenance=untrusted seeds \
         never auto-enter a mutating workflow).",
    )
    .with_stable_code(StableErrorCode::AgentUntrustedSeedForMutation)
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

fn open_store() -> CliResult<InvestigateRunStore> {
    let storage = util::try_get_storage_path(None).map_err(|e| {
        CliError::fatal(format!(
            "not in a libra repository ({e}); run `libra investigate` from inside a repository"
        ))
    })?;
    Ok(InvestigateRunStore::new(storage.join("sessions")))
}

/// A0-04: fail-closed error when the shared review/investigate run queue is
/// full. Carries `LBR-AGENT-014` (the same code `libra review` emits).
fn run_queue_full_error(rejected: RejectedAdmission) -> CliError {
    CliError::fatal(format!(
        "too many concurrent review/investigate runs: {} active, {} queued (queue cap {}); \
         refusing to start another",
        rejected.active, rejected.queued, rejected.cap
    ))
    .with_stable_code(StableErrorCode::AgentRunQueueFull)
    .with_hint("wait for a running review/investigate run to finish, or cancel one with `libra investigate cancel <run_id>` / `libra review cancel <run_id>`")
    .with_hint("raise the limit with `libra config set agent.max_concurrent_runs <N>` (default 2)")
}

fn map_store_error(context: &str, error: std::io::Error) -> CliError {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        CliError::command_usage(error.to_string())
    } else {
        CliError::fatal(format!("{context}: {error}"))
    }
}

fn run_not_found(run_id: &str) -> CliError {
    CliError::fatal(format!(
        "no investigate run matches id '{run_id}'; run `libra investigate list` for known run ids"
    ))
}

fn encode_investigate_cursor(cursor: &InvestigateRunCursor) -> CliResult<String> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(&cursor.started_at)
        .map_err(|e| {
            CliError::fatal(format!(
                "investigate run '{}' has an unparseable started_at '{}' ({e}); \
                 its state.json may be corrupt — inspect it with `libra investigate show {}`",
                cursor.run_id, cursor.started_at, cursor.run_id
            ))
        })?
        .timestamp_micros();
    Ok(encode_page_cursor(timestamp, &cursor.run_id))
}

fn decode_investigate_cursor(cursor: &str) -> CliResult<InvestigateRunCursor> {
    let (timestamp, run_id) = decode_page_cursor(cursor)?;
    let started_at = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(timestamp)
        .ok_or_else(|| {
            CliError::command_usage(format!(
                "invalid --cursor '{cursor}': pass the opaque next_cursor value from the \
                 previous page's output unmodified (cursors cannot be hand-built)"
            ))
        })?
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    Ok(InvestigateRunCursor { started_at, run_id })
}

fn terminal_label(state: Option<InvestigateTerminalState>, pause: Option<PauseReason>) -> String {
    match (state, pause) {
        (Some(state), _) => state.as_str().to_string(),
        (None, Some(reason)) => format!("paused ({})", reason.as_str()),
        (None, None) => "running".to_string(),
    }
}

/// Resolve on SIGINT/ctrl-c or SIGTERM, then trip the shared cancel handle
/// — foreground signals and `investigate cancel` funnel into the SAME
/// engine cleanup path.
async fn cancel_on_signal(cancel: InvestigateCancelHandle) {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    cancel.cancel();
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

async fn start(args: InvestigateStartArgs, output: &OutputConfig) -> CliResult<()> {
    // Dedupe agents while preserving round-robin order.
    let mut agents: Vec<String> = Vec::with_capacity(args.agents.len());
    for slug in &args.agents {
        if !agents.contains(slug) {
            agents.push(slug.clone());
        }
    }
    if agents.is_empty() {
        return Err(CliError::command_usage(
            "pass at least one investigator with --agent <slug>",
        ));
    }
    for slug in &agents {
        if !is_launchable_investigator(slug) {
            return Err(CliError::fatal(format!(
                "agent '{slug}' is not launchable for investigate; first-batch launchable \
                 agents: {}",
                crate::internal::ai::observed_agents::launchable_investigate_slugs().join(", ")
            )));
        }
    }
    if args.topic.trim().is_empty() {
        return Err(CliError::command_usage(
            "--topic must not be empty; describe what to investigate",
        ));
    }

    let max_turns = args.max_turns.unwrap_or(DEFAULT_MAX_TURNS);
    if max_turns == 0 {
        return Err(CliError::command_usage("--max-turns must be at least 1"));
    }
    // Default quorum = consensus of all agents; clamp to [1, agents.len()].
    let requested_quorum = args.quorum.unwrap_or(agents.len() as u32);
    let quorum = requested_quorum.clamp(1, agents.len() as u32);
    if requested_quorum > agents.len() as u32 && !output.quiet {
        eprintln!(
            "note: --quorum {requested_quorum} exceeds the {} agent(s) given; clamping to {quorum} \
             (quorum counts DISTINCT concluding agents)",
            agents.len()
        );
    }

    let repo_root = util::try_working_dir().map_err(|e| {
        CliError::fatal(format!(
            "not in a libra repository ({e}); run `libra investigate` from inside a repository"
        ))
    })?;
    let store = open_store()?;
    let starting_sha = Head::current_commit()
        .await
        .map(|oid| oid.to_string())
        .ok_or_else(|| {
            CliError::fatal(
                "cannot start an investigation: HEAD has no commit yet; create an initial \
                 commit first (`libra add … && libra commit`)",
            )
        })?;

    let sources: Vec<InvestigatorSource> = agents
        .iter()
        .map(|slug| InvestigatorSource::Builtin { slug: slug.clone() })
        .collect();
    let request = InvestigateRunRequest::new(
        repo_root,
        args.topic.clone(),
        starting_sha,
        sources,
        max_turns,
        quorum,
    );

    // A0-04: acquire a shared run-level admission slot (the same queue
    // `libra review` uses). Over `agent.max_concurrent_runs` this blocks in
    // the queue; a full queue fails closed with `LBR-AGENT-014`. Held for the
    // run's lifetime (RAII), released on completion / cancel / failure.
    let max_runs = run_admission::max_concurrent_runs().await.map_err(|e| {
        CliError::fatal(format!(
            "failed to resolve {}: {e}",
            run_admission::MAX_CONCURRENT_RUNS_KEY
        ))
    })?;
    let _run_slot = match run_admission::admit_blocking(
        &store.runs_root(),
        max_runs,
        run_admission::RUN_QUEUE_CAP,
        None,
    )
    .await
    .map_err(|e| CliError::fatal(format!("run admission failed: {e}")))?
    {
        Ok(slot) => slot,
        Err(rejected) => return Err(run_queue_full_error(rejected)),
    };

    if !output.quiet && !output.is_json() {
        println!(
            "starting investigation: {} agent(s), max {max_turns} turn(s), quorum {quorum} \
             (strict round-robin; Ctrl-C cancels)",
            agents.len()
        );
    }

    let cancel = InvestigateCancelHandle::new();
    let signal_task = tokio::spawn(cancel_on_signal(cancel.clone()));
    let result = run_investigate(&store, request, cancel).await;
    signal_task.abort();
    let outcome = result.map_err(map_run_error)?;

    emit_run_outcome(&outcome, output)?;

    if outcome.terminal_state == Some(InvestigateTerminalState::Error) {
        let detail = outcome
            .infra_error
            .as_deref()
            .unwrap_or("investigation failed to run");
        return Err(CliError::fatal(format!(
            "investigate run {} ended in state 'error' ({detail}); inspect it with \
             `libra investigate show {}`",
            outcome.run_id, outcome.run_id
        )));
    }
    Ok(())
}

fn map_run_error(error: InvestigateRunError) -> CliError {
    match error {
        InvestigateRunError::NoInvestigators
        | InvestigateRunError::InvalidMaxTurns
        | InvestigateRunError::InvalidQuorum => CliError::command_usage(error.to_string()),
        InvestigateRunError::UnsupportedInvestigator { .. }
        | InvestigateRunError::RunLocked { .. }
        | InvestigateRunError::NotFound { .. }
        | InvestigateRunError::AlreadyTerminal { .. } => CliError::fatal(error.to_string()),
        InvestigateRunError::Store(inner) => {
            map_store_error("failed to persist investigate run state", inner)
        }
    }
}

#[derive(Debug, Serialize)]
struct StanceRow {
    turn: u32,
    slug: String,
    disposition: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    stdout_truncated: bool,
}

fn emit_run_outcome(outcome: &InvestigateRunOutcome, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        let payload = serde_json::json!({
            "schema_version": PAGE_SCHEMA_VERSION,
            "run_id": outcome.run_id,
            "terminal_state": outcome.terminal_state.map(|s| s.as_str()),
            "pause_reason": outcome.pause_reason.map(|r| r.as_str()),
            "turns_executed": outcome.turns_executed,
            "completed_rounds": outcome.completed_rounds,
            "stance_count": outcome.stance_count,
            "concluding_count": outcome.concluding_count,
            "duration_ms": outcome.duration_ms,
            "run_dir": outcome.run_dir.display().to_string(),
            "infra_error": outcome.infra_error,
        });
        return emit_json_data("investigate_run", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!(
        "investigate run {}: {} ({} turn(s), {} ms)",
        outcome.run_id,
        terminal_label(outcome.terminal_state, outcome.pause_reason),
        outcome.turns_executed,
        outcome.duration_ms
    );
    if outcome.terminal_state.is_none() {
        println!("resume with: libra investigate continue {}", outcome.run_id);
    }
    println!("next: libra investigate show {}", outcome.run_id);
    Ok(())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct InvestigateListPage {
    schema_version: u32,
    items: Vec<InvestigateRunSummary>,
    next_cursor: Option<String>,
    has_more: bool,
}

async fn list(args: InvestigateListArgs, output: &OutputConfig) -> CliResult<()> {
    let (limit, clamp_note) = resolve_page_limit(args.limit);
    if let Some(note) = &clamp_note {
        eprintln!("{note}");
    }
    let cursor = args
        .cursor
        .as_deref()
        .map(decode_investigate_cursor)
        .transpose()?;
    let store = open_store()?;
    let page = store
        .list_runs_page(cursor.as_ref(), limit as usize)
        .map_err(|e| CliError::fatal(format!("failed to list investigate runs: {e}")))?;
    let next_cursor = page
        .next_cursor
        .as_ref()
        .map(encode_investigate_cursor)
        .transpose()?;
    let page = InvestigateListPage {
        schema_version: PAGE_SCHEMA_VERSION,
        items: page.items,
        next_cursor,
        has_more: page.has_more,
    };
    if output.is_json() {
        return emit_json_data("investigate_list", &page, output);
    }
    if output.quiet {
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no investigate runs)");
        return Ok(());
    }
    println!(
        "{:<37}  {:<16}  {:<7}  {:<24}  topic",
        "run_id", "state", "turns", "started_at"
    );
    for run in &page.items {
        println!(
            "{:<37}  {:<16}  {:<7}  {:<24}  {}",
            run.run_id,
            terminal_label(run.terminal_state, run.pause_reason),
            format!("{}/{}", run.turn, run.max_turns),
            run.started_at,
            truncate_topic(&run.topic),
        );
    }
    if let Some(cursor) = &page.next_cursor {
        println!("(more rows available — next page: --cursor {cursor})");
    }
    Ok(())
}

fn truncate_topic(topic: &str) -> String {
    let sanitized = render_untrusted_findings(topic).replace('\n', " ");
    if sanitized.chars().count() > 48 {
        let clipped: String = sanitized.chars().take(47).collect();
        format!("{clipped}…")
    } else {
        sanitized
    }
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

async fn show(args: InvestigateShowArgs, output: &OutputConfig) -> CliResult<()> {
    let store = open_store()?;
    let state = store
        .load_state(&args.run_id)
        .map_err(|e| map_store_error("failed to read investigate run state", e))?
        .ok_or_else(|| run_not_found(&args.run_id))?;
    let manifest = store
        .load_manifest(&args.run_id)
        .map_err(|e| map_store_error("failed to read investigate run manifest", e))?;
    // findings.md and the topic are untrusted: ALWAYS rendered through the
    // ANSI/control-stripping sanitizer, in both human and JSON output.
    let findings = store
        .read_findings(&args.run_id)
        .map_err(|e| map_store_error("failed to read investigate findings", e))?
        .map(|raw| render_untrusted_findings(&raw));

    if output.is_json() {
        let stances: Vec<StanceRow> = state
            .stances
            .iter()
            .map(|s| StanceRow {
                turn: s.turn,
                slug: s.slug.clone(),
                disposition: s.disposition.as_str(),
                exit_code: s.exit_code,
                stdout_truncated: s.stdout_truncated,
            })
            .collect();
        let payload = serde_json::json!({
            "schema_version": PAGE_SCHEMA_VERSION,
            "run_id": state.run_id,
            "kind": state.kind,
            "topic": render_untrusted_findings(&state.topic),
            "agents": state.agents,
            "max_turns": state.max_turns,
            "quorum": state.quorum,
            "turn": state.turn,
            "completed_rounds": state.completed_rounds,
            "next_agent_idx": state.next_agent_idx,
            "concluding_count": state.concluding_agent_count(),
            "terminal_state": state.terminal_state.map(|s| s.as_str()),
            "pending_turn": state.pending_turn,
            "starting_sha": state.starting_sha,
            "started_at": state.started_at,
            "updated_at": state.updated_at,
            "stances": stances,
            "manifest": manifest,
            "findings": findings,
        });
        return emit_json_data("investigate_show", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("run_id           : {}", state.run_id);
    println!("kind             : {}", state.kind);
    println!(
        "topic            : {}",
        render_untrusted_findings(&state.topic).replace('\n', " ")
    );
    println!("agents           : {}", state.agents.join(", "));
    println!(
        "state            : {}",
        terminal_label(
            state.terminal_state,
            state.pending_turn.as_ref().map(|p| p.reason)
        )
    );
    println!(
        "progress         : turn {}/{}, round {}, quorum {}/{}",
        state.turn,
        state.max_turns,
        state.completed_rounds,
        state.concluding_agent_count(),
        state.quorum
    );
    println!("next_agent_idx   : {}", state.next_agent_idx);
    println!("starting_sha     : {}", state.starting_sha);
    println!("started_at       : {}", state.started_at);
    println!("updated_at       : {}", state.updated_at);
    if let Some(pending) = &state.pending_turn {
        println!(
            "pending_turn     : turn {} (agent {} '{}', {})",
            pending.turn, pending.agent_idx, pending.slug, pending.reason
        );
        println!(
            "resume with      : libra investigate continue {}",
            state.run_id
        );
    }
    println!("stances:");
    if state.stances.is_empty() {
        println!("  (none yet)");
    }
    for stance in &state.stances {
        let detail = match stance.exit_code {
            Some(code) => format!(" (exit {code})"),
            None => String::new(),
        };
        println!(
            "  turn {:<3} {:<14} {}{detail}",
            stance.turn,
            stance.slug,
            stance.disposition.as_str()
        );
    }
    if let Some(manifest) = &manifest {
        println!("manifest:");
        println!(
            "  redaction      : {} match(es), {} byte(s) redacted of {} scanned",
            manifest.redaction_report.matches,
            manifest.redaction_report.bytes_redacted,
            manifest.redaction_report.bytes_scanned
        );
        println!(
            "  findings_oid   : {}",
            manifest.findings_oid.as_deref().unwrap_or("(none)")
        );
    }
    match findings {
        Some(text) if !text.trim().is_empty() => {
            println!("---");
            println!("findings.md (sanitized — investigator output is untrusted):");
            println!("{text}");
        }
        _ => println!("(no findings recorded yet)"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// continue
// ---------------------------------------------------------------------------

async fn resume(args: InvestigateContinueArgs, output: &OutputConfig) -> CliResult<()> {
    let repo_root = util::try_working_dir().map_err(|e| {
        CliError::fatal(format!(
            "not in a libra repository ({e}); run `libra investigate` from inside a repository"
        ))
    })?;
    let store = open_store()?;

    if !output.quiet && !output.is_json() {
        println!(
            "resuming investigation {} (strict round-robin; Ctrl-C cancels)",
            args.run_id
        );
    }
    let cancel = InvestigateCancelHandle::new();
    let signal_task = tokio::spawn(cancel_on_signal(cancel.clone()));
    let result = crate::internal::ai::investigate::continue_investigate(
        &store,
        &args.run_id,
        &repo_root,
        DEFAULT_INVESTIGATOR_TIMEOUT,
        true,
        DEFAULT_CLAUDE_REVIEW_MAX_BUDGET_USD,
        cancel,
    )
    .await;
    signal_task.abort();
    let outcome = result.map_err(map_run_error)?;
    emit_run_outcome(&outcome, output)?;
    if outcome.terminal_state == Some(InvestigateTerminalState::Error) {
        return Err(CliError::fatal(format!(
            "investigate run {} ended in state 'error'; inspect it with \
             `libra investigate show {}`",
            outcome.run_id, outcome.run_id
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cancel
// ---------------------------------------------------------------------------

const CANCEL_ACK_POLLS: u32 = 15;
const CANCEL_ACK_POLL_INTERVAL: Duration = Duration::from_millis(200);

async fn cancel(args: InvestigateCancelArgs, output: &OutputConfig) -> CliResult<()> {
    let store = open_store()?;
    let state = store
        .load_state(&args.run_id)
        .map_err(|e| map_store_error("failed to read investigate run state", e))?
        .ok_or_else(|| run_not_found(&args.run_id))?;
    if state.is_terminal() {
        let terminal = terminal_label(state.terminal_state, None);
        if output.is_json() {
            let payload = serde_json::json!({
                "schema_version": PAGE_SCHEMA_VERSION,
                "run_id": args.run_id,
                "cancelled": false,
                "mode": "already-terminal",
                "terminal_state": terminal,
            });
            return emit_json_data("investigate_cancel", &payload, output);
        }
        if !output.quiet {
            println!(
                "investigate run {} is already terminal ({terminal}); nothing to cancel",
                args.run_id
            );
        }
        return Ok(());
    }

    // Drop the cross-process cancel marker a live driver polls; the owning
    // driver then runs the SAME cleanup used by foreground SIGINT/SIGTERM.
    store
        .mark_cancel_requested(&args.run_id)
        .map_err(|e| map_store_error("failed to write the cancel-request marker", e))?;
    let mut acknowledged = false;
    for _ in 0..CANCEL_ACK_POLLS {
        tokio::time::sleep(CANCEL_ACK_POLL_INTERVAL).await;
        let now = store
            .load_state(&args.run_id)
            .map_err(|e| map_store_error("failed to re-read investigate run state", e))?;
        if now.map(|state| state.is_terminal()).unwrap_or(false) {
            acknowledged = true;
            break;
        }
    }
    // No live driver answered (crashed / never running / paused): the OS
    // run lock is already released (it drops with the driver process), so
    // stamp the terminal state directly.
    let mode = if acknowledged {
        "live"
    } else {
        store
            .mark_cancelled(&args.run_id)
            .map_err(|e| map_store_error("failed to cancel the investigate run", e))?;
        "direct"
    };
    if output.is_json() {
        let payload = serde_json::json!({
            "schema_version": PAGE_SCHEMA_VERSION,
            "run_id": args.run_id,
            "cancelled": true,
            "mode": mode,
        });
        return emit_json_data("investigate_cancel", &payload, output);
    }
    if !output.quiet {
        match mode {
            "live" => println!(
                "investigate run {} cancelled (live driver acknowledged and cleaned up)",
                args.run_id
            ),
            _ => println!(
                "investigate run {} marked cancelled (no live driver was running)",
                args.run_id
            ),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// clean
// ---------------------------------------------------------------------------

async fn clean(args: InvestigateCleanArgs, output: &OutputConfig) -> CliResult<()> {
    let store = open_store()?;
    match (args.run.as_deref(), args.all) {
        (Some(run_id), false) => {
            // Take the run lock BEFORE any delete (P1): a live run holds
            // `.lock`, so even a run whose state.json is momentarily
            // unreadable/corrupt (e.g. mid-write) can never be deleted out
            // from under its active driver. `clean_run` itself does not
            // lock, so the CLI must.
            let lock = match store.try_lock_run(run_id) {
                Ok(lock) => lock,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(CliError::fatal(format!(
                        "investigate run '{run_id}' is being driven by another process; \
                         cancel it first with `libra investigate cancel {run_id}`"
                    )));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(run_not_found(run_id));
                }
                Err(e) => return Err(map_store_error("failed to lock the investigate run", e)),
            };
            // With the lock held there is no live driver. A readable
            // NON-terminal run is paused/resumable — refuse so its findings
            // survive. Only NOW (under the lock) is the corrupt/unreadable
            // allowance safe: such a run is exactly the clean case.
            if let Ok(Some(state)) = store.load_state(run_id)
                && !state.is_terminal()
            {
                return Err(CliError::fatal(format!(
                    "investigate run '{run_id}' has not finished (state: {}); cancel it \
                     first with `libra investigate cancel {run_id}` (a paused run is \
                     resumable with `libra investigate continue {run_id}`)",
                    terminal_label(
                        state.terminal_state,
                        state.pending_turn.as_ref().map(|p| p.reason)
                    )
                )));
            }
            let removed = store.clean_run(run_id).map_err(|e| {
                map_store_error("failed to remove the investigate run directory", e)
            })?;
            // The run directory (and its `.lock`) is gone; release the now
            // unlinked lock handle explicitly.
            drop(lock);
            if !removed {
                return Err(run_not_found(run_id));
            }
            if output.is_json() {
                let payload = serde_json::json!({
                    "schema_version": PAGE_SCHEMA_VERSION,
                    "removed": 1,
                    "skipped_running": 0,
                });
                return emit_json_data("investigate_clean", &payload, output);
            }
            if !output.quiet {
                println!("removed investigate run {run_id}");
            }
            Ok(())
        }
        (None, true) => {
            let runs = store
                .list_runs()
                .map_err(|e| CliError::fatal(format!("failed to list investigate runs: {e}")))?;
            // Only TERMINAL runs are removed, and each is locked before
            // deletion (P1): a run still holding `.lock` is never deleted.
            // Non-terminal (running OR paused/resumable) runs are skipped so
            // their findings survive. The unlocked bulk `clean_all` sweep is
            // deliberately NOT used here — a corrupt/foreign directory is
            // cleaned individually via the lock-guarded `clean --run <id>`.
            let mut removed = 0usize;
            let mut skipped: Vec<String> = Vec::new();
            for run in &runs {
                if run.terminal_state.is_none() {
                    skipped.push(run.run_id.clone());
                    continue;
                }
                match store.try_lock_run(&run.run_id) {
                    Ok(lock) => {
                        if store.clean_run(&run.run_id).map_err(|e| {
                            map_store_error("failed to remove an investigate run directory", e)
                        })? {
                            removed += 1;
                        }
                        drop(lock);
                    }
                    // A terminal run holds no driver lock; a WouldBlock means
                    // something live still owns it — skip, never delete.
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        skipped.push(run.run_id.clone());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(map_store_error("failed to lock an investigate run", e));
                    }
                }
            }
            if output.is_json() {
                let payload = serde_json::json!({
                    "schema_version": PAGE_SCHEMA_VERSION,
                    "removed": removed,
                    "skipped_running": skipped.len(),
                });
                return emit_json_data("investigate_clean", &payload, output);
            }
            if !output.quiet {
                println!(
                    "removed {removed} investigate run director{}",
                    if removed == 1 { "y" } else { "ies" }
                );
                if !skipped.is_empty() {
                    println!(
                        "skipped {} unfinished run(s): {} — cancel (or continue) them \
                         first with `libra investigate cancel <run_id>`",
                        skipped.len(),
                        skipped.join(", ")
                    );
                }
            }
            Ok(())
        }
        _ => Err(CliError::command_usage(
            "pass --run <run_id> to remove one run or --all to remove every finished run",
        )),
    }
}

// ---------------------------------------------------------------------------
// fix (unsupported: LBR-AGENT-010 until the fix bridge lands)
// ---------------------------------------------------------------------------

async fn fix(args: InvestigateFixArgs, _output: &OutputConfig) -> CliResult<()> {
    // The internal serialized fix bridge has no source anchor yet, so the
    // verb fails closed unconditionally with LBR-AGENT-010. Once the bridge
    // lands, the untrusted-seed gate (LBR-AGENT-011) fires first for any run
    // whose topic is untrusted (always) and lacks explicit approval — see
    // `untrusted_seed_for_mutation_error`.
    let _ = &args.run_id;
    Err(fix_bridge_unavailable_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn investigate_examples_cover_the_whole_command_family() {
        assert!(!INVESTIGATE_EXAMPLES.trim().is_empty());
        assert!(INVESTIGATE_EXAMPLES.starts_with("EXAMPLES:"));
        for needle in [
            "investigate start",
            "--topic",
            "--agent",
            "--max-turns",
            "--quorum",
            "investigate list",
            "investigate show",
            "investigate continue",
            "investigate cancel",
            "investigate clean --run",
            "investigate clean --all",
            "LBR-AGENT-010",
        ] {
            assert!(
                INVESTIGATE_EXAMPLES.contains(needle),
                "INVESTIGATE_EXAMPLES must mention '{needle}'"
            );
        }
    }

    #[test]
    fn fix_maps_to_lbr_agent_010_with_readonly_and_precondition_message() {
        let error = fix_bridge_unavailable_error();
        assert_eq!(
            error.stable_code(),
            StableErrorCode::AgentFixBridgeUnavailable
        );
        assert_eq!(error.stable_code().as_str(), "LBR-AGENT-010");
        let text = error.to_string();
        assert!(text.contains("fix bridge"), "{text}");
        assert!(text.contains("read-only"), "{text}");
        assert!(
            text.contains("investigate show"),
            "must point at the read-only alternative: {text}"
        );
    }

    #[test]
    fn untrusted_seed_mutation_maps_to_lbr_agent_011() {
        let error = untrusted_seed_for_mutation_error();
        assert_eq!(
            error.stable_code(),
            StableErrorCode::AgentUntrustedSeedForMutation
        );
        assert_eq!(error.stable_code().as_str(), "LBR-AGENT-011");
        let text = error.to_string();
        assert!(text.contains("untrusted"), "{text}");
        assert!(text.contains("approval"), "{text}");
        assert!(
            text.contains("investigate show"),
            "must point at the read-only alternative: {text}"
        );
    }

    #[test]
    fn investigate_cursor_round_trips_through_the_shared_keyset_helpers() {
        let cursor = InvestigateRunCursor {
            started_at: "2026-07-06T12:34:56.789012Z".to_string(),
            run_id: "0e6f0a1c-run_1".to_string(),
        };
        let token = encode_investigate_cursor(&cursor).expect("encode");
        let decoded = decode_investigate_cursor(&token).expect("decode");
        assert_eq!(decoded, cursor);
        assert!(decode_investigate_cursor("not-base64!").is_err());
    }

    #[test]
    fn terminal_label_distinguishes_terminal_paused_and_running() {
        assert_eq!(
            terminal_label(Some(InvestigateTerminalState::Quorum), None),
            "quorum"
        );
        assert_eq!(
            terminal_label(None, Some(PauseReason::Stalled)),
            "paused (stalled)"
        );
        assert_eq!(terminal_label(None, None), "running");
    }
}
