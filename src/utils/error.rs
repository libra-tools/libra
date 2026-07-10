//! User-facing CLI error rendering utilities.
//!
//! The CLI uses [`CliError`] as the single user-visible error type at the
//! process boundary. Domain errors inside commands should be mapped into
//! [`CliError`] with an explicit stable code, exit code, and hint set instead
//! of printing raw internal causes to stderr.
//!
//! Contract references for maintainers and AI agents:
//! - Stable rendering and JSON envelope: `docs/development/cli-error-contract-design.md`
//! - Public stable-code catalogue: `docs/error-codes.md`
//!
//! ## Anatomy of a CliError
//!
//! Each error carries five pieces of metadata that drive rendering:
//! - **kind** ([`CliErrorKind`]) — chooses the prefix (`fatal:`, `error:`, none).
//! - **stable_code** ([`StableErrorCode`]) — machine-readable identifier (`LBR-...`)
//!   that maps to a [`CliErrorCategory`] and a default exit code.
//! - **message** — the primary human-readable line.
//! - **hints** — at most two trailing `Hint:`-prefixed lines.
//! - **usage** / **details** — optional usage block and structured key/value details.
//!
//! ## Rendering modes
//!
//! - `render` — single-line prefixed output for human consumption.
//! - `render_json` — structured envelope used in `--json` mode and when
//!   `LIBRA_ERROR_JSON=1` forces structured stderr.
//! - `render_report` — combines both, emitting the human line and a trailing JSON
//!   payload so log scrapers can parse the same line a developer is reading.
//!
//! ## Exit-code resolution
//!
//! By default the exit code follows the Git convention: 128 for fatal runtime
//! errors and 129 for usage errors. Setting `LIBRA_FINE_EXIT_CODES=1` switches to
//! the legacy 2..=9 category codes for backward compatibility with old scripts.
//! `with_exit_code` overrides everything else, used when matching Git's quirky
//! per-command codes (e.g. `git config --get` returning 1).

use std::{
    collections::BTreeMap,
    env, fmt,
    io::{self, IsTerminal, Write},
};

use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::utils::output::{JsonFormat, OutputConfig, record_warning};

/// Shared CLI result type used by every command handler and dispatcher.
pub type CliResult<T = ()> = Result<T, CliError>;

/// Env var that forces structured (JSON) error output on stderr regardless of
/// whether stderr is a TTY. Recognised values: `1`, `true`, `yes`, `on`, `always`.
pub const LIBRA_ERROR_JSON_ENV: &str = "LIBRA_ERROR_JSON";
/// Env var that switches exit codes from the Git-style 128/129 to the legacy
/// fine-grained 2..=9 category codes. Recognised values: `1`, `true`, `yes`, `on`.
pub const LIBRA_FINE_EXIT_CODES_ENV: &str = "LIBRA_FINE_EXIT_CODES";
/// Canonical issue tracker URL shown for unexpected internal failures.
pub const LIBRA_ISSUES_URL: &str = "https://github.com/libra-tools/libra/issues";
/// Human-facing hint appended to internal-invariant errors.
pub const INTERNAL_ERROR_REPORT_HINT: &str =
    "please report this issue at: https://github.com/libra-tools/libra/issues";

/// Returns `true` when `LIBRA_FINE_EXIT_CODES=1` is set, enabling backward-
/// compatible category-specific exit codes (2–9) instead of the default
/// Git-standard 128/129.
///
/// Boundary conditions:
/// - Only recognises a small allowlist of truthy strings; an unknown value (e.g.
///   `LIBRA_FINE_EXIT_CODES=yesplease`) leaves the flag off rather than guessing.
fn fine_exit_codes_enabled() -> bool {
    matches!(
        env::var(LIBRA_FINE_EXIT_CODES_ENV).as_deref(),
        Ok("1" | "true" | "yes" | "on")
    )
}

/// High-level CLI error classes used to decide prefixes and parse semantics.
///
/// Variants do not encode severity directly — see [`ErrorLevel`] for that — but
/// they do drive how `render` chooses the leading prefix string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliErrorKind {
    /// `libra foo` where `foo` is not a known subcommand. Renders without prefix.
    UnknownCommand,
    /// Runtime failure whose Git-compatible human output intentionally renders
    /// without an `error:` / `fatal:` prefix.
    UnprefixedFailure,
    /// Top-level argv parse failure (clap-detected). Renders with `error:` prefix.
    ParseUsage,
    /// Subcommand-specific usage failure (e.g. mutually exclusive flags).
    CommandUsage,
    /// Hard runtime error: rendered with `fatal:` prefix.
    Fatal,
    /// Soft runtime error: rendered with `error:` prefix.
    Failure,
}

/// Prefix level used for rendered messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorLevel {
    /// Maps to `fatal:` prefix in human output.
    Fatal,
    /// Maps to `error:` prefix in human output.
    Error,
}

/// Coarse process exit codes for shell and CI automation.
///
/// Values follow the Git convention: **128** for fatal runtime errors and
/// **129** for usage / invalid-argument errors, so existing scripts that
/// branch on Git's exit codes work unchanged with Libra.
///
/// The finer-grained failure category is still available through the
/// [`StableErrorCode`] carried in the structured JSON report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[repr(i32)]
pub enum CliExitCode {
    /// Command succeeded but emitted warnings (`--exit-code-on-warning`).
    Warning = 9,
    /// Fatal runtime error (repo, conflict, network, auth, I/O, internal).
    Fatal = 128,
    /// CLI usage or invalid-target error.
    Usage = 129,
}

impl CliExitCode {
    /// Convert to the platform-native `i32` exit code accepted by `process::exit`.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

/// Backward-compatible alias kept for older call sites.
pub type ExitCode = CliExitCode;

/// Stable error categories for machine classification.
///
/// Each `StableErrorCode` belongs to exactly one category; agents and CI scripts
/// can use the category to make broad routing decisions without tracking every
/// individual code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliErrorCategory {
    Cli,
    Repo,
    Config,
    Conflict,
    Network,
    Auth,
    Io,
    Internal,
    /// Command succeeded but emitted warnings; used with `--exit-code-on-warning`.
    Warning,
}

impl CliErrorCategory {
    /// Stable lowercase string used in JSON error envelopes (e.g. `"repo"`,
    /// `"network"`). Keep in sync with the `serde(rename_all = "snake_case")`
    /// attribute so manual string matching does not drift from the JSON output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Repo => "repo",
            Self::Config => "config",
            Self::Conflict => "conflict",
            Self::Network => "network",
            Self::Auth => "auth",
            Self::Io => "io",
            Self::Internal => "internal",
            Self::Warning => "warning",
        }
    }
}

/// Stable Libra CLI error codes for agents and structured tooling.
///
/// Each variant maps to a fixed `LBR-*` string ID rendered in JSON output and
/// `Error-Code:` headers. Adding a new code requires:
/// 1. Adding the variant here and a unique `LBR-*` ID in `as_str`.
/// 2. Adding the variant -> `CliErrorCategory` mapping in `category`.
/// 3. Adding a human-readable description in `description` (rendered by
///    `libra help error-codes`).
/// 4. Updating `docs/error-codes.md` and checking
///    `docs/development/cli-error-contract-design.md` for contract impact.
///
/// Removing or renaming an existing code is a breaking change for downstream
/// agents and CI scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StableErrorCode {
    CliUnknownCommand,
    CliInvalidArguments,
    CliInvalidTarget,
    RepoNotFound,
    RepoCorrupt,
    RepoStateInvalid,
    /// Global config database schema is newer than this Libra binary supports.
    ConfigSchemaFuture,
    ConflictUnresolved,
    ConflictOperationBlocked,
    /// Branch policy (protect/archive metadata) blocked a ref update — the
    /// first enforcement code of the 1.13 policy layer (branch reset +
    /// update-ref); future delete/push/merge enforcement reuses it.
    PolicyRefUpdateBlocked,
    /// A case-fold path collision was refused under `core.casehandling=error`
    /// (lore.md 1.14) — mv/add/checkout/switch on case-insensitive views.
    ConflictCaseCollision,
    /// A `layer apply` destination collided with tracked (index/HEAD) content
    /// (lore.md 2.4) — fail-closed; a layer may only ADD untracked paths.
    LayerConflict,
    /// `file obliterate` on an object with no payload / unknown OID (2.5).
    ObliterateNotFound,
    /// `file obliterate` refused a packed-only object (no pack surgery) (2.5).
    ObliteratePacked,
    /// `file obliterate` refused to proceed without `--yes` confirmation (2.5).
    ObliterateConfirm,
    NetworkUnavailable,
    NetworkProtocol,
    AuthMissingCredentials,
    AuthPermissionDenied,
    IoReadFailed,
    IoWriteFailed,
    InternalInvariant,
    /// Command succeeded but emitted warnings (`--exit-code-on-warning`).
    WarningEmitted,
    /// All pathspecs matched ignored files; nothing was staged.
    AddNothingStaged,
    /// Feature or operation is not yet supported.
    Unsupported,
    /// `bisect view` / `bisect run` invoked outside an active bisect session.
    BisectNotActive,
    /// `bisect run` command exited with code ≥ 128 or was killed by a signal.
    BisectRunFailed,
    /// `bisect run` cannot advance because no candidate commits remain.
    BisectNoCandidates,
    /// AI agent run hit a configured budget cap (cost, tokens, steps,
    /// or wall-clock). OC-Phase 5 P5.3 enforcement surface.
    AgentBudgetExceeded,
    /// External `libra-agent-*` discovery/invocation requested while the
    /// `agent.external_agents.enabled` gate (default `false`) is off (E10
    /// `ERR_AGENT_EXTERNAL_AGENTS_DISABLED`, AG-18).
    AgentExternalAgentsDisabled,
    /// External agent binary negotiated an incompatible protocol version
    /// (E10 `ERR_AGENT_PROTOCOL_VERSION_MISMATCH`, AG-18).
    AgentProtocolVersionMismatch,
    /// Method invoked on an external agent without being declared in its
    /// negotiated capabilities (E10 `ERR_AGENT_CAPABILITY_UNDECLARED`, AG-18).
    AgentCapabilityUndeclared,
    /// External binary failed provenance revalidation — sha256/device/
    /// inode/mtime changed since trust was recorded, or the binary is not
    /// trusted (E10 `ERR_AGENT_PROVENANCE_REJECTED`, AG-18).
    AgentProvenanceRejected,
    /// External `libra-agent-*` binary tried to shadow a built-in agent
    /// slug (E10 `ERR_AGENT_BUILTIN_SLUG_IMPERSONATION`, AG-18).
    AgentBuiltinSlugImpersonation,
    /// External agent IO exceeded hard caps or failed redaction; output is
    /// withheld fail-closed (E10 `ERR_AGENT_IO_REDACTION_SECURITY_FAILURE`,
    /// AG-18/AG-19).
    AgentIoRedactionSecurityFailure,
    /// Hook envelope failed central validation (UTF-8/JSON/schema/path)
    /// before checkpoint persistence (E10 `ERR_AGENT_HOOK_ENVELOPE_INVALID`,
    /// AG-19).
    AgentHookEnvelopeInvalid,
    /// Agent checkpoint store inconsistent across ref/DB/object-index
    /// (E10 `ERR_AGENT_CHECKPOINT_STORE_INCONSISTENT`, AG-20).
    AgentCheckpointStoreInconsistent,
    /// `review --fix` / `investigate fix` requested but the internal
    /// AgentRuntime fix bridge is unavailable (E10
    /// `ERR_AGENT_FIX_BRIDGE_UNAVAILABLE`, AG-22/AG-23).
    AgentFixBridgeUnavailable,
    /// Untrusted seed content attempted to enter a mutating workflow
    /// without explicit approval (E10 `ERR_AGENT_UNTRUSTED_SEED_FOR_MUTATION`,
    /// AG-22/AG-23).
    AgentUntrustedSeedForMutation,
    /// External agent RPC transport failed — invoke timeout, broken
    /// pipe / unexpected exit, or a malformed JSON-RPC frame — and the
    /// invocation was withheld fail-closed (E10
    /// `ERR_AGENT_RPC_TRANSPORT_FAILED`, allocated during the AG-18
    /// implementation slice; distinct from the IO hard-cap / redaction
    /// failures covered by `LBR-AGENT-007`).
    AgentRpcTransportFailed,
    /// Raw (un-redacted) checkpoint access/export was requested without
    /// the required `--allow-raw` authorization, and was refused
    /// fail-closed (AG-24a compliance, E10
    /// `ERR_AGENT_RAW_ACCESS_DENIED`). The refusal is itself audited in
    /// `agent_audit_log` (granted = 0).
    AgentRawAccessDenied,
    /// A `review` / `investigate` run was refused because the shared
    /// run-level admission queue is full (more than `agent.max_concurrent_runs`
    /// runs are active and the wait queue already holds its cap). Fail-closed
    /// so callers never silently overrun the concurrency budget (A0-04, E10
    /// `ERR_AGENT_RUN_QUEUE_FULL`).
    AgentRunQueueFull,
}

impl Serialize for StableErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl StableErrorCode {
    /// Render the code as its stable `LBR-*` identifier. Documented in
    /// `docs/error-codes.md` and treated as part of the public CLI contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CliUnknownCommand => "LBR-CLI-001",
            Self::CliInvalidArguments => "LBR-CLI-002",
            Self::CliInvalidTarget => "LBR-CLI-003",
            Self::RepoNotFound => "LBR-REPO-001",
            Self::RepoCorrupt => "LBR-REPO-002",
            Self::RepoStateInvalid => "LBR-REPO-003",
            Self::ConfigSchemaFuture => "LBR-CONFIG-001",
            Self::ConflictUnresolved => "LBR-CONFLICT-001",
            Self::ConflictOperationBlocked => "LBR-CONFLICT-002",
            Self::PolicyRefUpdateBlocked => "LBR-POLICY-001",
            Self::ConflictCaseCollision => "LBR-CASE-001",
            Self::LayerConflict => "LBR-LAYER-001",
            Self::ObliterateNotFound => "LBR-OBLITERATE-001",
            Self::ObliteratePacked => "LBR-OBLITERATE-002",
            Self::ObliterateConfirm => "LBR-OBLITERATE-003",
            Self::NetworkUnavailable => "LBR-NET-001",
            Self::NetworkProtocol => "LBR-NET-002",
            Self::AuthMissingCredentials => "LBR-AUTH-001",
            Self::AuthPermissionDenied => "LBR-AUTH-002",
            Self::IoReadFailed => "LBR-IO-001",
            Self::IoWriteFailed => "LBR-IO-002",
            Self::InternalInvariant => "LBR-INTERNAL-001",
            Self::WarningEmitted => "LBR-WARN-001",
            Self::AddNothingStaged => "LBR-ADD-001",
            Self::Unsupported => "LBR-UNSUPPORTED-001",
            Self::BisectNotActive => "LBR-BISECT-001",
            Self::BisectRunFailed => "LBR-BISECT-002",
            Self::BisectNoCandidates => "LBR-BISECT-003",
            Self::AgentBudgetExceeded => "LBR-AGENT-001",
            Self::AgentExternalAgentsDisabled => "LBR-AGENT-002",
            Self::AgentProtocolVersionMismatch => "LBR-AGENT-003",
            Self::AgentCapabilityUndeclared => "LBR-AGENT-004",
            Self::AgentProvenanceRejected => "LBR-AGENT-005",
            Self::AgentBuiltinSlugImpersonation => "LBR-AGENT-006",
            Self::AgentIoRedactionSecurityFailure => "LBR-AGENT-007",
            Self::AgentHookEnvelopeInvalid => "LBR-AGENT-008",
            Self::AgentCheckpointStoreInconsistent => "LBR-AGENT-009",
            Self::AgentFixBridgeUnavailable => "LBR-AGENT-010",
            Self::AgentUntrustedSeedForMutation => "LBR-AGENT-011",
            Self::AgentRpcTransportFailed => "LBR-AGENT-012",
            Self::AgentRawAccessDenied => "LBR-AGENT-013",
            Self::AgentRunQueueFull => "LBR-AGENT-014",
        }
    }

    /// Group this code under one of the broad categories used for shell
    /// scripting and structured automation.
    pub const fn category(self) -> CliErrorCategory {
        match self {
            Self::CliUnknownCommand | Self::CliInvalidArguments | Self::CliInvalidTarget => {
                CliErrorCategory::Cli
            }
            Self::RepoNotFound | Self::RepoCorrupt | Self::RepoStateInvalid => {
                CliErrorCategory::Repo
            }
            Self::ConfigSchemaFuture => CliErrorCategory::Config,
            Self::ConflictUnresolved
            | Self::ConflictOperationBlocked
            // Policy refusals ride the Conflict category (no dedicated
            // Policy category yet; the JSON envelope reads "conflict").
            | Self::PolicyRefUpdateBlocked
            | Self::ConflictCaseCollision
            | Self::LayerConflict => CliErrorCategory::Conflict,
            Self::NetworkUnavailable | Self::NetworkProtocol => CliErrorCategory::Network,
            Self::AuthMissingCredentials | Self::AuthPermissionDenied => CliErrorCategory::Auth,
            Self::IoReadFailed | Self::IoWriteFailed => CliErrorCategory::Io,
            Self::InternalInvariant | Self::Unsupported | Self::BisectRunFailed => {
                CliErrorCategory::Internal
            }
            Self::WarningEmitted => CliErrorCategory::Warning,
            Self::AddNothingStaged => CliErrorCategory::Cli,
            Self::BisectNotActive | Self::BisectNoCandidates => CliErrorCategory::Repo,
            // Obliteration: not-found/packed are object-state (Repo); the
            // confirmation refusal is a Conflict-style block against an
            // irreversible destructive action.
            Self::ObliterateNotFound | Self::ObliteratePacked => CliErrorCategory::Repo,
            Self::ObliterateConfirm => CliErrorCategory::Conflict,
            // Budget caps surface as runtime/internal failures: the run
            // didn't crash, but the operator-configured cap forced an
            // early abort. Fits the Internal category (the run could
            // continue if the cap were lifted) per docs/error-codes.md.
            Self::AgentBudgetExceeded => CliErrorCategory::Internal,
            Self::AgentExternalAgentsDisabled
            | Self::AgentProtocolVersionMismatch
            | Self::AgentCapabilityUndeclared
            | Self::AgentProvenanceRejected
            | Self::AgentBuiltinSlugImpersonation
            | Self::AgentIoRedactionSecurityFailure
            | Self::AgentHookEnvelopeInvalid
            | Self::AgentCheckpointStoreInconsistent
            | Self::AgentFixBridgeUnavailable
            | Self::AgentUntrustedSeedForMutation
            | Self::AgentRpcTransportFailed
            | Self::AgentRawAccessDenied
            | Self::AgentRunQueueFull => CliErrorCategory::Internal,
        }
    }

    /// Default coarse exit code for this stable code, before any per-error
    /// override is applied. `AddNothingStaged` is special-cased back to
    /// `Fatal` (128) to match Git's behaviour for `git add` with only ignored
    /// paths even though the category is `Cli`.
    pub const fn exit_code(self) -> CliExitCode {
        match self {
            // AddNothingStaged falls in the Cli category (which normally
            // maps to Usage/129), but "nothing to add" is a runtime
            // condition, not a CLI usage error — exit 128 matches Git's
            // behavior for `git add` with only ignored paths.
            Self::AddNothingStaged => CliExitCode::Fatal,
            _ => match self.category() {
                CliErrorCategory::Cli => CliExitCode::Usage,
                CliErrorCategory::Warning => CliExitCode::Warning,
                _ => CliExitCode::Fatal,
            },
        }
    }

    /// Fine-grained exit code for backward compatibility.
    ///
    /// Returns the category-specific exit code (2–9) used prior to the
    /// Git-standard 128/129 migration.  Activated by setting
    /// `LIBRA_FINE_EXIT_CODES=1`.
    pub const fn fine_exit_code(self) -> i32 {
        match self.category() {
            CliErrorCategory::Cli => 2,
            CliErrorCategory::Repo => 3,
            CliErrorCategory::Config => 8,
            CliErrorCategory::Conflict => 4,
            CliErrorCategory::Network => 5,
            CliErrorCategory::Auth => 6,
            CliErrorCategory::Io => 7,
            CliErrorCategory::Internal => 8,
            CliErrorCategory::Warning => 9,
        }
    }

    /// Long-form description rendered by `libra help error-codes`.
    /// Intended for direct human consumption — should be a complete sentence.
    pub const fn description(self) -> &'static str {
        match self {
            Self::CliUnknownCommand => "Unknown command or unsupported top-level invocation.",
            Self::CliInvalidArguments => "Invalid or missing CLI arguments.",
            Self::CliInvalidTarget => "Invalid object, revision, pathspec, or command target.",
            Self::RepoNotFound => "Current directory is not a Libra repository.",
            Self::RepoCorrupt => "Repository metadata is missing, incompatible, or corrupt.",
            Self::RepoStateInvalid => {
                "Repository state prevents the requested operation from proceeding."
            }
            Self::ConfigSchemaFuture => {
                "Global config database schema is newer than this Libra binary supports."
            }
            Self::ConflictUnresolved => {
                "Operation stopped because unresolved conflicts are present."
            }
            Self::PolicyRefUpdateBlocked => {
                "Branch policy (protect/archive metadata) blocked the ref update."
            }
            Self::ConflictCaseCollision => {
                "Paths that differ only by case collide on a case-insensitive filesystem."
            }
            Self::LayerConflict => {
                "A layer overlay path collided with tracked content; a layer may only add                  untracked paths."
            }
            Self::ObliterateNotFound => "No payload was found for the object to obliterate.",
            Self::ObliteratePacked => {
                "The object exists only inside a packfile; v1 obliteration cannot rewrite packs."
            }
            Self::ObliterateConfirm => {
                "Obliteration was not confirmed; it is irreversible and requires --yes."
            }
            Self::ConflictOperationBlocked => {
                "Operation was blocked to avoid overwriting local or remote state."
            }
            Self::NetworkUnavailable => "Remote transport, connectivity, or reachability failure.",
            Self::NetworkProtocol => "Remote protocol, sideband, or pack negotiation failure.",
            Self::AuthMissingCredentials => {
                "Required credentials, identity, key material, or tokens are missing."
            }
            Self::AuthPermissionDenied => {
                "Credentials were present but the operation is not permitted."
            }
            Self::IoReadFailed => "Filesystem or storage read failed.",
            Self::IoWriteFailed => "Filesystem or storage write failed.",
            Self::InternalInvariant => {
                "Unexpected internal failure or broken invariant. This should be reported."
            }
            Self::WarningEmitted => {
                "Command completed successfully but emitted warnings (--exit-code-on-warning)."
            }
            Self::AddNothingStaged => "All specified paths are ignored; nothing was staged.",
            Self::Unsupported => "Feature or operation is not yet supported.",
            Self::BisectNotActive => {
                "`bisect view` or `bisect run` was invoked outside an active bisect session."
            }
            Self::BisectRunFailed => {
                "`bisect run` command exited with code 128+ or was killed by a signal."
            }
            Self::BisectNoCandidates => {
                "Bisect cannot advance because no candidate commits remain to test."
            }
            Self::AgentBudgetExceeded => {
                "AI agent run hit a configured budget cap (cost, tokens, steps, or wall-clock)."
            }
            Self::AgentExternalAgentsDisabled => {
                "External libra-agent-* agents are disabled; set agent.external_agents.enabled=true to opt in."
            }
            Self::AgentProtocolVersionMismatch => {
                "External agent binary negotiated an incompatible protocol version."
            }
            Self::AgentCapabilityUndeclared => {
                "Method not declared in the external agent's negotiated capabilities."
            }
            Self::AgentProvenanceRejected => {
                "External agent binary failed provenance revalidation (untrusted or changed since trust)."
            }
            Self::AgentBuiltinSlugImpersonation => {
                "External libra-agent-* binary tried to impersonate a built-in agent slug."
            }
            Self::AgentIoRedactionSecurityFailure => {
                "External agent IO exceeded hard caps or failed redaction; output withheld fail-closed."
            }
            Self::AgentHookEnvelopeInvalid => {
                "Hook envelope failed validation before checkpoint persistence."
            }
            Self::AgentCheckpointStoreInconsistent => {
                "Agent checkpoint store inconsistent across ref/DB/object-index; run libra agent doctor."
            }
            Self::AgentFixBridgeUnavailable => {
                "review/investigate --fix requires the internal AgentRuntime fix bridge, which is not available."
            }
            Self::AgentUntrustedSeedForMutation => {
                "Untrusted seed content cannot enter a mutating workflow without explicit approval."
            }
            Self::AgentRpcTransportFailed => {
                "External agent RPC transport failed (timeout, broken pipe, or malformed frame); invocation withheld fail-closed."
            }
            Self::AgentRawAccessDenied => {
                "Raw (un-redacted) checkpoint access/export denied without --allow-raw; redacted --detail/--transcript output stays available."
            }
            Self::AgentRunQueueFull => {
                "Too many concurrent review/investigate runs are active and the wait queue is full; the run was refused fail-closed."
            }
        }
    }
}

/// Structured hint text rendered after the main error line.
///
/// Hints are added via [`CliError::with_hint`] / [`CliError::with_priority_hint`].
/// The renderer prefixes every line with `Hint:` (Libra style — not the lowercase
/// `hint:` used by Git).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint(String);

impl Hint {
    /// Construct a hint from any string-like value. The text is stored verbatim;
    /// any prefix stripping happens later in `with_hint`.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// Borrow the hint text without the `Hint:` rendering prefix.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Hint {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Hint {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// User-facing CLI error with explicit rendering and exit semantics.
///
/// Build via the family of constructors (`fatal`, `failure`, `repo_not_found`, ...)
/// then chain with `with_hint`, `with_usage`, `with_detail`, etc. The resulting
/// error knows how to render itself for both humans and machines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    kind: CliErrorKind,
    stable_code: StableErrorCode,
    message: String,
    hints: Vec<Hint>,
    usage: Option<String>,
    details: BTreeMap<String, Value>,
    /// Optional override for the process exit code. When set, this takes
    /// precedence over the code derived from [`StableErrorCode`].
    exit_code_override: Option<i32>,
    /// When true, `print_for_output` / `print_stderr` emit nothing.
    /// Used for exit-code-only signalling (e.g. `status --exit-code`).
    silent: bool,
    /// Whether rendering should append the standard issue-report hint.
    report_issue_hint: bool,
}

impl CliError {
    /// Internal constructor used by every public builder.
    ///
    /// Boundary conditions:
    /// - Runs the legacy substring inference (`infer_stable_error_code`) so callers
    ///   that pre-date the structured-code era still get a reasonable default.
    ///   Callers should override with `with_stable_code` whenever the precise code
    ///   is known.
    fn new(kind: CliErrorKind, message: impl Into<String>) -> Self {
        let message = message.into();
        let stable_code = infer_stable_error_code(kind, &message);
        let report_issue_hint =
            stable_code == StableErrorCode::InternalInvariant && is_internal_reportable(&message);
        Self {
            kind,
            stable_code,
            message,
            hints: Vec::new(),
            usage: None,
            details: BTreeMap::new(),
            exit_code_override: None,
            silent: false,
            report_issue_hint,
        }
    }

    /// Create a silent exit error that only sets the process exit code
    /// without printing anything to stderr. Used for `--exit-code` style
    /// flags where a non-zero exit is a signal, not an error.
    ///
    /// Boundary conditions:
    /// - `print_stderr` and `print_for_output` become no-ops on silent errors.
    /// - The `kind` is set to `Failure` and the `stable_code` to
    ///   `InternalInvariant`; these are unused while silent but become visible if
    ///   a caller accidentally non-silently renders the error.
    pub fn silent_exit(code: i32) -> Self {
        Self {
            kind: CliErrorKind::Failure,
            stable_code: StableErrorCode::InternalInvariant,
            message: String::new(),
            hints: Vec::new(),
            usage: None,
            details: BTreeMap::new(),
            exit_code_override: Some(code),
            silent: true,
            report_issue_hint: false,
        }
    }

    /// Canonical "not a Libra repository" error with the standard hint suggesting
    /// `libra init`. Centralising it here keeps the wording identical across every
    /// preflight site.
    pub fn repo_not_found() -> Self {
        Self::fatal("not a libra repository (or any of the parent directories): .libra")
            .with_stable_code(StableErrorCode::RepoNotFound)
            .with_hint("run 'libra init' to create a repository in the current directory.")
    }

    /// Top-level "no such subcommand" error. Rendered without a prefix so the
    /// output matches Git's `'wat' is not a libra command.` style verbatim.
    pub fn unknown_command(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::UnknownCommand, message)
            .with_stable_code(StableErrorCode::CliUnknownCommand)
    }

    /// Runtime failure rendered without a leading severity prefix.
    ///
    /// Use this only for Git-compatible command surfaces whose canonical human
    /// output is an advisory block instead of an `error:` line.
    pub fn unprefixed_failure(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::UnprefixedFailure, message)
    }

    /// Argv parse error detected by clap (root-level). Renders with the `error:`
    /// prefix. Use this for failures that occur before any subcommand handler runs.
    pub fn parse_usage(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::ParseUsage, message)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    }

    /// Subcommand-level usage error (e.g. mutually exclusive flags). Same `error:`
    /// prefix as `parse_usage` but classifies as `CommandUsage` for downstream
    /// reporting.
    pub fn command_usage(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::CommandUsage, message)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    }

    /// Hard runtime error rendered with `fatal:` prefix. Default exit code 128.
    pub fn fatal(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::Fatal, message)
    }

    /// Soft runtime error rendered with `error:` prefix. Default exit code 128.
    pub fn failure(message: impl Into<String>) -> Self {
        Self::new(CliErrorKind::Failure, message)
    }

    /// Conflict-class fatal with stable code `ConflictOperationBlocked`.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::fatal(message).with_stable_code(StableErrorCode::ConflictOperationBlocked)
    }

    /// Network-class fatal with stable code `NetworkUnavailable`.
    pub fn network(message: impl Into<String>) -> Self {
        Self::fatal(message).with_stable_code(StableErrorCode::NetworkUnavailable)
    }

    /// Auth-class fatal with stable code `AuthMissingCredentials`.
    pub fn auth(message: impl Into<String>) -> Self {
        Self::fatal(message).with_stable_code(StableErrorCode::AuthMissingCredentials)
    }

    /// IO-class fatal with stable code `IoReadFailed`. For write failures, prefer
    /// `CliError::fatal(...).with_stable_code(StableErrorCode::IoWriteFailed)`.
    pub fn io(message: impl Into<String>) -> Self {
        Self::fatal(message).with_stable_code(StableErrorCode::IoReadFailed)
    }

    /// Internal-invariant fatal. Prefer this for "should not happen" code paths so
    /// users see a clear "this is a Libra bug" categorisation.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::fatal(message).with_stable_code(StableErrorCode::InternalInvariant)
    }

    /// Convert a legacy prefixed error string (e.g. `"fatal: ..."` or
    /// `"error: ..."`) into a structured [`CliError`].
    ///
    /// This is the shared bridge for commands whose inner implementation still
    /// returns `Result<(), String>` with a human-readable prefix.
    ///
    /// New command code should prefer setting [`StableErrorCode`] explicitly
    /// with [`CliError::with_stable_code`] instead of depending on message
    /// substring inference.
    ///
    /// Boundary conditions:
    /// - `usage:` prefix is converted into a `CommandUsage` error and the original
    ///   prefix is preserved in the usage block (prefix `usage:` retained for
    ///   round-trip compatibility).
    /// - `warning:` prefix is folded into a `Failure` error so the message still
    ///   shows up; callers who want true warning behaviour should use
    ///   [`emit_warning`] instead.
    /// - Any other input falls back to `Failure` with the trimmed message.
    pub fn from_legacy_string(msg: impl Into<String>) -> Self {
        let raw = msg.into();
        let trimmed = raw.trim().to_string();
        if let Some(rest) = trimmed.strip_prefix("fatal: ") {
            Self::fatal(rest.to_string())
        } else if let Some(rest) = trimmed.strip_prefix("error: ") {
            Self::failure(rest.to_string())
        } else if let Some(rest) = trimmed.strip_prefix("warning: ") {
            Self::failure(rest.to_string())
        } else if let Some(rest) = trimmed.strip_prefix("usage: ") {
            Self::command_usage("invalid arguments").with_usage(format!("usage: {rest}"))
        } else {
            Self::failure(trimmed)
        }
    }

    /// Borrow the [`CliErrorKind`] used to drive prefix selection.
    pub fn kind(&self) -> CliErrorKind {
        self.kind
    }

    /// Borrow the stable machine-readable error code.
    pub fn stable_code(&self) -> StableErrorCode {
        self.stable_code
    }

    /// Convenience: derive the broad [`CliErrorCategory`] from `stable_code`.
    pub fn category(&self) -> CliErrorCategory {
        self.stable_code.category()
    }

    /// Map kind to the human-facing severity level rendered as `fatal:` or
    /// `error:`. Returns `None` for `UnknownCommand`, which renders without a
    /// prefix.
    pub fn level(&self) -> Option<ErrorLevel> {
        match self.kind {
            CliErrorKind::Fatal => Some(ErrorLevel::Fatal),
            CliErrorKind::ParseUsage | CliErrorKind::CommandUsage | CliErrorKind::Failure => {
                Some(ErrorLevel::Error)
            }
            CliErrorKind::UnknownCommand | CliErrorKind::UnprefixedFailure => None,
        }
    }

    /// Borrow the primary error message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Borrow the optional usage block displayed after the message.
    pub fn usage(&self) -> Option<&str> {
        self.usage.as_deref()
    }

    /// Borrow the (at most two) accumulated hints.
    pub fn hints(&self) -> &[Hint] {
        &self.hints
    }

    /// Borrow the structured details map (rendered into the JSON envelope).
    pub fn details(&self) -> &BTreeMap<String, Value> {
        &self.details
    }

    /// Override the stable code that was inferred from the message.
    ///
    /// When this is called from a `From<DomainError> for CliError` mapping,
    /// prefer adding a short nearby comment explaining why the selected code
    /// represents the recovery intent. For example, a partially initialized
    /// repository should usually map to [`StableErrorCode::RepoStateInvalid`],
    /// while a missing input path should usually map to
    /// [`StableErrorCode::IoReadFailed`].
    pub fn with_stable_code(mut self, stable_code: StableErrorCode) -> Self {
        self.stable_code = stable_code;
        self.report_issue_hint = stable_code == StableErrorCode::InternalInvariant;
        self
    }

    /// Append a hint to the error.
    ///
    /// Boundary conditions:
    /// - The third hint and beyond are silently dropped — the renderer caps
    ///   visible hints at two for readability.
    /// - Any leading `Hint:` / `hint:` prefix on the input is stripped, so callers
    ///   who copy text from existing rendered errors are not double-prefixed.
    /// - Empty / whitespace-only hints are ignored.
    pub fn with_hint(mut self, hint: impl Into<Hint>) -> Self {
        if self.hints.len() >= 2 {
            return self;
        }

        let hint = normalize_hint_text(hint.into().0);
        if hint.trim().is_empty() {
            return self;
        }

        self.hints.push(Hint::new(hint));
        self
    }

    /// Insert a high-priority hint at the front.  If the hint budget (2) is
    /// already full, the *last* (lowest-priority) hint is dropped to make room.
    ///
    /// Boundary conditions:
    /// - Empty / whitespace hints are still ignored.
    /// - Used by repository-conversion preflight to surface a more relevant hint
    ///   ahead of the generic "run libra init" suggestion.
    pub fn with_priority_hint(mut self, hint: impl Into<Hint>) -> Self {
        let hint = normalize_hint_text(hint.into().0);
        if hint.trim().is_empty() {
            return self;
        }

        if self.hints.len() >= 2 {
            self.hints.pop(); // drop lowest-priority
        }
        self.hints.insert(0, Hint::new(hint));
        self
    }

    /// Attach a structured key/value detail. Detail keys are stable enough to be
    /// matched against by automation; treat them as part of the public contract
    /// once a release ships them.
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    /// Set the usage block. Empty / whitespace-only input is ignored so callers
    /// can pipe through clap output without double-checking for emptiness.
    pub fn with_usage(mut self, usage: impl Into<String>) -> Self {
        let usage = usage.into();
        if !usage.trim().is_empty() {
            self.usage = Some(usage);
        }
        self
    }

    /// Override the process exit code for this error instance.
    ///
    /// When set, this takes precedence over both the standard
    /// [`StableErrorCode`]-derived code and the fine-grained exit code.
    /// Use sparingly for cases where Git-compatible exit codes differ from
    /// the Libra category mapping (e.g. `git config --get` returns 1 when
    /// a key is not found).
    pub fn with_exit_code(mut self, code: i32) -> Self {
        self.exit_code_override = Some(code);
        self
    }

    /// Resolve the final exit code in the order: explicit override > legacy
    /// fine-grained codes (when env var is set) > coarse Git-style code.
    pub fn exit_code(&self) -> i32 {
        if let Some(code) = self.exit_code_override {
            return code;
        }
        if fine_exit_codes_enabled() {
            return self.stable_code.fine_exit_code();
        }
        self.stable_code.exit_code().as_i32()
    }

    /// Render the error as the structured JSON envelope.
    ///
    /// Boundary conditions:
    /// - Falls back to a hard-coded internal-invariant payload if `serde_json`
    ///   serialisation fails. This keeps `--json` mode honest even on the
    ///   pathological case of a bad `details` value.
    pub fn render_json(&self) -> String {
        // INVARIANT: `CliErrorReport` contains only serializable enums, strings,
        // integers, vectors, maps, and `serde_json::Value`. Serialization is
        // expected to succeed; this fallback only guards against an unexpected
        // future regression in the report type itself.
        serde_json::to_string(&self.report()).unwrap_or_else(|_| {
            "{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\
\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"failed to serialize CLI error report\",\
\"hints\":[\"please report this issue at: https://github.com/libra-tools/libra/issues\"]}"
                .to_string()
        })
    }

    /// Render a single human-readable string (no `Error-Code:` header, no JSON).
    pub fn render(&self) -> String {
        self.render_human(false)
    }

    /// Render the human form *and* the JSON envelope on a trailing line.
    /// Used when stderr is not a TTY so log scrapers can parse the JSON line.
    pub fn render_report(&self) -> String {
        format!("{}\n{}", self.render_human(true), self.render_json())
    }

    /// Pick the right renderer for the current stderr mode (human / structured)
    /// based on env var override and TTY detection.
    pub fn render_for_stderr(&self) -> String {
        match stderr_render_mode() {
            StderrRenderMode::Human => self.render(),
            StderrRenderMode::Structured => self.render_report(),
        }
    }

    /// Print the error to stderr with the appropriate renderer. Silent errors
    /// (constructed with [`Self::silent_exit`]) are skipped.
    pub fn print_stderr(&self) {
        if self.silent {
            return;
        }
        eprintln!("{}", self.render_for_stderr());
    }

    /// Print the error according to the global output configuration.
    ///
    /// When JSON output is active, the error is rendered as a JSON envelope to
    /// **stderr** so stdout remains reserved for successful command data.
    ///
    /// Boundary conditions:
    /// - In `JsonFormat::Pretty`, the rendered JSON is re-parsed into a
    ///   `serde_json::Value` so the output is human-readable. If that re-parse
    ///   fails, falls back to the original compact line.
    /// - Compact / NDJSON output writes a single line plus a newline, matching
    ///   the format used by successful command output.
    pub fn print_for_output(&self, config: &OutputConfig) {
        if self.silent {
            return;
        }

        if let Some(fmt) = config.json_format {
            let json = self.render_json();
            let stderr = std::io::stderr();
            let mut writer = stderr.lock();
            match fmt {
                JsonFormat::Pretty => {
                    // Re-parse and pretty-print the JSON.
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) {
                        let _ = serde_json::to_writer_pretty(&mut writer, &value);
                        let _ = writeln!(writer);
                    } else {
                        let _ = writeln!(writer, "{json}");
                    }
                }
                JsonFormat::Compact | JsonFormat::Ndjson => {
                    let _ = writeln!(writer, "{json}");
                }
            }
        } else {
            self.print_stderr();
        }
    }

    /// True when this error is the normal Unix "downstream closed stdout"
    /// condition rather than a user-visible failure.
    pub fn is_stdout_broken_pipe(&self) -> bool {
        if self.silent {
            return false;
        }
        let lower = self.message.to_ascii_lowercase();
        let broken_pipe = lower.contains("broken pipe") || lower.contains("os error 32");
        let stdout_write = lower.contains("failed printing to stdout")
            || lower.contains("failed to write output")
            || lower.contains("failed to write json output")
            || lower.contains("failed to write clap output")
            || lower.contains("failed to write completion script")
            || lower.contains("failed to write ls-files output")
            || lower.contains("stdout");
        broken_pipe && stdout_write
    }

    /// Rendered severity string used by JSON envelopes. Stable across releases.
    fn severity(&self) -> &'static str {
        match self.kind {
            CliErrorKind::Fatal => "fatal",
            CliErrorKind::UnknownCommand
            | CliErrorKind::UnprefixedFailure
            | CliErrorKind::ParseUsage
            | CliErrorKind::CommandUsage
            | CliErrorKind::Failure => "error",
        }
    }

    /// Materialise the error into the serde-friendly `CliErrorReport` snapshot.
    fn report(&self) -> CliErrorReport {
        CliErrorReport {
            ok: false,
            error_code: self.stable_code,
            category: self.category(),
            exit_code: self.exit_code(),
            severity: self.severity(),
            message: self.message.clone(),
            usage: self.usage.clone(),
            hints: self.effective_hints(),
            details: self.details.clone(),
        }
    }

    fn render_human(&self, include_error_code: bool) -> String {
        let mut lines = Vec::new();
        match self.kind {
            CliErrorKind::UnknownCommand | CliErrorKind::UnprefixedFailure => {
                lines.push(self.message.clone());
            }
            CliErrorKind::ParseUsage | CliErrorKind::CommandUsage | CliErrorKind::Failure => {
                lines.push(format!("error: {}", self.message));
            }
            CliErrorKind::Fatal => lines.push(format!("fatal: {}", self.message)),
        }

        if include_error_code {
            lines.push(format!("Error-Code: {}", self.stable_code.as_str()));
        }

        if let Some(usage) = &self.usage
            && !usage.trim().is_empty()
        {
            lines.push(usage.trim_end().to_string());
        }

        let hints = self.effective_hints();
        if !hints.is_empty() {
            lines.push(String::new());
            for hint in hints {
                lines.extend(render_hint(&hint));
            }
        }

        lines.join("\n")
    }

    fn effective_hints(&self) -> Vec<String> {
        let mut hints = self
            .hints
            .iter()
            .map(|hint| hint.as_str().to_string())
            .collect::<Vec<_>>();

        if self.report_issue_hint && !hints.iter().any(|hint| hint.contains(LIBRA_ISSUES_URL)) {
            if hints.len() >= 2 {
                hints.pop();
            }
            hints.push(INTERNAL_ERROR_REPORT_HINT.to_string());
        }

        hints
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CliErrorReport {
    ok: bool,
    error_code: StableErrorCode,
    category: CliErrorCategory,
    exit_code: i32,
    severity: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    hints: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    details: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuredStderrMode {
    Auto,
    Always,
}

impl StructuredStderrMode {
    fn from_env() -> Self {
        match env::var(LIBRA_ERROR_JSON_ENV) {
            Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" | "always" => Self::Always,
                _ => Self::Auto,
            },
            Err(_) => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StderrRenderMode {
    Human,
    Structured,
}

fn stderr_render_mode() -> StderrRenderMode {
    match StructuredStderrMode::from_env() {
        StructuredStderrMode::Always => StderrRenderMode::Structured,
        StructuredStderrMode::Auto => {
            if io::stderr().is_terminal() {
                StderrRenderMode::Human
            } else {
                StderrRenderMode::Structured
            }
        }
    }
}

fn normalize_hint_text(text: String) -> String {
    text.lines()
        .map(strip_hint_prefix)
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_hint_prefix(line: &str) -> String {
    let trimmed = line.trim_start();
    if let Some(stripped) = trimmed.strip_prefix("Hint:") {
        return stripped.trim_start().to_string();
    }
    if let Some(stripped) = trimmed.strip_prefix("hint:") {
        return stripped.trim_start().to_string();
    }
    line.to_string()
}

// NOTE: We use "Hint:" (capital H) rather than Git's lowercase "hint:". This is
// a deliberate stylistic choice for Libra — not a bug.
fn render_hint(text: &str) -> Vec<String> {
    text.lines().map(|line| format!("Hint: {}", line)).collect()
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

impl std::error::Error for CliError {}

/// Emit a legacy CLI message to stderr, converting fatal/error lines into the
/// structured [`CliError`] report format.
pub fn emit_legacy_stderr(message: impl Into<String>) {
    let message = message.into();
    if let Some(text) = message.trim().strip_prefix("warning: ") {
        record_warning();
        eprintln!("warning: {}", text);
        return;
    }

    CliError::from_legacy_string(message).print_stderr();
}

/// Print a user-facing error to stderr.
///
/// New command code should prefer returning [`CliError`] instead of printing
/// directly. This macro remains for legacy command paths during migration.
#[macro_export]
macro_rules! cli_error {
    ($prefix:expr => $err:expr) => {{
        $crate::utils::error::emit_legacy_stderr(format!("{}: {}", $prefix, $err));
    }};
    ($err:expr, $($arg:tt)+) => {{
        let prefix = format!($($arg)+);
        $crate::utils::error::emit_legacy_stderr(format!("{prefix}: {}", $err));
    }};
}

/// Emit a warning to stderr and record it for `--exit-code-on-warning`.
///
/// Use this instead of raw `eprintln!("warning: ...")` so that the
/// global warning tracker is updated and the `--exit-code-on-warning` flag
/// works correctly.
pub fn emit_warning(message: impl std::fmt::Display) {
    record_warning();
    eprintln!("warning: {message}");
}

/// Transitional best-effort classifier for legacy string-only error paths.
///
/// New command implementations should set [`StableErrorCode`] explicitly with
/// [`CliError::with_stable_code`] instead of relying on these message
/// heuristics.
fn infer_stable_error_code(kind: CliErrorKind, message: &str) -> StableErrorCode {
    let lower = message.to_ascii_lowercase();

    match kind {
        CliErrorKind::UnknownCommand => StableErrorCode::CliUnknownCommand,
        CliErrorKind::ParseUsage | CliErrorKind::CommandUsage => {
            if is_invalid_target_error(&lower) {
                StableErrorCode::CliInvalidTarget
            } else {
                StableErrorCode::CliInvalidArguments
            }
        }
        CliErrorKind::Fatal | CliErrorKind::Failure | CliErrorKind::UnprefixedFailure => {
            infer_runtime_error_code(&lower)
        }
    }
}

fn infer_runtime_error_code(lower: &str) -> StableErrorCode {
    if is_internal_error(lower) {
        return StableErrorCode::InternalInvariant;
    }
    if is_auth_permission_error(lower) {
        return StableErrorCode::AuthPermissionDenied;
    }
    if is_auth_missing_error(lower) {
        return StableErrorCode::AuthMissingCredentials;
    }
    if is_conflict_unresolved_error(lower) {
        return StableErrorCode::ConflictUnresolved;
    }
    if is_conflict_blocked_error(lower) {
        return StableErrorCode::ConflictOperationBlocked;
    }
    if is_repo_not_found_error(lower) {
        return StableErrorCode::RepoNotFound;
    }
    if is_repo_corrupt_error(lower) {
        return StableErrorCode::RepoCorrupt;
    }
    if is_repo_state_error(lower) {
        return StableErrorCode::RepoStateInvalid;
    }
    if is_network_protocol_error(lower) {
        return StableErrorCode::NetworkProtocol;
    }
    if is_network_unavailable_error(lower) {
        return StableErrorCode::NetworkUnavailable;
    }
    if is_io_write_error(lower) {
        return StableErrorCode::IoWriteFailed;
    }
    if is_io_read_error(lower) {
        return StableErrorCode::IoReadFailed;
    }
    if is_invalid_target_error(lower) {
        return StableErrorCode::CliInvalidTarget;
    }
    if is_usage_error(lower) {
        return StableErrorCode::CliInvalidArguments;
    }

    StableErrorCode::InternalInvariant
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn is_usage_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "unexpected argument",
            "invalid arguments",
            "invalid argument",
            "missing required",
            "requires a value",
            "conflicts with",
            "required when",
            "please specify the destination path explicitly",
            "branch name is required",
            "too many arguments",
            "expected format",
            "clean requires -f or -n",
            "must use http or https",
            "is not a valid url",
            "one of '-t', '-s', '-p', '-e' or an --ai* flag is required",
        ],
    )
}

fn is_invalid_target_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "pathspec",
            "not a valid object name",
            "invalid reference",
            "invalid upstream",
            "invalid remote branch",
            "invalid object",
            "invalid revision",
            "ambiguous argument",
            "<object> is required",
            "outside of the repository",
            "outside repository",
            "not something we can merge",
            "is not a valid stash reference",
            "bad source",
            "is not a directory",
            "can not move directory into itself",
        ],
    )
}

fn is_repo_not_found_error(lower: &str) -> bool {
    lower.contains("not a libra repository")
        || lower.contains("does not appear to be a libra repository")
        || (lower.contains("repository '") && lower.contains("does not exist"))
}

fn is_repo_corrupt_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "repository database not found",
            "unsupported object format",
            "storage broken",
            "corrupted",
            "invalid tag object encoding",
            "failed to load tag object",
            "object storage error",
            "repository broken",
        ],
    )
}

fn is_repo_state_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "head is detached",
            "detached head",
            "no configured remote",
            "no remote configured",
            "no upstream specified",
            "no rebase in progress",
            "not on a branch",
            "current branch",
            "your current branch",
            "no configured push destination",
            "no commit at head",
            "no such remote",
            "refusing to merge unrelated histories",
            "no names found, cannot describe anything",
            "stash does not exist",
            "reflog entry",
        ],
    ) || ((lower.contains("branch") || lower.contains("tag") || lower.contains("remote"))
        && lower.contains("not found"))
}

fn is_conflict_unresolved_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "resolve all conflicts",
            "unresolved conflict",
            "merge conflict",
            "on conflict",
            "conflicted",
            "conflict:",
            "conflict marker",
        ],
    )
}

fn is_conflict_blocked_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "already exists",
            "already in progress",
            "would be overwritten",
            "working tree not clean",
            "unstaged changes",
            "uncommitted changes",
            "untracked working tree file would be overwritten",
            "cannot overwrite",
            "non-fast-forward",
            "not possible to fast-forward",
            "destination path",
            "ignored by one of your",
            "address already in use",
            "multiple root commits",
            "multiple sources moving to the same target path",
            "not under version control",
        ],
    )
}

fn is_network_unavailable_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "failed to discover references",
            "failed to fetch objects",
            "failed to send request",
            "failed to send pack data",
            "failed to read server response",
            "host key verification failed",
            "connection refused",
            "timed out",
            "timeout",
            "tls",
            "ssl",
            "could not resolve host",
            "network error",
            "connection closed unexpectedly",
            "connection reset by peer",
            "remote end hung up unexpectedly",
            "failed to start mcp server",
            "failed to start web server",
        ],
    )
}

fn is_network_protocol_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "protocol error",
            "packet line",
            "pkt-line",
            "pkt line",
            "invalid packet line",
            "invalid pkt-line",
            "sideband",
            "checksum mismatch",
            "content-type",
            "object format mismatch",
            "unpack failed",
            "ref update failed",
            "send_pack failed",
            "pack encoding failed",
            "failed to build pack index",
        ],
    )
}

fn is_auth_missing_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "_api_key is not set",
            "api key is not set",
            "credential",
            "authentication required",
            "author identity unknown",
            "name and email are not configured",
            "unseal key not found",
            "username or password",
            "no ssh public key found",
            "missing token",
        ],
    )
}

fn is_auth_permission_error(lower: &str) -> bool {
    if lower.contains("permission denied")
        && contains_any(
            lower,
            &[
                "failed to remove",
                "failed to write",
                "failed to save",
                "failed to update",
                "failed to restore",
                "failed to create",
                "failed to open",
                "could not open",
                "failed to load",
                "failed to read",
            ],
        )
    {
        return false;
    }

    contains_any(
        lower,
        &[
            "forbidden",
            "permission denied",
            "access denied",
            "push access",
            "insufficient scope",
            "unauthorized",
            "not authorized",
            "authentication failed",
        ],
    )
}

fn is_io_read_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "failed to read",
            "unable to read",
            "could not open",
            "failed to open",
            "failed to load",
            "could not read",
            "failed to determine working directory",
            "failed to list",
            "invalid path encoding",
            "unable to read index",
        ],
    )
}

fn is_io_write_error(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "failed to write",
            "failed to save",
            "write error",
            "failed to create",
            "could not create",
            "failed to remove",
            "failed to restore",
            "failed to persist",
            "failed to update",
            "failed to delete",
            "failed to reset working directory",
            "failed to move",
            "unable to write index",
        ],
    )
}

fn is_internal_error(lower: &str) -> bool {
    contains_any(lower, &["internal error", "panic", "invariant"])
}

fn is_internal_reportable(message: &str) -> bool {
    contains_any(
        &message.to_ascii_lowercase(),
        &["internal error", "panic", "invariant", "unexpected"],
    )
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use serial_test::serial;

    use super::{
        CliError, CliErrorCategory, CliErrorKind, LIBRA_ERROR_JSON_ENV, LIBRA_FINE_EXIT_CODES_ENV,
        StableErrorCode, StderrRenderMode, StructuredStderrMode, stderr_render_mode,
    };
    use crate::utils::test::ScopedEnvVar;

    #[test]
    fn fatal_render_uses_git_style_prefix() {
        let rendered = CliError::fatal("failed to open index").render();
        assert_eq!(rendered, "fatal: failed to open index");
    }

    #[test]
    fn repo_not_found_includes_standard_hint() {
        let rendered = CliError::repo_not_found().render();
        assert_eq!(
            rendered,
            "fatal: not a libra repository (or any of the parent directories): .libra\n\nHint: run 'libra init' to create a repository in the current directory."
        );
    }

    #[test]
    fn parse_usage_render_includes_usage_and_hints() {
        let rendered = CliError::parse_usage("unexpected argument '--bad'")
            .with_usage("Usage: libra add [OPTIONS] [PATHSPEC]...")
            .with_hint("use '--help' to see available options.")
            .render();
        assert_eq!(
            rendered,
            "error: unexpected argument '--bad'\nUsage: libra add [OPTIONS] [PATHSPEC]...\n\nHint: use '--help' to see available options."
        );
    }

    #[test]
    fn multiline_hint_prefixes_every_line() {
        let rendered = CliError::failure("name and email are not configured")
            .with_hint(
                "to configure, run:\n  libra config --global user.name \"Some One\"\n  libra config --global user.email \"someone@example.com\"",
            )
            .render();
        assert_eq!(
            rendered,
            "error: name and email are not configured\n\nHint: to configure, run:\nHint:   libra config --global user.name \"Some One\"\nHint:   libra config --global user.email \"someone@example.com\""
        );
    }

    // Tests asserting DEFAULT exit codes must be `#[serial]`: the
    // fine-exit-code tests flip the process-global
    // `LIBRA_FINE_EXIT_CODES` env var under `#[serial]`, and an
    // unmarked reader running in parallel can observe it mid-flight
    // (seen as exit code 3 instead of 128 in CI-like parallel runs).
    #[test]
    #[serial]
    fn unknown_command_has_no_error_prefix() {
        let err =
            CliError::unknown_command("libra: 'wat' is not a libra command. See 'libra --help'.");
        assert_eq!(err.kind(), CliErrorKind::UnknownCommand);
        assert_eq!(
            err.render(),
            "libra: 'wat' is not a libra command. See 'libra --help'."
        );
        assert_eq!(err.exit_code(), 129);
    }

    #[test]
    #[serial]
    fn unprefixed_failure_has_no_error_prefix_but_keeps_runtime_exit_code() {
        let err = CliError::unprefixed_failure("plain advisory block")
            .with_stable_code(StableErrorCode::RepoStateInvalid);
        assert_eq!(err.kind(), CliErrorKind::UnprefixedFailure);
        assert_eq!(err.render(), "plain advisory block");
        assert_eq!(err.exit_code(), 128);
    }

    #[test]
    fn with_hint_strips_prefix_and_limits_count() {
        let rendered = CliError::failure("bad")
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("hint: first")
            .with_hint("Hint: second")
            .with_hint("third")
            .render();
        assert_eq!(rendered, "error: bad\n\nHint: first\nHint: second");
    }

    #[test]
    fn internal_error_render_includes_issue_url() {
        let rendered = CliError::internal("status index should be loaded").render();
        assert_eq!(
            rendered,
            "fatal: status index should be loaded\n\nHint: please report this issue at: https://github.com/libra-tools/libra/issues"
        );
    }

    #[test]
    fn explicit_internal_code_render_includes_issue_url() {
        let rendered = CliError::fatal("tree creation failed")
            .with_stable_code(StableErrorCode::InternalInvariant)
            .render();
        assert!(rendered.contains("https://github.com/libra-tools/libra/issues"));
    }

    #[test]
    fn inferred_non_reportable_internal_does_not_add_issue_hint() {
        let rendered = CliError::failure("bad").render();
        assert_eq!(rendered, "error: bad");
    }

    #[test]
    fn internal_error_json_includes_issue_url_hint() {
        let json = CliError::internal("unexpected state transition").render_json();
        let payload: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(payload["error_code"], "LBR-INTERNAL-001");
        assert_eq!(
            payload["hints"][0],
            "please report this issue at: https://github.com/libra-tools/libra/issues"
        );
    }

    #[test]
    fn from_legacy_string_strips_warning_prefix() {
        let err = CliError::from_legacy_string("warning: something off");
        assert_eq!(err.kind(), CliErrorKind::Failure);
        assert_eq!(err.render(), "error: something off");
    }

    #[test]
    fn from_legacy_string_handles_usage_prefix() {
        let err = CliError::from_legacy_string("usage: libra mv <source> <dest>");
        assert_eq!(err.kind(), CliErrorKind::CommandUsage);
        assert!(err.render().contains("usage: libra mv <source> <dest>"));
    }

    #[test]
    #[serial]
    fn stable_code_maps_repo_not_found_to_exit_code_128() {
        let err = CliError::repo_not_found();
        assert_eq!(err.stable_code(), StableErrorCode::RepoNotFound);
        assert_eq!(err.exit_code(), 128);
    }

    #[test]
    #[serial]
    fn stable_code_infers_auth_missing() {
        let err = CliError::fatal("OPENAI_API_KEY is not set");
        assert_eq!(err.stable_code(), StableErrorCode::AuthMissingCredentials);
        assert_eq!(err.exit_code(), 128);
    }

    #[test]
    #[serial]
    fn stable_code_infers_conflict() {
        let err = CliError::fatal("rebase already in progress");
        assert_eq!(err.stable_code(), StableErrorCode::ConflictOperationBlocked);
        assert_eq!(err.exit_code(), 128);
    }

    #[test]
    fn legacy_inference_matches_representative_runtime_messages() {
        let cases = [
            (
                "repository database not found at '/tmp/libra.db'",
                StableErrorCode::RepoCorrupt,
            ),
            (
                "error: you must resolve all conflicts before continuing",
                StableErrorCode::ConflictUnresolved,
            ),
            (
                "failed to read server response: timed out",
                StableErrorCode::NetworkUnavailable,
            ),
            (
                "invalid packet line header 'zzzz'",
                StableErrorCode::NetworkProtocol,
            ),
            (
                "failed to write index: Permission denied",
                StableErrorCode::IoWriteFailed,
            ),
            (
                "author identity unknown",
                StableErrorCode::AuthMissingCredentials,
            ),
            ("failed to read object", StableErrorCode::IoReadFailed),
        ];

        for (message, expected) in cases {
            let err = CliError::fatal(message);
            assert_eq!(
                err.stable_code(),
                expected,
                "message should classify consistently: {message}"
            );
        }
    }

    #[test]
    fn legacy_inference_does_not_treat_generic_conflict_word_as_unresolved_conflict() {
        let err = CliError::fatal("the conflict resolution strategy is unavailable");
        assert_eq!(err.stable_code(), StableErrorCode::InternalInvariant);
    }

    #[test]
    fn legacy_inference_does_not_treat_generic_protocol_word_as_network_protocol_error() {
        let err = CliError::fatal("unsupported protocol version for local storage");
        assert_eq!(err.stable_code(), StableErrorCode::InternalInvariant);
    }

    #[test]
    fn legacy_inference_routes_connection_closed_unexpectedly_to_network() {
        let err = CliError::fatal("connection closed unexpectedly by remote peer");
        assert_eq!(err.stable_code(), StableErrorCode::NetworkUnavailable);
    }

    #[test]
    #[serial]
    fn render_report_appends_json_payload() {
        let rendered = CliError::repo_not_found().render_report();
        assert!(rendered.contains("Error-Code: LBR-REPO-001"));
        let json_line = rendered
            .lines()
            .last()
            .expect("error report should include a JSON line");
        let payload: Value =
            serde_json::from_str(json_line).expect("last line should be valid JSON");
        assert_eq!(payload["error_code"], "LBR-REPO-001");
        assert_eq!(payload["category"], "repo");
        assert_eq!(payload["exit_code"], 128);
    }

    #[test]
    fn render_report_includes_structured_details() {
        let rendered = CliError::fatal("failed to read object")
            .with_stable_code(StableErrorCode::IoReadFailed)
            .with_detail("object", "HEAD")
            .render_report();
        let json_line = rendered.lines().last().unwrap();
        let payload: Value = serde_json::from_str(json_line).unwrap();
        assert_eq!(payload["details"]["object"], "HEAD");
    }

    #[test]
    #[serial]
    fn stderr_render_mode_env_defaults_to_auto_for_falsey_values() {
        let _guard = ScopedEnvVar::set(LIBRA_ERROR_JSON_ENV, "0");
        assert_eq!(StructuredStderrMode::from_env(), StructuredStderrMode::Auto);
    }

    #[test]
    #[serial]
    fn stderr_render_mode_env_can_force_structured_output() {
        let _guard = ScopedEnvVar::set(LIBRA_ERROR_JSON_ENV, "1");
        assert_eq!(
            StructuredStderrMode::from_env(),
            StructuredStderrMode::Always
        );
        assert_eq!(stderr_render_mode(), StderrRenderMode::Structured);
    }

    #[test]
    #[serial]
    fn fine_exit_codes_env_returns_legacy_category_codes() {
        let _guard = ScopedEnvVar::set(LIBRA_FINE_EXIT_CODES_ENV, "1");

        assert_eq!(CliError::repo_not_found().exit_code(), 3);
        assert_eq!(CliError::fatal("OPENAI_API_KEY is not set").exit_code(), 6);
        assert_eq!(CliError::fatal("rebase already in progress").exit_code(), 4);
        assert_eq!(
            CliError::unknown_command("libra: 'wat' is not a libra command.").exit_code(),
            2
        );
    }

    /// Pin the `LBR-*` stable identifier emitted by
    /// [`StableErrorCode::as_str`] for every variant. The strings
    /// are the canonical public CLI surface — `docs/error-codes.md`
    /// references them by name, every typed-error pin test landed
    /// in v0.17.701..v0.17.709 routes UP to these codes, and JSON
    /// consumers branch on the literal string ("LBR-IO-001") not on
    /// the Rust variant name. A silent rename (e.g. dropping the
    /// `LBR-` prefix or renumbering `LBR-NET-001` → `LBR-NET-002`)
    /// would invalidate every downstream pin without tripping any
    /// test until end-to-end JSON harness assertions caught it.
    ///
    /// Enumerate all 23 non-agent variants so a new addition trips both this
    /// list and the `as_str` impl's exhaustive match.
    #[test]
    fn stable_error_code_as_str_pins_each_variant() {
        assert_eq!(StableErrorCode::CliUnknownCommand.as_str(), "LBR-CLI-001");
        assert_eq!(StableErrorCode::CliInvalidArguments.as_str(), "LBR-CLI-002",);
        assert_eq!(StableErrorCode::CliInvalidTarget.as_str(), "LBR-CLI-003");
        assert_eq!(StableErrorCode::RepoNotFound.as_str(), "LBR-REPO-001");
        assert_eq!(StableErrorCode::RepoCorrupt.as_str(), "LBR-REPO-002");
        assert_eq!(StableErrorCode::RepoStateInvalid.as_str(), "LBR-REPO-003");
        assert_eq!(
            StableErrorCode::ConfigSchemaFuture.as_str(),
            "LBR-CONFIG-001",
        );
        assert_eq!(
            StableErrorCode::ConflictUnresolved.as_str(),
            "LBR-CONFLICT-001",
        );
        assert_eq!(
            StableErrorCode::ConflictOperationBlocked.as_str(),
            "LBR-CONFLICT-002",
        );
        assert_eq!(StableErrorCode::NetworkUnavailable.as_str(), "LBR-NET-001",);
        assert_eq!(StableErrorCode::NetworkProtocol.as_str(), "LBR-NET-002");
        assert_eq!(
            StableErrorCode::AuthMissingCredentials.as_str(),
            "LBR-AUTH-001",
        );
        assert_eq!(
            StableErrorCode::AuthPermissionDenied.as_str(),
            "LBR-AUTH-002",
        );
        assert_eq!(StableErrorCode::IoReadFailed.as_str(), "LBR-IO-001");
        assert_eq!(StableErrorCode::IoWriteFailed.as_str(), "LBR-IO-002");
        assert_eq!(
            StableErrorCode::InternalInvariant.as_str(),
            "LBR-INTERNAL-001",
        );
        assert_eq!(StableErrorCode::WarningEmitted.as_str(), "LBR-WARN-001");
        assert_eq!(StableErrorCode::AddNothingStaged.as_str(), "LBR-ADD-001");
        assert_eq!(StableErrorCode::Unsupported.as_str(), "LBR-UNSUPPORTED-001",);
        assert_eq!(StableErrorCode::BisectNotActive.as_str(), "LBR-BISECT-001");
        assert_eq!(StableErrorCode::BisectRunFailed.as_str(), "LBR-BISECT-002");
        assert_eq!(
            StableErrorCode::BisectNoCandidates.as_str(),
            "LBR-BISECT-003",
        );
        assert_eq!(
            StableErrorCode::AgentBudgetExceeded.as_str(),
            "LBR-AGENT-001",
        );
        for (variant, code) in [
            (
                StableErrorCode::AgentExternalAgentsDisabled,
                "LBR-AGENT-002",
            ),
            (
                StableErrorCode::AgentProtocolVersionMismatch,
                "LBR-AGENT-003",
            ),
            (StableErrorCode::AgentCapabilityUndeclared, "LBR-AGENT-004"),
            (StableErrorCode::AgentProvenanceRejected, "LBR-AGENT-005"),
            (
                StableErrorCode::AgentBuiltinSlugImpersonation,
                "LBR-AGENT-006",
            ),
            (
                StableErrorCode::AgentIoRedactionSecurityFailure,
                "LBR-AGENT-007",
            ),
            (StableErrorCode::AgentHookEnvelopeInvalid, "LBR-AGENT-008"),
            (
                StableErrorCode::AgentCheckpointStoreInconsistent,
                "LBR-AGENT-009",
            ),
            (StableErrorCode::AgentFixBridgeUnavailable, "LBR-AGENT-010"),
            (
                StableErrorCode::AgentUntrustedSeedForMutation,
                "LBR-AGENT-011",
            ),
            (StableErrorCode::AgentRpcTransportFailed, "LBR-AGENT-012"),
            (StableErrorCode::AgentRawAccessDenied, "LBR-AGENT-013"),
            (StableErrorCode::AgentRunQueueFull, "LBR-AGENT-014"),
        ] {
            assert_eq!(variant.as_str(), code);
        }
    }

    /// Pin the [`CliErrorCategory`] grouping returned by
    /// [`StableErrorCode::category`] for every variant. Categories
    /// drive both `fine_exit_code()` (2..=9) and the inferred
    /// `exit_code()` default (Usage / Warning / Fatal). A silent
    /// re-bucketing — e.g. moving `BisectNotActive` from `Repo` to
    /// `Internal` — would change shell-script exit-code branching
    /// without invalidating any other test.
    ///
    /// Note three deliberate non-obvious groupings worth pinning.
    /// `AddNothingStaged` routes to `Cli` (per `:275`).
    /// `BisectNotActive` and `BisectNoCandidates` route to `Repo`
    /// (per `:276`). `AgentBudgetExceeded` routes to `Internal`
    /// (per `:281`) because an operator-configured budget cap is a
    /// runtime invariant, not a user-input error. Future refactors
    /// that "tidy" these into their lexical bucket will trip this
    /// guard.
    #[test]
    fn stable_error_code_category_pins_each_variant() {
        assert_eq!(
            StableErrorCode::CliUnknownCommand.category(),
            CliErrorCategory::Cli,
        );
        assert_eq!(
            StableErrorCode::CliInvalidArguments.category(),
            CliErrorCategory::Cli,
        );
        assert_eq!(
            StableErrorCode::CliInvalidTarget.category(),
            CliErrorCategory::Cli,
        );
        assert_eq!(
            StableErrorCode::RepoNotFound.category(),
            CliErrorCategory::Repo,
        );
        assert_eq!(
            StableErrorCode::RepoCorrupt.category(),
            CliErrorCategory::Repo,
        );
        assert_eq!(
            StableErrorCode::RepoStateInvalid.category(),
            CliErrorCategory::Repo,
        );
        assert_eq!(
            StableErrorCode::ConfigSchemaFuture.category(),
            CliErrorCategory::Config,
        );
        assert_eq!(
            StableErrorCode::ConflictUnresolved.category(),
            CliErrorCategory::Conflict,
        );
        assert_eq!(
            StableErrorCode::ConflictOperationBlocked.category(),
            CliErrorCategory::Conflict,
        );
        assert_eq!(
            StableErrorCode::NetworkUnavailable.category(),
            CliErrorCategory::Network,
        );
        assert_eq!(
            StableErrorCode::NetworkProtocol.category(),
            CliErrorCategory::Network,
        );
        assert_eq!(
            StableErrorCode::AuthMissingCredentials.category(),
            CliErrorCategory::Auth,
        );
        assert_eq!(
            StableErrorCode::AuthPermissionDenied.category(),
            CliErrorCategory::Auth,
        );
        assert_eq!(
            StableErrorCode::IoReadFailed.category(),
            CliErrorCategory::Io,
        );
        assert_eq!(
            StableErrorCode::IoWriteFailed.category(),
            CliErrorCategory::Io,
        );
        assert_eq!(
            StableErrorCode::InternalInvariant.category(),
            CliErrorCategory::Internal,
        );
        assert_eq!(
            StableErrorCode::WarningEmitted.category(),
            CliErrorCategory::Warning,
        );
        // Deliberate exception per `:275`: AddNothingStaged → Cli.
        assert_eq!(
            StableErrorCode::AddNothingStaged.category(),
            CliErrorCategory::Cli,
        );
        assert_eq!(
            StableErrorCode::Unsupported.category(),
            CliErrorCategory::Internal,
        );
        // Deliberate exception per `:276`: BisectNotActive +
        // BisectNoCandidates → Repo (BisectRunFailed → Internal).
        assert_eq!(
            StableErrorCode::BisectNotActive.category(),
            CliErrorCategory::Repo,
        );
        assert_eq!(
            StableErrorCode::BisectRunFailed.category(),
            CliErrorCategory::Internal,
        );
        assert_eq!(
            StableErrorCode::BisectNoCandidates.category(),
            CliErrorCategory::Repo,
        );
        // Deliberate exception per `:281`: operator-configured cap
        // surfaces under Internal, not under a hypothetical Budget
        // category.
        assert_eq!(
            StableErrorCode::AgentBudgetExceeded.category(),
            CliErrorCategory::Internal,
        );
        // E10 agent codes (LBR-AGENT-002..012) all route to Internal —
        // per docs/development/tracing/agent.md E10 they must exit 128
        // (runtime category), never 129.
        for variant in [
            StableErrorCode::AgentExternalAgentsDisabled,
            StableErrorCode::AgentProtocolVersionMismatch,
            StableErrorCode::AgentCapabilityUndeclared,
            StableErrorCode::AgentProvenanceRejected,
            StableErrorCode::AgentBuiltinSlugImpersonation,
            StableErrorCode::AgentIoRedactionSecurityFailure,
            StableErrorCode::AgentHookEnvelopeInvalid,
            StableErrorCode::AgentCheckpointStoreInconsistent,
            StableErrorCode::AgentFixBridgeUnavailable,
            StableErrorCode::AgentUntrustedSeedForMutation,
            StableErrorCode::AgentRpcTransportFailed,
            StableErrorCode::AgentRawAccessDenied,
        ] {
            assert_eq!(variant.category(), CliErrorCategory::Internal);
        }
    }
}
