//! Top-level `libra agent` command surface.
//!
//! Every subcommand dispatches to a real handler: `status`/`list` (capability
//! matrix), `enable`/`add` and `disable`/`remove` (hook install/uninstall
//! aliases), `session`, `checkpoint`, `clean`, `doctor`, `push`, `hooks`, and
//! `import`, `graph`, and `rpc`. See `docs/development/commands/_general.md` section 9 and
//! `docs/development/tracing/agent.md` for the external-agent capture surface.

use clap::{Args, Subcommand};

use crate::{
    internal::ai::{
        capture::ingress::CaptureDeadline,
        hooks::provider::{HookProvider, ProviderInstallOptions},
        observed_agents::{AgentKind, agent_for, registration_for, supported_slugs},
    },
    utils::{
        error::{CliError, CliResult},
        output::OutputConfig,
    },
};

pub mod bridge;
mod capture_source;
pub(crate) mod checkpoint;
mod clean;
mod doctor;
pub mod mcp;
#[cfg(test)]
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use doctor::scan_checkpoint_store_with_replay_budget_for_test;
mod graph;
mod hooks;
mod import;
mod list;
pub mod mega2_stream;
mod push;
mod workspace;
// `libra review` is a TOP-LEVEL command (AG-22; `Commands::Review` in
// `src/cli.rs`); its implementation lives here so it can reuse the
// AG-20 `pub(super)` pagination helpers in `checkpoint.rs`.
pub mod review;
// `libra investigate` is a TOP-LEVEL command (AG-23; `Commands::Investigate`
// in `src/cli.rs`); it lives here for the same reason — reuse of the AG-20
// `pub(super)` pagination helpers in `checkpoint.rs`.
pub mod investigate;
mod rpc;
mod session;
mod skill;
mod status;

/// Entry for the hidden detached recovery child, recognized before normal
/// command startup. It is intentionally silent and content-free.
pub(crate) async fn run_capture_recovery_worker() -> CliResult<()> {
    let storage = crate::utils::util::try_get_storage_path(None).map_err(|_| {
        CliError::fatal("capture recovery worker could not resolve repository storage".to_string())
    })?;
    let lock_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
    let _lock = loop {
        match crate::internal::ai::capture::worker::try_lock(&storage).map_err(|_| {
            CliError::fatal("capture recovery worker lock is unavailable".to_string())
        })? {
            Some(lock) => break lock,
            None if tokio::time::Instant::now() < lock_deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            None => return Ok(()),
        }
    };
    #[cfg(debug_assertions)]
    crate::internal::ai::capture::worker::hold_for_test().await;
    doctor::run_pending_artifact_worker().await
}

#[doc(hidden)]
pub use import::{
    IMPORT_DISCOVERY_HELPER_ARG, IMPORT_DISCOVERY_HELPER_FRAME_CAP, IMPORT_INDEX_REPAIR_HELPER_ARG,
    IMPORT_INDEX_REPAIR_HELPER_FRAME_CAP, IMPORT_PREPARATION_DESCRIPTOR_CONTROL_ENV,
    IMPORT_PREPARATION_DESCRIPTOR_HELPER_ARG, IMPORT_PREPARATION_HELPER_OUTPUT_CAP,
    run_import_discovery_helper, run_import_index_repair_helper,
    run_import_preparation_descriptor_helper_from_stdin,
};

/// A0-06: derive a safe display/record name from a `review/investigate attach`
/// path. Only the basename is kept (never the full path — no directory-tree
/// leak); the basename is then redacted (a filename can embed a secret) and
/// every control character is stripped (Unix filenames can carry
/// newlines/tabs/ANSI). Used for BOTH the manifest `manual_attach` entry AND
/// any user-facing read error, so a hostile path never reaches output
/// unsanitized.
pub(crate) fn sanitize_attachment_name(path: &std::path::Path) -> String {
    let raw = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".to_string());
    let (redacted, _) = crate::internal::ai::review::redact_untrusted(raw.as_bytes());
    redacted.chars().filter(|c| !c.is_control()).collect()
}

/// `--help` examples shown in `libra agent --help` output.
///
/// `agent` is the operator surface for the external Agent capture
/// pipeline. It exposes the visible status, list, import, enable, disable,
/// session, checkpoint, graph, clean, doctor, push, skill, and rpc commands plus a
/// hidden `hooks` entry point invoked by installed provider hooks.
/// The banner pins the canonical invocation per sub-command plus the
/// `--all` clean form, a named `--remote` push, and a JSON variant
/// for agents so users see all supported forms without reading the
/// design doc. Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
pub const AGENT_EXAMPLES: &str = "\
EXAMPLES:
    libra agent status                              Show captured-session counts and recent checkpoint summary
    libra agent list                                Show the agent capability matrix (supported roster, hooks, install state)
    libra agent list --json                         Capability matrix as JSON (stable schema for automation)
    libra agent list --schema-version 2 --json      Capability matrix with transcript import/export methods
    libra agent import --session <id> --agent claude-code --yes  Import one historical transcript after consent
    libra --json agent graph <session>             Capture graph as frozen JSON schema v1
    libra --machine agent graph <session>          Capture graph as compact machine-readable JSON
    libra agent add claude-code                     Enable Claude Code capture and install its hooks (alias of enable)
    libra agent add                                 Enable every supported agent
    libra agent remove claude-code                  Disable Claude Code capture and uninstall its hooks (alias of disable)
    libra agent enable --agent claude               Enable Claude Code capture and install its hooks
    libra agent enable --agent claude --hook-timeout 60  Install Claude Code hooks with a 60-second host timeout
    libra agent disable --agent claude              Disable Claude Code capture and uninstall its hooks
    libra agent session list                        List captured sessions
    libra agent checkpoint list                     List captured checkpoints
    libra agent checkpoint show <id>                Show a checkpoint's safe structural summary
    libra agent checkpoint rewind <id>              Preview/apply checkpoint rewind
    libra agent checkpoint export <id>              Export the redacted transcript (no authorization needed)
    libra agent checkpoint export <id> --allow-raw --raw  Export the raw transcript (audited; requires --allow-raw)
    libra agent skill search --skill /review        Search captured skill events (by skill/provider/session/time)
    libra agent skill registry                      Show the curated per-agent discoverable-skill registry
    libra agent clean                               Drop temporary checkpoints from the most recent stopped session
    libra agent clean --all                         Drop temporary checkpoints from every stopped session
    libra agent doctor                              Diagnose hook installation and capture state
    libra agent workspace list                      List workspace records (keyset pagination)
    libra agent workspace show <workspace-id>       Show one workspace record (lease, owner, path, state)
    libra agent push                                Push refs/libra/traces to the default remote
    libra agent push --remote origin                Push refs/libra/traces to a named remote
    libra agent rpc list                            Discover libra-agent-<name> RPC binaries on PATH
    libra agent rpc trust <slug>                    Trust a discovered binary (records sha256/inode provenance)
    libra agent rpc trust --dir <path>              Register a trusted directory (binaries must live under one)
    libra agent rpc untrust <slug>                  Revoke trust (binary returns to quarantine)
    libra agent rpc invoke <slug> <method>          Invoke a single JSON-RPC method (use --params '<json>' for arguments)
    libra agent push --mega2 --remote origin        Push refs/libra/traces to the scoped Mega2 TraceRepo
    libra agent bridge --stdio                      Run the DeepSeek Harness bridge over stdio (JSON-RPC 2.0 NDJSON)
    libra agent mcp --stdio                         Serve repository MCP tools over stdio (JSON-RPC 2.0 NDJSON)
    libra --json agent status                       Structured JSON output for agents";

#[derive(Args, Debug)]
#[command(after_help = AGENT_EXAMPLES)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentSubcommand {
    /// Show captured-session counts and recent checkpoint summary.
    #[command(about = "Report captured external-agent session status")]
    Status(status::StatusArgs),

    /// List known agents with their capability matrix (AG-17).
    #[command(about = "List agents with their capability matrix")]
    List(list::ListArgs),

    /// Import historical external-agent transcripts after explicit consent.
    #[command(about = "Import historical external-agent transcripts")]
    Import(import::ImportArgs),

    /// Inspect the read-only turn/revision graph for a captured session
    /// (`--json` / `--machine` output; the interactive TUI entry was removed
    /// in the W5 breaking release).
    #[command(about = "Inspect a captured agent session graph (JSON/machine output)")]
    Graph(graph::GraphArgs),

    /// Enable an external Agent and install its hooks.
    #[command(about = "Enable an external agent and install its hooks")]
    Enable(EnableArgs),

    /// Alias of `enable`: `add <name>` ≡ `enable --agent <name>`; no name
    /// means every supported agent.
    #[command(about = "Enable an external agent and install its hooks (alias of enable)")]
    Add(AddRemoveArgs),

    /// Disable an external Agent and uninstall its hooks.
    #[command(about = "Disable an external agent and uninstall its hooks")]
    Disable(DisableArgs),

    /// Alias of `disable`: `remove <name>` ≡ `disable --agent <name>`; no
    /// name means every supported agent.
    #[command(about = "Disable an external agent and uninstall its hooks (alias of disable)")]
    Remove(AddRemoveArgs),

    /// Inspect captured sessions.
    #[command(subcommand, about = "Inspect captured sessions")]
    Session(session::SessionSubcommand),

    /// Inspect captured checkpoints.
    #[command(subcommand, about = "Inspect captured checkpoints")]
    Checkpoint(CheckpointSubcommand),

    /// Discover and search captured skill events.
    #[command(subcommand, about = "Discover and search captured skill events")]
    Skill(skill::SkillSubcommand),

    /// Remove temporary checkpoints from stopped sessions.
    #[command(about = "Clean up temporary checkpoints from stopped sessions")]
    Clean(CleanArgs),

    /// Diagnose hook installation, stuck sessions, and orphan checkpoints.
    #[command(about = "Diagnose hook installation and capture state")]
    Doctor(DoctorArgs),

    /// Push `refs/libra/traces` to a remote.
    #[command(about = "Push refs/libra/traces to a remote")]
    Push(PushArgs),

    /// Inspect the workspace registry (§C.8, read-only).
    #[command(subcommand, about = "Inspect workspace records (read-only)")]
    Workspace(workspace::WorkspaceSubcommand),

    /// Internal hook entry point (called by hook configs installed by `enable`).
    #[command(about = "Hook entry point", hide = true)]
    Hooks(AgentHooksArgs),

    /// Discover and invoke external `libra-agent-<name>` RPC binaries.
    /// Phase 4.5 (entire.md §14.4 item 5).
    #[command(subcommand, about = "External libra-agent-<name> RPC")]
    Rpc(rpc::AgentRpcSubcommand),

    /// Repository-scoped JSON-RPC 2.0 NDJSON ingress for the DeepSeek
    /// Harness plugin (plan-20260818 LB-01). This is the ONLY standard
    /// inbound write transport for Harness; it is not `libra code --control`
    /// and is a plain JSON-RPC 2.0 NDJSON transport.
    #[command(about = "Run the DeepSeek Harness bridge over stdio (JSON-RPC 2.0 NDJSON)")]
    Bridge(bridge::BridgeArgs),

    /// Repository MCP server (legacy stdio). Not the DeepSeek Harness bridge.
    #[command(about = "Serve repository tools over the Model Context Protocol")]
    Mcp(mcp::McpArgs),
}

/// Arguments scoped exclusively to the hidden legacy hook alias.
///
/// Clap requires a subcommand-bearing `Subcommand` enum variant to be a
/// newtype. Keeping the budget and nested provider command in this `Args`
/// wrapper prevents `--capture-budget-ms` from becoming an `agent`-global
/// option while preserving the installed legacy grammar.
#[derive(Args, Debug)]
pub struct AgentHooksArgs {
    /// Installer-owned deadline for this legacy hook alias. It is scoped
    /// to hook entrypoints so unrelated `agent` subcommands cannot accept
    /// or silently ignore it.
    #[arg(
            long,
            hide = true,
            value_name = "MILLISECONDS",
            value_parser = crate::command::hooks::parse_capture_budget_ms
        )]
    pub capture_budget_ms: Option<u64>,
    #[command(subcommand)]
    pub command: hooks::AgentHooksSubcommand,
}

#[derive(Args, Debug)]
pub struct EnableArgs {
    /// One or more agent names. Empty means "every supported agent".
    #[arg(long = "agent", value_name = "NAME")]
    pub agents: Vec<String>,

    /// Host handler timeout in whole seconds for the installed Claude Code
    /// and Codex hooks (default: 30). The hidden capture budget is derived
    /// from it, keeping up to one second for cleanup. Claude `SessionEnd`
    /// keeps no timeout and Codex `SessionEnd` / `Interrupt` stay capped at
    /// 3 seconds. Every selected agent must accept a host timeout. Re-running
    /// enable without this flag restores the default.
    #[arg(
        long = "hook-timeout",
        value_name = "SECONDS",
        value_parser = parse_hook_timeout_secs
    )]
    pub hook_timeout: Option<u64>,
}

/// Parse `--hook-timeout`: a positive whole number of seconds whose derived
/// capture budget is representable, checked before any agent is installed.
fn parse_hook_timeout_secs(raw: &str) -> Result<u64, String> {
    let secs = raw
        .parse::<u64>()
        .map_err(|_| format!("'{raw}' is not a whole number of seconds"))?;
    crate::internal::ai::hooks::provider::capture_budget_millis_for_host_timeout(secs)
        .map_err(|err| err.to_string())?;
    Ok(secs)
}

#[derive(Args, Debug)]
pub struct DisableArgs {
    /// One or more agent names to disable. Empty means "every supported agent".
    #[arg(long = "agent", value_name = "NAME")]
    pub agents: Vec<String>,
}

/// Positional-name form shared by the `add` / `remove` aliases
/// (`libra agent add claude-code` ≡ `libra agent enable --agent claude-code`).
#[derive(Args, Debug)]
pub struct AddRemoveArgs {
    /// Agent names. Empty means "every supported agent".
    #[arg(value_name = "NAME")]
    pub agents: Vec<String>,
}

#[derive(Args, Debug)]
pub struct CleanArgs {
    /// Drop temporary checkpoints from every stopped session, not just the
    /// most recent.
    #[arg(long)]
    pub all: bool,

    /// Retention GC (AG-24a): drop checkpoints from stopped sessions older
    /// than `agent.retention.transcript_days` (default 90), regardless of
    /// scope. Never touches the append-only `agent_audit_log`. Mutually
    /// informative with `--all` (GC always spans every stopped session).
    #[arg(long)]
    pub gc: bool,

    /// Override the transcript retention window (days) for this `--gc` run
    /// instead of reading `agent.retention.transcript_days`.
    #[arg(long, value_name = "DAYS", requires = "gc")]
    pub retention_days: Option<u32>,

    /// Preview only: report what a `--gc` sweep *would* remove (checkpoints,
    /// stderr logs, findings runs) without deleting anything.
    #[arg(long, requires = "gc")]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// AG-20: repair detected checkpoint-store inconsistencies (rebuild
    /// missing catalog rows from `refs/libra/traces`, re-enqueue missing
    /// `object_index` rows). Detection-only without this flag; rows whose
    /// objects are unrecoverable are reported for manual action.
    #[arg(long)]
    pub repair: bool,
    /// Page of at most 100 reachable commits to inspect for local agent
    /// commit-link evidence. Repeat `--repair` to drain each 64-link batch.
    #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub commit_link_page: u16,
    /// Continue the bounded Codex legacy-revision scan after this checkpoint.
    /// Doctor reports the next cursor when more rows remain.
    #[arg(long, value_name = "CHECKPOINT_ID", value_parser = parse_codex_legacy_cursor)]
    pub codex_legacy_after: Option<String>,
    /// Explicitly discard a corrupt or unrecoverably full local commit-link
    /// journal after inspection. Checkpoint data and commits are untouched.
    #[arg(long, requires = "repair")]
    pub reset_commit_link_journal: bool,
    /// Acknowledge discarding still-recoverable links outside the inspected
    /// page or worktree when resetting a readable, full journal.
    #[arg(long, requires = "reset_commit_link_journal")]
    pub acknowledge_commit_link_loss: bool,
    /// Reconcile recent Codex sessions using an explicit, closed and consistent
    /// SQLite backup. Never opens a live provider database or imports data.
    #[arg(long, value_name = "PATH", conflicts_with = "repair")]
    pub codex_index_backup: Option<std::path::PathBuf>,

    /// Page of at most 1000 scope-filtered candidates from the provided backup.
    #[arg(long, value_name = "N", requires = "codex_index_backup", value_parser = clap::value_parser!(u16).range(1..=10))]
    pub codex_index_page: Option<u16>,
}

fn parse_codex_legacy_cursor(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err("expected a checkpoint ID containing only hex digits and hyphens (at most 128 characters)".to_string());
    }
    Ok(value.to_string())
}

#[derive(Args, Debug)]
pub struct PushArgs {
    /// Remote name to push refs/libra/traces to (default: origin)
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,
    /// AG-20: allow the non-fast-forward push that follows a local
    /// `libra agent clean` prune (the traces ref is Libra-managed and
    /// rewritten as a whole chain). Uses force-with-lease semantics
    /// against the remote-tracking ref — never an unconditional force —
    /// so a rewrite from another machine still fails closed.
    /// Mega2 mode rejects this flag before any network attempt.
    #[arg(long)]
    pub force_rewrite: bool,
    /// Push to the scoped Mega2 TraceRepo instead of the ordinary code remote.
    #[arg(long)]
    pub mega2: bool,
    /// Open a new traces epoch. Required only when replacing one that already exists.
    #[arg(long)]
    pub new_stream: bool,
}

#[derive(Subcommand, Debug)]
pub enum CheckpointSubcommand {
    /// List captured checkpoints, newest first.
    #[command(about = "List captured checkpoints")]
    List(CheckpointListArgs),
    /// Show a checkpoint's safe structural summary.
    #[command(about = "Show a safe checkpoint summary")]
    Show(CheckpointShowArgs),
    /// Inspect what `rewind` would do (`--apply` to actually run). Apply
    /// restores the working tree; provider transcripts remain unchanged.
    #[command(
        about = "Rewind a checkpoint (dry-run by default; --apply restores the worktree only)"
    )]
    Rewind(CheckpointRewindArgs),
    /// Export a checkpoint's transcript. Redacted by default; raw
    /// (un-redacted) export requires `--allow-raw` and is audited (AG-24a).
    #[command(about = "Export a checkpoint transcript (raw export requires --allow-raw; audited)")]
    Export(CheckpointExportArgs),
}

#[derive(Args, Debug)]
pub struct CheckpointListArgs {
    /// Filter checkpoints to those belonging to a single session id
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
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
pub struct CheckpointShowArgs {
    /// Checkpoint identifier returned by `libra agent checkpoint list`
    #[arg(value_name = "CHECKPOINT_ID")]
    pub checkpoint_id: String,
}

/// `libra agent checkpoint export <id>` — export a checkpoint's stored
/// transcript. Redacted output is the default and requires no special
/// authorization; RAW (un-redacted) export requires `--allow-raw` and
/// writes one append-only `agent_audit_log` row per access (AG-24a).
#[derive(Args, Debug)]
pub struct CheckpointExportArgs {
    /// Checkpoint identifier returned by `libra agent checkpoint list`
    #[arg(value_name = "CHECKPOINT_ID")]
    pub checkpoint_id: String,

    /// Authorize a RAW (un-redacted) export. Without it, the redacted
    /// transcript is exported and no audit row is required. A raw export
    /// without this flag is refused fail-closed (`LBR-AGENT-013`) and the
    /// refusal is itself audited.
    #[arg(long)]
    pub allow_raw: bool,

    /// Request the raw (un-redacted) transcript. Only honored together
    /// with `--allow-raw`; on its own it triggers the fail-closed refusal.
    #[arg(long)]
    pub raw: bool,

    /// Operator justification recorded in the audit row (who/why).
    #[arg(long, value_name = "TEXT")]
    pub justification: Option<String>,

    /// Export this opaque artifact instead of the transcript. Requires
    /// --allow-raw, a nonempty --justification and -o; never writes bytes to
    /// stdout and never replaces an existing destination.
    #[arg(long, value_name = "SHA256")]
    pub reasoning_artifact: Option<String>,

    /// Write the export to this file instead of stdout.
    #[arg(long, short = 'o', value_name = "PATH")]
    pub output_path: Option<String>,
}

#[derive(Args, Debug)]
pub struct CheckpointRewindArgs {
    /// Checkpoint identifier to rewind to (from `libra agent checkpoint list`)
    #[arg(value_name = "CHECKPOINT_ID")]
    pub checkpoint_id: String,
    /// Show the impact without modifying anything (default)
    #[arg(long, conflicts_with = "apply")]
    pub dry_run: bool,
    /// Actually restore the working tree; provider transcripts remain unchanged
    #[arg(long)]
    pub apply: bool,
}

/// Run an `agent` subcommand. Every variant routes to its implemented
/// handler; unsupported *inputs* (e.g. a non-first-batch agent slug) still
/// return an actionable error rather than panicking.
pub async fn execute_safe(
    args: AgentArgs,
    output: &OutputConfig,
    hook_deadline: Option<CaptureDeadline>,
) -> CliResult<()> {
    let AgentArgs { command } = args;
    match command {
        AgentSubcommand::Status(args) => status::execute_safe(args, output).await,
        AgentSubcommand::List(args) => list::execute_safe(args, output).await,
        AgentSubcommand::Import(args) => import::execute_safe(args, output).await,
        AgentSubcommand::Graph(args) => graph::execute_safe(args, output).await,
        AgentSubcommand::Enable(args) => enable_agents(&args.agents, args.hook_timeout, output),
        AgentSubcommand::Add(args) => enable_agents(&args.agents, None, output),
        AgentSubcommand::Disable(args) => disable_agents(&args.agents, output),
        AgentSubcommand::Remove(args) => disable_agents(&args.agents, output),
        AgentSubcommand::Session(cmd) => session::execute_safe(cmd, output).await,
        AgentSubcommand::Checkpoint(cmd) => checkpoint::execute_safe(cmd, output).await,
        AgentSubcommand::Skill(cmd) => skill::execute_safe(cmd, output).await,
        AgentSubcommand::Clean(cmd) => clean::execute_safe(cmd, output).await,
        AgentSubcommand::Doctor(cmd) => doctor::execute_safe(cmd, output).await,
        AgentSubcommand::Push(cmd) => push::execute_safe(cmd, output).await,
        AgentSubcommand::Workspace(cmd) => workspace::execute_safe(cmd, output).await,
        AgentSubcommand::Hooks(args) => {
            hooks::execute_safe(args.command, output, hook_deadline).await
        }
        AgentSubcommand::Rpc(cmd) => rpc::execute_safe(cmd, output).await,
        AgentSubcommand::Bridge(args) => bridge::execute_safe(args, output).await,
        AgentSubcommand::Mcp(args) => mcp::execute_safe(args, output).await,
    }
}

fn enable_agents(
    agents: &[String],
    hook_timeout: Option<u64>,
    output: &OutputConfig,
) -> CliResult<()> {
    install_or_uninstall(agents, output, true, hook_timeout)
}

pub(super) const CIPHERTEXT_ARCHIVE_DISCLOSURE: &str = "Provider-encrypted reasoning original bytes may be retained as ordinary transcript data without decryption and mirrored to cloud storage with their checkpoint; session erasure hides the references and local GC reclaims unreachable objects. Native artifacts still require source proof; unproven fields remain ordinary transcript data.";

fn disable_agents(agents: &[String], output: &OutputConfig) -> CliResult<()> {
    install_or_uninstall(agents, output, false, None)
}

fn install_or_uninstall(
    agents: &[String],
    output: &OutputConfig,
    install: bool,
    hook_timeout: Option<u64>,
) -> CliResult<()> {
    let verb_present = if install { "enable" } else { "disable" };

    let mut resolved = resolve_agent_kinds(agents)?;
    if resolved.is_empty() {
        return Err(CliError::fatal(format!(
            "no installable agents to {verb_present}"
        )));
    }

    // Fail-closed before any side effect: anything outside the supported
    // roster is an actionable error (so an `add a b` batch never
    // half-installs). The one exception is `remove gemini` — the
    // uninstall-only channel for the demoted agent (E9 / AG-17).
    for kind in &resolved {
        let row = registration_for(*kind);
        if row.supported {
            continue;
        }
        if install {
            return Err(unsupported_for_install(row.slug));
        }
        if *kind != AgentKind::Gemini {
            return Err(CliError::fatal(format!(
                "agent '{}' is not in the supported roster ({}) and has no hooks to remove; \
                 only gemini retains an uninstall-only channel",
                row.slug,
                supported_slugs().join(", ")
            )));
        }
    }
    if install && resolved.contains(&AgentKind::Hermes) {
        match crate::internal::ai::hooks::providers::hermes::settings::hermes_enable_gate(
            agents.is_empty(),
            crate::internal::ai::hooks::providers::hermes::settings::hermes_runtime_is_supported(),
        ) {
            crate::internal::ai::hooks::providers::hermes::settings::HermesEnableGate::Install => {}
            crate::internal::ai::hooks::providers::hermes::settings::HermesEnableGate::SkipBare => {
                resolved.retain(|kind| *kind != AgentKind::Hermes);
                if !output.quiet {
                    eprintln!(
                        "libra agent enable: skipping 'hermes' (Hermes v0.22.5 bf0203a was not found on PATH; no Hermes files were changed)"
                    );
                }
            }
            crate::internal::ai::hooks::providers::hermes::settings::HermesEnableGate::RefuseExplicit => {
                return Err(CliError::fatal(
                    "agent 'hermes' needs Hermes v0.22.5 (bf0203a) on PATH before capture can be enabled; no files were changed",
                ));
            }
        }
    }
    // Fail closed before any side effect, like the roster check above: a
    // provider that takes no host timeout would otherwise reject the option
    // mid-batch, after earlier agents were already installed.
    if let Some(secs) = hook_timeout
        && let Some(slug) = resolved.iter().find_map(|kind| {
            let row = registration_for(*kind);
            (row.hook_installable && !accepts_hook_timeout(*kind)).then_some(row.slug)
        })
    {
        return Err(hook_timeout_unsupported(slug, secs));
    }
    if install && !output.quiet {
        eprintln!("{CIPHERTEXT_ARCHIVE_DISCLOSURE}");
    }

    for kind in resolved {
        let row = registration_for(kind);
        let slug = row.slug;
        if install {
            if !row.hook_installable {
                // Supported (first-batch) but its HookProvider has not
                // landed yet — transcript capture still works, so this is
                // an informational skip, not an error (AG-19 wires it).
                if !output.quiet {
                    eprintln!(
                        "libra agent {verb_present}: skipping '{slug}' \
                         (HookProvider not landed yet — transcript-readable only; \
                         hook install arrives with AG-19)"
                    );
                }
                continue;
            }
            let provider = agent_for(kind).as_hooks().ok_or_else(|| {
                CliError::fatal(format!(
                    "internal error: '{slug}' is marked hook-installable but exposes no \
                     HookProvider; run libra agent doctor and report this"
                ))
            })?;
            install_provider_hooks(provider, hook_timeout)
                .map_err(|err| CliError::fatal(format!("failed to enable '{slug}': {err}")))?;
            if !output.quiet {
                println!("libra agent enable: enabled '{slug}' (provider hooks installed)");
            }
        } else {
            // Uninstall side: supported agents plus the uninstall-only
            // channel for formerly-supported agents (gemini). Agents that
            // never had a HookProvider have nothing to remove — that is a
            // no-op, not an error, so repeated removes stay idempotent.
            //
            // AG-19: dispatch is AgentKind -> registry adapter ->
            // `as_hooks()`. Gemini's provider is deliberately NOT exposed
            // through `as_hooks()` (E9 forbids advertising its
            // capabilities), so its uninstall-only channel references the
            // typed singleton directly — no name-string registry.
            let provider = agent_for(kind).as_hooks().or_else(|| {
                (kind == AgentKind::Gemini)
                    .then(crate::internal::ai::hooks::providers::gemini_provider)
            });
            let Some(provider) = provider else {
                if !output.quiet {
                    println!(
                        "libra agent disable: '{slug}' has no hook provider; nothing to remove"
                    );
                }
                continue;
            };
            // A stale managed command is intentionally not "installed" in
            // the status sense (for example, it lacks a newly required
            // capture-budget argument), but it is still ours to remove.
            // Only a successful `false` result proves that stale-cleanup
            // case. An inspection error (such as malformed Codex trust
            // state) must stop before uninstall: some providers update more
            // than one settings file, and proceeding could leave a partial
            // mutation behind.
            let installed = provider.hooks_are_installed().map_err(|err| {
                CliError::fatal(format!(
                    "failed to inspect '{slug}' hook installation state before disabling: {err}"
                ))
            })?;
            provider
                .uninstall_hooks()
                .map_err(|err| CliError::fatal(format!("failed to disable '{slug}': {err}")))?;
            if !output.quiet {
                if installed {
                    println!("libra agent disable: disabled '{slug}' (provider hooks removed)");
                } else {
                    println!(
                        "libra agent disable: '{slug}' hooks not installed or stale; removed any managed entries"
                    );
                }
            }
        }
    }
    Ok(())
}

/// Actionable unsupported-roster error for `enable`/`add` (E9). The gemini
/// wording points at its uninstall-only channel.
fn unsupported_for_install(slug: &str) -> CliError {
    let roster = supported_slugs().join(", ");
    if slug == "gemini" {
        CliError::fatal(format!(
            "agent 'gemini' is no longer in the supported roster ({roster}); \
             it is uninstall-only — existing hooks can be removed with \
             'libra agent remove gemini', and previously captured sessions stay readable"
        ))
    } else {
        CliError::fatal(format!(
            "agent '{slug}' is not in the supported roster ({roster}); \
             it cannot be enabled or install hooks"
        ))
    }
}

/// Whether `kind`'s hook provider writes `--hook-timeout` as its host
/// handler timeout. Resolved through the registry, never by slug.
fn accepts_hook_timeout(kind: AgentKind) -> bool {
    agent_for(kind)
        .as_hooks()
        .is_some_and(|provider| provider.supports_host_timeout())
}

/// Usage error for `--hook-timeout` combined with an agent whose hooks take
/// no host timeout. Raised before any agent is installed.
fn hook_timeout_unsupported(slug: &str, secs: u64) -> CliError {
    let accepted = supported_slugs()
        .into_iter()
        .filter(|candidate| AgentKind::from_cli_slug(candidate).is_some_and(accepts_hook_timeout))
        .collect::<Vec<_>>()
        .join(", ");
    CliError::command_usage(format!(
        "--hook-timeout {secs} cannot be applied to agent '{slug}': its hooks take no host \
         timeout (agents that accept one: {accepted}); no files were changed"
    ))
    .with_hint(format!(
        "pass --agent for each of {accepted} together with --hook-timeout, and enable other \
         agents in a separate run without it"
    ))
}

fn install_provider_hooks(
    provider: &dyn HookProvider,
    hook_timeout: Option<u64>,
) -> anyhow::Result<()> {
    // Use the running binary's path so installed hooks point at exactly the
    // libra the user is invoking — falling back to the bare `libra` symbol
    // (which `HookProvider`s will substitute) if `current_exe` fails.
    let binary_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string));
    // `None` keeps each provider's own default (Claude Code and Codex: 30s).
    let opts = ProviderInstallOptions {
        binary_path,
        timeout_secs: hook_timeout,
    };
    provider.install_hooks(&opts)
}

/// Validate `agents` and canonicalize to [`AgentKind`]s. Empty input expands
/// to the supported roster (derived from the AG-16 registry — the CLI keeps
/// no roster constant of its own); non-empty input accepts any known slug so
/// the uninstall-only channel stays reachable, with unknown slugs rejected
/// against the supported roster.
fn resolve_agent_kinds(agents: &[String]) -> CliResult<Vec<AgentKind>> {
    if agents.is_empty() {
        return Ok(supported_slugs()
            .iter()
            .filter_map(|slug| AgentKind::from_cli_slug(slug))
            .collect());
    }
    let mut out = Vec::with_capacity(agents.len());
    for slug in agents {
        let Some(kind) = AgentKind::from_cli_slug(slug) else {
            return Err(CliError::fatal(format!(
                "unknown agent '{slug}'; supported: {}",
                supported_slugs().join(", ")
            )));
        };
        out.push(kind);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::error::StableErrorCode;

    #[test]
    fn consent_text_discloses_ciphertext_archive() {
        assert_eq!(
            CIPHERTEXT_ARCHIVE_DISCLOSURE,
            "Provider-encrypted reasoning original bytes may be retained as ordinary transcript data without decryption and mirrored to cloud storage with their checkpoint; session erasure hides the references and local GC reclaims unreachable objects. Native artifacts still require source proof; unproven fields remain ordinary transcript data."
        );
        let enable = include_str!("mod.rs");
        let import = include_str!("import.rs");
        assert!(enable.contains("eprintln!(\"{CIPHERTEXT_ARCHIVE_DISCLOSURE}\")"));
        assert!(import.contains("super::CIPHERTEXT_ARCHIVE_DISCLOSURE"));
    }

    #[test]
    fn resolve_agent_kinds_expands_empty_to_supported_roster() {
        let resolved = resolve_agent_kinds(&[]).expect("empty resolves cleanly");
        assert_eq!(
            resolved,
            vec![
                AgentKind::ClaudeCode,
                AgentKind::Codex,
                AgentKind::OpenCode,
                AgentKind::Grok,
                AgentKind::Hermes,
            ]
        );
    }

    #[test]
    fn resolve_agent_kinds_passes_known() {
        let resolved =
            resolve_agent_kinds(&["claude-code".to_string()]).expect("known slug resolves");
        assert_eq!(resolved, vec![AgentKind::ClaudeCode]);
    }

    #[test]
    fn resolve_agent_kinds_accepts_uninstall_only_gemini() {
        let resolved = resolve_agent_kinds(&["gemini".to_string()])
            .expect("gemini stays resolvable for the uninstall-only channel");
        assert_eq!(resolved, vec![AgentKind::Gemini]);
    }

    #[test]
    fn resolve_agent_kinds_rejects_unknown() {
        let err = resolve_agent_kinds(&["bogus".to_string()]).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("unknown agent 'bogus'"));
        assert!(message.contains("claude-code, codex, opencode, grok"));
    }

    #[test]
    fn unsupported_for_install_points_gemini_at_uninstall_channel() {
        assert!(
            unsupported_for_install("gemini")
                .to_string()
                .contains("libra agent remove gemini")
        );
        assert!(
            unsupported_for_install("cursor")
                .to_string()
                .contains("not in the supported roster")
        );
    }

    #[test]
    fn hook_timeout_parser_accepts_positive_whole_seconds_only() {
        assert_eq!(parse_hook_timeout_secs("1"), Ok(1));
        assert_eq!(parse_hook_timeout_secs("30"), Ok(30));
        assert_eq!(parse_hook_timeout_secs("601"), Ok(601));
        for invalid in ["0", "-5", "1.5", "abc", "", "18446744073709551615"] {
            assert!(
                parse_hook_timeout_secs(invalid).is_err(),
                "'{invalid}' must be rejected"
            );
        }
    }

    /// Only providers that write a host handler timeout accept
    /// `--hook-timeout`; the rest of the roster is refused up front.
    #[test]
    fn hook_timeout_is_accepted_only_by_host_timeout_providers() {
        assert!(accepts_hook_timeout(AgentKind::ClaudeCode));
        assert!(accepts_hook_timeout(AgentKind::Codex));
        for kind in [
            AgentKind::OpenCode,
            AgentKind::Grok,
            AgentKind::Hermes,
            AgentKind::Gemini,
        ] {
            assert!(
                !accepts_hook_timeout(kind),
                "{kind:?} takes no host timeout"
            );
        }

        let err = hook_timeout_unsupported("opencode", 45);
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);
        let rendered = err.render();
        assert!(rendered.contains("--hook-timeout 45"), "{rendered}");
        assert!(rendered.contains("agent 'opencode'"), "{rendered}");
        assert!(
            rendered.contains("agents that accept one: claude-code, codex"),
            "{rendered}"
        );
        assert!(rendered.contains("no files were changed"), "{rendered}");
    }

    /// AG-19: hook dispatch goes AgentKind -> `agent_for` -> `as_hooks()`;
    /// gemini's uninstall-only channel is the sole typed exception. Pin
    /// that shape so a name-string bridge cannot quietly reappear.
    #[test]
    fn hook_dispatch_has_no_string_bridge() {
        use crate::internal::ai::observed_agents::agent_for;
        assert!(
            agent_for(AgentKind::ClaudeCode).as_hooks().is_some(),
            "claude-code dispatches through as_hooks()"
        );
        assert!(
            agent_for(AgentKind::Gemini).as_hooks().is_none(),
            "gemini stays unexposed via as_hooks() (E9); its uninstall \
             channel references the typed singleton directly"
        );
    }
}
