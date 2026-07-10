//! Top-level `libra agent` command surface.
//!
//! Every subcommand dispatches to a real handler: `status`/`list` (capability
//! matrix), `enable`/`add` and `disable`/`remove` (hook install/uninstall
//! aliases), `session`, `checkpoint`, `clean`, `doctor`, `push`, `hooks`, and
//! `rpc`. See `docs/development/commands/_general.md` section 9 and
//! `docs/development/tracing/agent.md` for the external-agent capture surface.

use clap::{Args, Subcommand};

use crate::{
    internal::ai::{
        hooks::provider::{HookProvider, ProviderInstallOptions},
        observed_agents::{AgentKind, agent_for, registration_for, supported_slugs},
    },
    utils::{
        error::{CliError, CliResult},
        output::OutputConfig,
    },
};

mod checkpoint;
mod clean;
mod doctor;
mod hooks;
mod list;
mod push;
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
/// pipeline. It exposes eight visible sub-commands (status, enable,
/// disable, session, checkpoint, clean, doctor, push, rpc) plus a
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
    libra agent add claude-code                     Enable Claude Code capture and install its hooks (alias of enable)
    libra agent add                                 Enable every supported agent
    libra agent remove claude-code                  Disable Claude Code capture and uninstall its hooks (alias of disable)
    libra agent enable --agent claude               Enable Claude Code capture and install its hooks
    libra agent disable --agent claude              Disable Claude Code capture and uninstall its hooks
    libra agent session list                        List captured sessions
    libra agent checkpoint list                     List captured checkpoints
    libra agent checkpoint show <id>                Show a single checkpoint by id
    libra agent checkpoint rewind <id>              Preview/apply checkpoint rewind
    libra agent checkpoint export <id>              Export the redacted transcript (no authorization needed)
    libra agent checkpoint export <id> --allow-raw --raw  Export the raw transcript (audited; requires --allow-raw)
    libra agent skill search --skill /review        Search captured skill events (by skill/provider/session/time)
    libra agent skill registry                      Show the curated per-agent discoverable-skill registry
    libra agent clean                               Drop temporary checkpoints from the most recent stopped session
    libra agent clean --all                         Drop temporary checkpoints from every stopped session
    libra agent doctor                              Diagnose hook installation and capture state
    libra agent push                                Push refs/libra/traces to the default remote
    libra agent push --remote origin                Push refs/libra/traces to a named remote
    libra agent rpc list                            Discover libra-agent-<name> RPC binaries on PATH
    libra agent rpc trust <slug>                    Trust a discovered binary (records sha256/inode provenance)
    libra agent rpc trust --dir <path>              Register a trusted directory (binaries must live under one)
    libra agent rpc untrust <slug>                  Revoke trust (binary returns to quarantine)
    libra agent rpc invoke <slug> <method>          Invoke a single JSON-RPC method (use --params '<json>' for arguments)
    libra agent --json status                       Structured JSON output for agents";

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

    /// Internal hook entry point (called by hook configs installed by `enable`).
    #[command(subcommand, about = "Hook entry point", hide = true)]
    Hooks(hooks::AgentHooksSubcommand),

    /// Discover and invoke external `libra-agent-<name>` RPC binaries.
    /// Phase 4.5 (entire.md §14.4 item 5).
    #[command(subcommand, about = "External libra-agent-<name> RPC")]
    Rpc(rpc::AgentRpcSubcommand),
}

#[derive(Args, Debug)]
pub struct EnableArgs {
    /// One or more agent names. Empty means "every supported agent".
    #[arg(long = "agent", value_name = "NAME")]
    pub agents: Vec<String>,
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
    #[arg(long)]
    pub force_rewrite: bool,
}

#[derive(Subcommand, Debug)]
pub enum CheckpointSubcommand {
    /// List captured checkpoints, newest first.
    #[command(about = "List captured checkpoints")]
    List(CheckpointListArgs),
    /// Show a single checkpoint's metadata and tree summary.
    #[command(about = "Show checkpoint metadata")]
    Show(CheckpointShowArgs),
    /// Inspect what `rewind` would do (`--apply` to actually run). Apply
    /// restores the working tree and truncates supported agent transcripts
    /// (currently Claude Code) when metadata includes a transcript path.
    #[command(
        about = "Rewind a checkpoint (dry-run by default; --apply restores worktree and supported transcripts)"
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
    /// Actually restore the working tree and truncate supported agent transcripts
    /// (currently Claude Code) when metadata includes a transcript path
    #[arg(long)]
    pub apply: bool,
}

/// Run an `agent` subcommand. Every variant routes to its implemented
/// handler; unsupported *inputs* (e.g. a non-first-batch agent slug) still
/// return an actionable error rather than panicking.
pub async fn execute_safe(args: AgentArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        AgentSubcommand::Status(args) => status::execute_safe(args, output).await,
        AgentSubcommand::List(args) => list::execute_safe(args, output).await,
        AgentSubcommand::Enable(args) => enable_agents(&args.agents, output),
        AgentSubcommand::Add(args) => enable_agents(&args.agents, output),
        AgentSubcommand::Disable(args) => disable_agents(&args.agents, output),
        AgentSubcommand::Remove(args) => disable_agents(&args.agents, output),
        AgentSubcommand::Session(cmd) => session::execute_safe(cmd, output).await,
        AgentSubcommand::Checkpoint(cmd) => checkpoint::execute_safe(cmd, output).await,
        AgentSubcommand::Skill(cmd) => skill::execute_safe(cmd, output).await,
        AgentSubcommand::Clean(cmd) => clean::execute_safe(cmd, output).await,
        AgentSubcommand::Doctor(cmd) => doctor::execute_safe(cmd, output).await,
        AgentSubcommand::Push(cmd) => push::execute_safe(cmd, output).await,
        AgentSubcommand::Hooks(cmd) => hooks::execute_safe(cmd, output).await,
        AgentSubcommand::Rpc(cmd) => rpc::execute_safe(cmd, output).await,
    }
}

fn enable_agents(agents: &[String], output: &OutputConfig) -> CliResult<()> {
    install_or_uninstall(agents, output, true)
}

fn disable_agents(agents: &[String], output: &OutputConfig) -> CliResult<()> {
    install_or_uninstall(agents, output, false)
}

fn install_or_uninstall(agents: &[String], output: &OutputConfig, install: bool) -> CliResult<()> {
    let verb_present = if install { "enable" } else { "disable" };

    let resolved = resolve_agent_kinds(agents)?;
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
            install_provider_hooks(provider)
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
            let installed = provider.hooks_are_installed().map_err(|err| {
                CliError::fatal(format!(
                    "failed to inspect '{slug}' hook installation state: {err}"
                ))
            })?;
            if !installed {
                if !output.quiet {
                    println!("libra agent disable: '{slug}' hooks not installed; nothing to do");
                }
                continue;
            }
            provider
                .uninstall_hooks()
                .map_err(|err| CliError::fatal(format!("failed to disable '{slug}': {err}")))?;
            if !output.quiet {
                println!("libra agent disable: disabled '{slug}' (provider hooks removed)");
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

fn install_provider_hooks(provider: &dyn HookProvider) -> anyhow::Result<()> {
    // Use the running binary's path so installed hooks point at exactly the
    // libra the user is invoking — falling back to the bare `libra` symbol
    // (which `HookProvider`s will substitute) if `current_exe` fails.
    let binary_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string));
    let opts = ProviderInstallOptions {
        binary_path,
        timeout_secs: None,
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

/// Reserved refuse helper for future agent subcommands that need an explicit
/// non-zero-exit refuse path. Currently unused (all subcommands are
/// implemented); kept as a small seam rather than re-added ad hoc later.
#[allow(dead_code)]
fn refuse(message: &str) -> CliResult<()> {
    Err(CliError::fatal(message.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_agent_kinds_expands_empty_to_supported_roster() {
        let resolved = resolve_agent_kinds(&[]).expect("empty resolves cleanly");
        assert_eq!(
            resolved,
            vec![AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::OpenCode]
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
        assert!(message.contains("claude-code, codex, opencode"));
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
