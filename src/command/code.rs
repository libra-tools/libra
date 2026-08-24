//! # Code Command — Interactive AI-Powered Coding Sessions
//!
//! This module implements the `libra code` subcommand, which is the primary entry point
//! for AI-agent-driven and human-collaborative development within a Libra repository.
//!
//! ## Architecture Overview
//!
//! The command orchestrates several concurrent subsystems:
//!
//! - **Web Server**: An embedded `axum` HTTP server that serves the Next.js static export
//!   from `web/out/`, providing a browser-based UI alternative.
//! - **MCP Server**: A Model Context Protocol server (using `rmcp`) that exposes Libra's
//!   tools (read, grep, patch, shell, etc.) over Streamable HTTP or Stdio transport,
//!   enabling integration with external AI clients such as Claude Desktop.
//! - **AI Agent**: A tool-calling loop powered by configurable LLM providers (Gemini,
//!   OpenAI, Anthropic, DeepSeek, Kimi, Zhipu, Ollama) or the managed Codex runtime.
//!
//! ## Supported Modes
//!
//! The command supports three mutually exclusive operating modes:
//!
//! | Mode | Flag | Description |
//! |------|------|-------------|
//! | **Web** (default) | *(none)* | Headless web server + MCP; prints URL/control info and waits |
//! | **Stdio** | `--stdio` | MCP server over stdin/stdout for AI client integration |
//!
//! ## Provider Dispatch
//!
//! The `--provider` flag selects the AI backend. The default Web launch builds
//! the provider inside the headless runtime (`build_non_codex_headless_runtime`
//! for generic completion providers, `start_codex_code_ui_runtime` for the
//! managed Codex app-server).
//!
//! ## Sandbox & Approval
//!
//! Tool execution is governed by a layered sandbox and approval system:
//! - **SandboxPolicy**: Controls filesystem and network access (read-only for review/research,
//!   workspace-write for dev mode).
//! - **AskForApproval**: Determines when to prompt the user for tool execution approval
//!   (never, on-failure, on-request, unless-trusted).
//!
//! ## Session Persistence
//!
//! Conversation history is persisted via `SessionStore` under the `.libra/` storage
//! directory, supporting `--resume <thread_id>` to continue a canonical Libra thread.
//!
//! Cross-references for agents extending this command:
//! - Agent workflow and object model: `docs/ai/workflow.md`
//! - MCP split, transport, and object-model notes: `docs/development/mcp.md`
//! - IntentSpec contract examples: `docs/ai/intentspec_typical.yaml`

use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use chrono::Utc;
use clap::{Parser, ValueEnum};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    service::TowerToHyperService,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use serde::{Deserialize, Serialize};
use tokio::{
    process::{Child, Command},
    sync::{mpsc, oneshot},
    time::{Duration, Instant, sleep},
};
use tokio_tungstenite::connect_async;
use url::Url;
use uuid::Uuid;

#[cfg(feature = "test-provider")]
use crate::internal::ai::providers::fake::FAKE_DEFAULT_MODEL;
use crate::{
    cli_error,
    command::code_control_files::{
        CONTROL_INFO_VERSION, ControlInfo, ControlLockError, ControlLockGuard, ControlPaths,
        ControlScope, ControlScopePolicy, acquire_control_lock, cleanup_control_files,
        current_pid_starttime, ensure_control_token_file, ensure_scope_takeover_allowed,
        repo_has_linked_evidence, resolve_control_paths, resolve_control_scope, write_control_info,
    },
    internal::{
        ai::{
            agent::{
                TaskIntent,
                profile::{AgentProfileRouter, AgentsConfig, load_profiles},
            },
            codex as agent_codex,
            completion::{
                CompletionModel, CompletionReasoningEffort, CompletionThinking, CompletionUsage,
            },
            context_budget::ContextBudget,
            history::HistoryManager,
            mcp::server::LibraMcpServer,
            permission::{
                ApprovalRuntimeCacheError, resolve_approval_runtime_cache,
                unbound_approval_cache_scope,
            },
            projection::ThreadBundle,
            prompt::{ContextMode, SystemPromptBuilder},
            providers::{
                anthropic::CLAUDE_3_5_SONNET, gemini::GEMINI_2_5_FLASH, kimi::KIMI_K2_6,
                openai::GPT_4O_MINI, zhipu::GLM_5,
            },
            runtime::{
                CodeAgentApprovalConfig, CodeAgentSandboxProfile, CodeAgentServicesBuilder,
                LifecycleShutdownError, LifecycleShutdownOwner, LifecycleStepError, SecretRedactor,
                lifecycle_resource, tool_runtime_context,
            },
            sandbox::{
                ApprovalCachePolicy, AskForApproval, DEFAULT_APPROVAL_TTL, ExecApprovalRequest,
                ToolRuntimeContext, load_approval_project_config,
            },
            session::{SessionJsonlStore, SessionState, SessionStore},
            tools::{ToolRegistry, context::UserInputRequest},
            usage::{UsageContext, UsagePriceTable, UsageRecorder},
            web::{
                WebServerHandle, WebServerOptions,
                code_ui::{
                    CodeUiCapabilities, CodeUiInitialController, CodeUiInteractionStatus,
                    CodeUiProviderInfo, CodeUiRuntimeHandle, CodeUiRuntimeOptions, CodeUiSession,
                    CodeUiSessionSnapshot, CodeUiSessionStatus, CodeUiTranscriptEntry,
                    CodeUiTranscriptEntryKind, ReadOnlyCodeUiAdapter, initial_snapshot,
                    snapshot_from_thread_bundle,
                },
                code_ui_projection::{
                    MAX_CODE_UI_PROJECTION_EVENTS, MAX_CODE_UI_PROJECTION_REPLAY_BYTES,
                    rebuild_code_ui_read_model_from_events,
                },
                describe_web_bind_error,
                headless::{
                    HeadlessCodeRuntime, HeadlessSessionPersistence, headless_capabilities,
                },
                start as start_web_server,
            },
        },
        db::establish_connection,
        process_terminate::ProcessTerminateGate,
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        pager::LIBRA_TEST_ENV,
        util::{DATABASE, try_get_storage_path},
    },
};

// ---------------------------------------------------------------------------
// Constants — default network ports, bind address, and Codex startup tuning
// ---------------------------------------------------------------------------

/// Default port for the embedded web server serving the Next.js static export.
const DEFAULT_WEB_PORT: u16 = 3000;

/// Default port for the MCP (Model Context Protocol) HTTP server.
const DEFAULT_MCP_PORT: u16 = 6789;

/// Default network interface to bind servers to (localhost only).
const DEFAULT_BIND_HOST: &str = "127.0.0.1";

/// Default executable name for the Codex CLI app-server.
const DEFAULT_CODEX_BIN: &str = "codex";

/// Maximum time to wait for the Codex app-server WebSocket to become reachable.
const CODEX_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

/// Interval between WebSocket connectivity checks during Codex startup.
const CODEX_STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(200);

// ---------------------------------------------------------------------------
// Enums — provider selection, context mode, and approval policy
// ---------------------------------------------------------------------------

/// Available AI provider backends for the `libra code` command.
///
/// Each variant maps to a specific LLM client implementation. The provider
/// determines which API key environment variable is required and which
/// default model is used when `--model` is omitted.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CodeProvider {
    Gemini,
    Openai,
    Anthropic,
    Deepseek,
    Kimi,
    Zhipu,
    Ollama,
    Codex,
    #[cfg(feature = "test-provider")]
    #[value(name = "fake", hide = true)]
    Fake,
}

/// Operating context that shapes the agent's system prompt and sandbox policy.
///
/// - `Dev`: Full read-write access to the workspace; the agent can modify files.
/// - `Review`: Read-only sandbox; the agent focuses on code review feedback.
/// - `Research`: Read-only sandbox; the agent focuses on codebase exploration.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CodeContext {
    #[value(alias = "development")]
    Dev,
    #[value(alias = "code-review")]
    Review,
    #[value(alias = "explore")]
    Research,
}

/// Local automation control mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlMode {
    /// Keep the current loopback-only read behavior; no write token is created.
    Observe,
    /// Enable local automation write control with token and controller checks.
    Write,
    /// Client-only JSON-RPC NDJSON shim (W4-02 / W4-10). Does not start a
    /// Web server, terminal UI, or MCP server.
    /// Discovers endpoint from `--control-info-file` (default `.libra/code/control.json`);
    /// `--control-url` / `--control-token-file` override.
    Stdio,
}

/// Browser write-control posture for `libra code`.
///
/// Controls whether `/api/code/controller/attach` will issue a `Browser`
/// lease (allowing the embedded UI to drive `/messages`,
/// `/interactions/{id}`, and `/control/cancel`). The `--host` is still
/// forced to a loopback address whenever `loopback` is selected — see
/// [`ensure_loopback_browser_control_host`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum BrowserControlMode {
    /// Browser controllers cannot attach. Selected explicitly with
    /// `--browser-control off` (e.g. for non-loopback `--host` binds).
    #[default]
    Off,
    /// Browser controllers may attach as long as the bound `--host` is
    /// loopback. Default for the Web Code UI launch (the default mode).
    Loopback,
}

impl BrowserControlMode {
    /// Returns the canonical wire-format string used in banners, info files,
    /// and audit summaries — matches the clap value names exactly.
    pub fn as_str(self) -> &'static str {
        match self {
            BrowserControlMode::Off => "off",
            BrowserControlMode::Loopback => "loopback",
        }
    }
}

/// Ollama-specific thinking/reasoning mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OllamaThinkingArg {
    /// Let Ollama decide by omitting the `think` field.
    Auto,
    /// Disable thinking for faster local tool-calling responses.
    Off,
    /// Enable thinking without specifying a depth.
    On,
    /// Request low thinking depth.
    Low,
    /// Request medium thinking depth.
    Medium,
    /// Request high thinking depth.
    High,
}

impl From<OllamaThinkingArg> for CompletionThinking {
    fn from(value: OllamaThinkingArg) -> Self {
        match value {
            OllamaThinkingArg::Auto => CompletionThinking::Auto,
            OllamaThinkingArg::Off => CompletionThinking::Disabled,
            OllamaThinkingArg::On => CompletionThinking::Enabled,
            OllamaThinkingArg::Low => CompletionThinking::Low,
            OllamaThinkingArg::Medium => CompletionThinking::Medium,
            OllamaThinkingArg::High => CompletionThinking::High,
        }
    }
}

/// DeepSeek-specific thinking mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum DeepSeekThinkingArg {
    /// Send `thinking: {"type": "enabled"}` to DeepSeek.
    Enabled,
    /// Send `thinking: {"type": "disabled"}` to DeepSeek.
    Disabled,
}

impl From<DeepSeekThinkingArg> for CompletionThinking {
    fn from(value: DeepSeekThinkingArg) -> Self {
        match value {
            DeepSeekThinkingArg::Enabled => CompletionThinking::Enabled,
            DeepSeekThinkingArg::Disabled => CompletionThinking::Disabled,
        }
    }
}

/// Kimi-specific thinking mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum KimiThinkingArg {
    /// Send `thinking: {"type": "enabled"}` to Kimi.
    Enabled,
    /// Send `thinking: {"type": "disabled"}` to Kimi.
    Disabled,
}

impl From<KimiThinkingArg> for CompletionThinking {
    fn from(value: KimiThinkingArg) -> Self {
        match value {
            KimiThinkingArg::Enabled => CompletionThinking::Enabled,
            KimiThinkingArg::Disabled => CompletionThinking::Disabled,
        }
    }
}

/// DeepSeek-specific reasoning effort.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum DeepSeekReasoningEffortArg {
    Low,
    Medium,
    High,
    #[value(alias = "xhigh")]
    Max,
}

impl From<DeepSeekReasoningEffortArg> for CompletionReasoningEffort {
    fn from(value: DeepSeekReasoningEffortArg) -> Self {
        match value {
            DeepSeekReasoningEffortArg::Low => CompletionReasoningEffort::Low,
            DeepSeekReasoningEffortArg::Medium => CompletionReasoningEffort::Medium,
            DeepSeekReasoningEffortArg::High => CompletionReasoningEffort::High,
            DeepSeekReasoningEffortArg::Max => CompletionReasoningEffort::Max,
        }
    }
}

/// User-facing approval policy controlling when tool execution requires
/// explicit human confirmation in the Code UI session.
///
/// This enum is the CLI-facing representation; it converts into the internal
/// [`AskForApproval`] enum via the `From` impl below.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CodeApprovalPolicy {
    /// Never prompt; dangerous commands are rejected.
    Never,
    /// Never prompt; allow every command for this interactive session.
    #[value(
        alias = "allow-all",
        alias = "allow_all",
        alias = "always",
        alias = "accept"
    )]
    AllowAll,
    /// Prompt only when retrying after sandbox denial.
    #[value(alias = "on-failure")]
    OnFailure,
    /// Run inside sandbox by default; prompt when escalation or policy requires it.
    #[value(alias = "on-request")]
    OnRequest,
    /// Prompt for non-trusted operations (safe read commands are auto-allowed).
    #[value(alias = "unless-trusted", alias = "untrusted")]
    Untrusted,
}

/// Developer-selected network access policy for shell/gate execution.
///
/// Only the default `deny` is accepted today: `allow` is rejected in every
/// mode until the Plan network-policy gate owns per-execution sandbox
/// network (approve network in Plan review instead).
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CodeNetworkAccess {
    /// Allow shell and gate tasks to use network access.
    Allow,
    /// Deny network access for shell and gate tasks.
    Deny,
}

impl CodeNetworkAccess {
    fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

impl CodeApprovalPolicy {
    fn allows_all_commands(self) -> bool {
        matches!(self, Self::AllowAll)
    }
}

/// Maps the user-facing [`CodeApprovalPolicy`] to the internal [`AskForApproval`]
/// enum used by the sandbox/approval subsystem.
impl From<CodeApprovalPolicy> for AskForApproval {
    fn from(value: CodeApprovalPolicy) -> Self {
        match value {
            CodeApprovalPolicy::Never => AskForApproval::Never,
            CodeApprovalPolicy::AllowAll => AskForApproval::OnRequest,
            CodeApprovalPolicy::OnFailure => AskForApproval::OnFailure,
            CodeApprovalPolicy::OnRequest => AskForApproval::OnRequest,
            CodeApprovalPolicy::Untrusted => AskForApproval::UnlessTrusted,
        }
    }
}

// ---------------------------------------------------------------------------
// CLI argument definition
// ---------------------------------------------------------------------------

/// `--help` examples shown in `libra code --help` output.
///
/// `code` launches the interactive Libra Code session in one of two
/// modes: Web Code UI (the default) or stdio.
/// The banner pins the most common invocations (Web default, provider
/// selection, `--browser-control loopback`, `--control write`, resume,
/// plan mode, and `--env-file`) so users see the right entry point
/// without reading the design doc. Cross-cutting `--help` EXAMPLES
/// rollout per `docs/development/commands/_general.md` item B.
pub const CODE_EXAMPLES: &str = "\
EXAMPLES:
    libra code                                       Launch the default Web Code UI (browser write lease on)
    libra code --provider deepseek --model deepseek-reasoner
                                                     Pick a provider/model at startup
    libra code --host 0.0.0.0 --browser-control off  Bind all interfaces observe-only / remote notice
    libra code --control write                       Enable local automation write control (token + controller checks)
    libra code --control stdio                       Drive write-control session via control.json discovery
    libra code --control stdio --control-url http://127.0.0.1:3000 --control-token-file .libra/code/control-token
                                                     Explicit endpoint overrides (loopback only)
    libra code --resume <thread-uuid>                Resume a prior canonical thread
    libra code --plan-mode                           Start in plan-only mode (no apply)
    libra code --env-file .env.test                  Load provider keys from a dotenv-style file
    libra code --stdio                               Deprecated MCP-only legacy (tools/resources; not turn control). Prefer --control stdio for automation; dedicated `libra mcp --stdio` is planned after W5";

/// Command-line arguments for `libra code`.
///
/// This struct is parsed by `clap` and drives the operating modes
/// (Web default, stdio). Many flags are
/// mode-specific and validated at runtime by [`validate_mode_args`].
#[derive(Parser, Debug)]
#[command(after_help = CODE_EXAMPLES)]
pub struct CodeArgs {
    /// Port to listen on (web server)
    #[arg(short, long, default_value_t = DEFAULT_WEB_PORT)]
    pub port: u16,

    /// Host address to bind to (web server)
    #[arg(long, default_value = DEFAULT_BIND_HOST)]
    pub host: String,

    /// Working directory for the code session (default: current directory)
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,

    /// Path to a Libra repository (default: discover from current directory)
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,

    /// Load provider environment variables from a dotenv-style file.
    ///
    /// Values in this file take precedence over already exported process
    /// environment variables for provider bootstrap.
    #[arg(long = "env-file", value_name = "PATH")]
    pub env_file: Option<PathBuf>,

    /// Local automation control mode (`observe` | `write` | `stdio`).
    ///
    /// `observe` / `write` configure the Web launch control sidecar.
    /// `stdio` is a client-only JSON-RPC NDJSON shim (no Web/MCP launch).
    #[arg(long, value_enum, default_value_t = ControlMode::Observe)]
    pub control: ControlMode,

    /// Browser write-control posture (`off` | `loopback`).
    ///
    /// Defaults are mode-specific:
    /// - Web launch (the default) → `loopback`
    /// - `--stdio` (MCP transport) → `off` (the flag conflicts with `--stdio`)
    ///
    /// Selecting `loopback` is rejected when `--host` is not a loopback
    /// address, and the flag is incompatible with `--stdio`. Use
    /// `--browser-control off` when binding a non-loopback `--host` for
    /// observe-only / remote-notice serving.
    #[arg(long = "browser-control", value_enum, conflicts_with = "stdio")]
    pub browser_control: Option<BrowserControlMode>,

    /// Path to the local automation control token file
    #[arg(long, value_name = "PATH")]
    pub control_token_file: Option<PathBuf>,

    /// Path to the local automation control discovery info file
    ///
    /// For `--control write`/`observe`, this is the write path for non-secret
    /// endpoint metadata. For `--control stdio`, this is the read path used to
    /// discover `baseUrl` (default `.libra/code/control.json`); explicit
    /// `--control-url` overrides the discovered URL.
    #[arg(long, value_name = "PATH")]
    pub control_info_file: Option<PathBuf>,

    /// Base URL of an existing Code UI control endpoint (W4-02 / W4-10).
    ///
    /// Optional with `--control stdio`: when omitted, the URL is read from
    /// `--control-info-file` (default `.libra/code/control.json`). Example:
    /// `http://127.0.0.1:3000`.
    #[arg(long = "control-url", value_name = "URL")]
    pub control_url: Option<String>,

    /// AI provider backend
    #[arg(long, value_enum, default_value_t = CodeProvider::Gemini)]
    pub provider: CodeProvider,

    /// Model id (provider-specific)
    #[arg(long)]
    pub model: Option<String>,

    /// Sampling temperature (provider-specific range, typically 0.0–2.0)
    #[arg(long, value_name = "FLOAT")]
    pub temperature: Option<f64>,

    /// Ollama thinking mode: auto, off, on, low, medium, or high.
    ///
    /// If omitted, Ollama uses OLLAMA_THINK and then defaults to `off`.
    #[arg(long = "ollama-thinking", alias = "thinking", value_enum)]
    pub ollama_thinking: Option<OllamaThinkingArg>,

    /// Send compact Ollama tool schemas for providers that reject complex JSON schemas.
    #[arg(long = "ollama-compact-tools")]
    pub ollama_compact_tools: bool,

    /// DeepSeek thinking mode: enabled or disabled.
    #[arg(long = "deepseek-thinking", value_enum)]
    pub deepseek_thinking: Option<DeepSeekThinkingArg>,

    /// DeepSeek reasoning effort: low, medium, high, or max.
    #[arg(long = "deepseek-reasoning-effort", value_enum)]
    pub deepseek_reasoning_effort: Option<DeepSeekReasoningEffortArg>,

    /// DeepSeek stream mode: true or false.
    #[arg(long = "deepseek-stream", alias = "stream", value_name = "BOOL")]
    pub deepseek_stream: Option<bool>,

    /// Kimi thinking mode: enabled or disabled.
    #[arg(long = "kimi-thinking", value_enum)]
    pub kimi_thinking: Option<KimiThinkingArg>,

    /// Kimi stream mode: true or false. Defaults to true for Kimi.
    #[arg(long = "kimi-stream", value_name = "BOOL")]
    pub kimi_stream: Option<bool>,

    /// Select an agent profile by name. When the profile carries a structured
    /// `model: provider/model[@variant]` binding, the agent's binding wins
    /// atomically — provider, model id, and variant all come from the
    /// agent's spec, and a separately-supplied `--model` is ignored to avoid
    /// hybrid pairs (anthropic provider + OpenAI-shaped model id). Profiles
    /// without a structured binding fall back to the CLI defaults verbatim.
    /// Profiles are looked up via the same three-tier hierarchy used elsewhere
    /// (project `.libra/agents/`, user `~/.config/libra/agents/`, embedded).
    #[arg(long = "agent", value_name = "NAME")]
    pub agent: Option<String>,

    /// Test-only fake provider fixture.
    #[cfg(feature = "test-provider")]
    #[arg(long = "fake-fixture", hide = true, value_name = "PATH")]
    pub fake_fixture: Option<PathBuf>,

    /// Operating context mode (dev, review, research)
    #[arg(long, value_enum)]
    pub context: Option<CodeContext>,

    /// Resume a canonical Libra thread by UUID
    #[arg(long, value_name = "THREAD_UUID")]
    pub resume: Option<String>,

    /// Tool approval policy:
    /// - `never`: no prompts, dangerous commands are rejected
    /// - `allow-all`: no prompts, all commands are allowed for this session
    /// - `on-failure`: prompt only for retry outside sandbox after sandbox denial
    /// - `on-request`: run sandboxed by default; prompt for escalation/policy-required cases
    /// - `untrusted`: prompt for non-trusted operations, auto-allow known-safe reads
    #[arg(long, value_enum, default_value_t = CodeApprovalPolicy::OnRequest)]
    pub approval_policy: CodeApprovalPolicy,

    /// Seconds that a TTL approval remains reusable for matching commands.
    #[arg(long = "approval-ttl", value_name = "SECS")]
    pub approval_ttl: Option<u64>,

    /// Network access policy for shell and gate execution (`allow` is
    /// rejected in every mode until the Plan network-policy gate owns
    /// per-execution sandbox network).
    #[arg(long, value_enum, default_value_t = CodeNetworkAccess::Deny)]
    pub network_access: CodeNetworkAccess,

    /// Port for the embedded MCP server to listen on
    #[arg(long, value_name = "PORT", default_value_t = DEFAULT_MCP_PORT)]
    pub mcp_port: u16,

    /// Deprecated MCP-only legacy stdio transport (tools/resources; not turn control).
    /// Prefer `libra code --control stdio` for automation; a dedicated
    /// `libra mcp --stdio` is planned after W5 (DEFER-02).
    #[arg(long, alias = "mcp-stdio")]
    pub stdio: bool,

    /// Provider API base URL.
    ///
    /// For Ollama, use a local/remote daemon URL such as
    /// `http://remote-host:11434/v1`, or `https://ollama.com` for direct
    /// Ollama Cloud API access with `OLLAMA_API_KEY`.
    #[arg(long, value_name = "URL")]
    pub api_base: Option<String>,

    /// Codex executable used to launch the managed app-server
    #[arg(long, value_name = "PATH", default_value = DEFAULT_CODEX_BIN)]
    pub codex_bin: String,

    /// Override the Codex app-server port (default: random local free port)
    #[arg(long, value_name = "PORT")]
    pub codex_port: Option<u16>,

    /// Codex plan-first mode: require an approved plan before execution.
    ///
    /// When `--provider=codex`, this defaults to ON so the session
    /// follows `docs/ai/workflow.md` Phase 0/1 (read-only intent &
    /// plan drafting) before Phase 2 execution. Pass `--plan-mode=false` to
    /// opt out for a single session. For non-Codex providers, omit the flag —
    /// Libra drives Phase 0/1 through its own tool loop.
    ///
    /// Accepted forms:
    /// `--plan-mode` (alias for `=true`), `--plan-mode=true`, `--plan-mode=false`.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub plan_mode: Option<bool>,

    /// Goal-mode objective. When set, the session boots with an
    /// active Goal whose objective is the supplied string; the
    /// supervisor (P6.3) drives the tool loop until completion is
    /// claimed and the verifier (P6.2) accepts. Equivalent to
    /// invoking `/goal start <objective>` immediately after the
    /// session opens.
    ///
    /// The objective is validated up-front against the same shape
    /// rules `GoalSpec::new` applies — non-empty after trim, ≤ 16
    /// KiB. A bad objective fails CLI parsing rather than crashing
    /// the supervisor at startup.
    #[arg(long = "goal", value_name = "OBJECTIVE")]
    pub goal: Option<String>,
}

/// Resolves the effective `plan_mode` flag for the current invocation.
///
/// Returns the user-supplied value when present; otherwise defaults to
/// `true` for the Codex provider and `false` for other providers.
///
/// **Scope of enforcement:** `plan_mode` is forwarded to Codex's
/// `developerInstructions` / `baseInstructions` and tells Codex's own agent
/// loop to produce a structured plan and wait for an approval before
/// executing. The approval gate is therefore **Codex's own approval channel**
/// (per-tool / per-command requests), not Libra's Phase 0 / Phase 1 review
/// loop. Libra's own intent / plan drafting tool loop is defined by
/// `runtime::phase0::phase0_plan_tool_loop_config` and
/// `runtime::phase1::phase1_plan_tool_loop_config`. It requires a generic
/// `CompletionModel`; managed Codex uses its app-server backend instead of
/// that generic workflow.
///
/// Combining `--plan-mode=true` with `--approval-policy=allow-all` /
/// `=never` means Codex still produces the plan, but its approval gate is
/// auto-approved — the operator sees the plan in the transcript / log but
/// is never asked to confirm. `start_codex_code_ui_runtime` emits a
/// `tracing::warn!` when this combination is detected so the operator can
/// notice that the review gate has been disabled.
pub(crate) fn effective_plan_mode(args: &CodeArgs) -> bool {
    args.plan_mode
        .unwrap_or(matches!(args.provider, CodeProvider::Codex))
}

// ---------------------------------------------------------------------------
// Top-level entry point — mode dispatch
// ---------------------------------------------------------------------------

/// True when this invocation runs the Web Code UI path (the default), as
/// opposed to `--stdio`. Bare `--provider codex --resume` no longer reaches a
/// launch path: the legacy TUI resume driver was removed in W5-06 and
/// `validate_mode_args` rejects the combination with a migration hint.
fn code_uses_web_launch(args: &CodeArgs) -> bool {
    !args.stdio
}

/// Stderr pin for W4-03: MCP `--stdio` is legacy tools/resources only (C6), not
/// live turn control. Dedicated `libra mcp --stdio` is DEFER-02 (after W5).
pub const MCP_STDIO_DEPRECATION_WARNING: &str = "warning: `libra code --stdio` is a deprecated MCP-only legacy entry (tools/resources only; not live turn control). Prefer `libra code --control stdio` for automation; a dedicated `libra mcp --stdio` is planned after W5";

fn warn_deprecated_mcp_stdio() {
    eprintln!("{MCP_STDIO_DEPRECATION_WARNING}");
}

/// Entry point for the `libra code` subcommand.
///
/// Validates CLI flag combinations, then dispatches to:
/// - `--control stdio`: JSON-RPC NDJSON automation client (no server launch)
/// - `--stdio`: MCP over stdin/stdout
/// - default: Web Code UI + AgentRuntime
///
/// # Side Effects
/// - May start local web, MCP, and Codex app-server processes depending on mode.
/// - May create `.libra/objects` and connect to `.libra/libra.db` for history.
/// - In Web mode, prints URL / control details and waits for SIGINT/SIGTERM.
/// - In Web mode, tools may mutate the workspace through the headless AgentRuntime,
///   subject to sandbox and approval policy.
/// - In MCP stdio mode, owns stdin/stdout for the MCP session.
/// - In `--control stdio` mode, owns stdin/stdout for JSON-RPC NDJSON only.
///
/// # Errors
/// Returns [`CliError`] for invalid mode combinations, provider credential
/// failures, network bind failures, Codex app-server startup failures, or
/// terminal/session initialization failures. Error classification follows
/// `docs/development/cli-error-contract-design.md`.
pub async fn execute(args: CodeArgs, output: &OutputConfig) -> CliResult<()> {
    // Client-only control shim (W4-02) + control-info discovery (W4-10): no
    // worktree gate, no Web/MCP boot, no auto-start / port scan.
    if args.control == ControlMode::Stdio {
        validate_mode_args(&args, output).map_err(CliError::command_usage)?;
        let working_dir = std::env::current_dir().map_err(|error| {
            CliError::fatal(format!(
                "cannot resolve the current working directory: {error}"
            ))
            .with_stable_code(crate::utils::error::StableErrorCode::IoReadFailed)
        })?;
        let discovered = crate::command::code_control_files::discover_control_stdio_endpoint(
            &working_dir,
            args.control_url.as_deref(),
            args.control_token_file.as_deref(),
            args.control_info_file.as_deref(),
        )
        .await
        .map_err(|error| {
            use serde_json::json;

            use crate::utils::error::StableErrorCode;
            let wire = error.code();
            let code = match wire {
                "CONTROL_TOKEN_PERMS" | "CONTROL_INFO_PERMS" => {
                    StableErrorCode::AuthPermissionDenied
                }
                "CONTROL_SCOPE_CONFLICT" => StableErrorCode::ConflictOperationBlocked,
                "CONTROL_SERVER_MISSING" => StableErrorCode::NetworkUnavailable,
                _ => StableErrorCode::CliInvalidTarget,
            };
            // Preserve CONTROL_* in structured details so `--machine` clients
            // can key off the same identifiers as JSON-RPC attach `data.code`.
            CliError::fatal(error.to_string())
                .with_stable_code(code)
                .with_detail("code", json!(wire))
        })?;
        return crate::command::code_control::run_control_stdio_client(
            &discovered.base_url,
            &discovered.token_file,
        )
        .await;
    }

    // W4-08: linked worktrees launch through the W4-06/W4-11/W4-12 resolver
    // and W4-07/W4-13 approval ownership. Still resolve the session workdir
    // before mode validation so `--cwd`/`--repo` cannot silently retarget,
    // and fail-close when that target is unregistered or unreadable.
    let session_workdir = resolve_code_preflight_working_dir(&args)?;
    crate::command::require_registered_worktree_scope("libra code", &session_workdir)?;
    validate_mode_args(&args, output).map_err(CliError::command_usage)?;
    if args.stdio {
        execute_stdio(&args).await
    } else {
        execute_web_only(&args).await
    }
}

// ---------------------------------------------------------------------------
// Server handles — RAII wrappers for graceful shutdown
// ---------------------------------------------------------------------------

/// Handle to a running MCP server.
///
/// In addition to the shared shutdown mechanism, this tracks individual
/// per-connection tasks so they can be aborted during shutdown — preventing
/// leaked tasks when the server is torn down.
struct McpServerHandle {
    addr: SocketAddr,
    shutdown_tx: oneshot::Sender<()>,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
    /// Tracks spawned per-connection Hyper service tasks for cleanup.
    connection_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

const MCP_SERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Outer deadline for the process-level [`LifecycleShutdownOwner`] that
/// sequences runtime, lease, listeners, managed child, and control cleanup.
const CODE_LIFECYCLE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
enum McpServerShutdownError {
    #[error("mcp_server did not stop before the shutdown deadline")]
    TimedOut,
    #[error("mcp_server task exited unexpectedly during shutdown: {reason}")]
    TaskFailed { reason: String },
}

impl McpServerHandle {
    async fn shutdown(self) -> Result<(), McpServerShutdownError> {
        self.shutdown_with_timeout(MCP_SERVER_SHUTDOWN_TIMEOUT)
            .await
    }

    async fn shutdown_with_timeout(
        self,
        shutdown_timeout: Duration,
    ) -> Result<(), McpServerShutdownError> {
        let _ = self.shutdown_tx.send(());
        let pending = match self.connection_tasks.lock() {
            Ok(mut handles) => std::mem::take(&mut *handles),
            Err(_) => Vec::new(),
        };
        for handle in pending {
            handle.abort();
        }

        // Abort the listener if an outer lifecycle deadline cancels this future
        // before the local timeout can call `join.abort()`.
        let mut join = McpAbortJoinOnDrop {
            handle: Some(self.join),
        };
        match tokio::time::timeout(shutdown_timeout, join.as_mut()).await {
            Ok(Ok(Ok(()))) => {
                join.disarm();
                Ok(())
            }
            Ok(Ok(Err(error))) => {
                join.disarm();
                Err(McpServerShutdownError::TaskFailed {
                    reason: error.to_string(),
                })
            }
            Ok(Err(error)) => {
                join.disarm();
                Err(McpServerShutdownError::TaskFailed {
                    reason: error.to_string(),
                })
            }
            Err(_) => {
                join.abort_now().await;
                Err(McpServerShutdownError::TimedOut)
            }
        }
    }
}

struct McpAbortJoinOnDrop {
    handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl McpAbortJoinOnDrop {
    fn as_mut(&mut self) -> &mut tokio::task::JoinHandle<anyhow::Result<()>> {
        // INVARIANT: handle is present until `disarm` / `abort_now`.
        self.handle
            .as_mut()
            .expect("McpAbortJoinOnDrop used after disarm")
    }

    fn disarm(&mut self) {
        self.handle.take();
    }

    async fn abort_now(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

impl Drop for McpAbortJoinOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

fn lifecycle_shutdown_cli_error(error: LifecycleShutdownError) -> CliError {
    match error {
        LifecycleShutdownError::TimedOut {
            unreleased_resources,
        } => CliError::failure(format!(
            "Libra Code did not shut down cleanly before the deadline; unreleased resources: {unreleased_resources:?}"
        )),
        LifecycleShutdownError::Failed {
            failed_resources,
            detail,
        } => CliError::failure(format!(
            "Libra Code failed to release {failed_resources:?} during shutdown: {detail}"
        )),
    }
}

async fn push_code_ui_lifecycle_step(
    owner: &LifecycleShutdownOwner,
    runtime: Arc<CodeUiRuntimeHandle>,
) {
    owner
        .push_step(lifecycle_resource::RUNTIME_TURN, async move {
            runtime.shutdown_for_lifecycle().await
        })
        .await;
}

async fn push_controller_lease_lifecycle_step(
    owner: &LifecycleShutdownOwner,
    runtime: Arc<CodeUiRuntimeHandle>,
) {
    owner
        .push_step(lifecycle_resource::CONTROLLER_LEASE, async move {
            runtime.release_controller_for_lifecycle().await;
            Ok(())
        })
        .await;
}

async fn push_web_server_lifecycle_step(owner: &LifecycleShutdownOwner, handle: WebServerHandle) {
    owner
        .push_step(lifecycle_resource::WEB_SERVER, async move {
            match handle.shutdown().await {
                Ok(()) => Ok(()),
                Err(crate::internal::ai::web::WebServerShutdownError::TimedOut) => {
                    Err(LifecycleStepError::timed_out())
                }
                Err(error) => Err(LifecycleStepError::failed(error.to_string())),
            }
        })
        .await;
}

async fn push_mcp_server_lifecycle_step(owner: &LifecycleShutdownOwner, handle: McpServerHandle) {
    owner
        .push_step(lifecycle_resource::MCP_SERVER, async move {
            match handle.shutdown().await {
                Ok(()) => Ok(()),
                Err(McpServerShutdownError::TimedOut) => Err(LifecycleStepError::timed_out()),
                Err(error) => Err(LifecycleStepError::failed(error.to_string())),
            }
        })
        .await;
}

async fn push_managed_codex_lifecycle_step(
    owner: &LifecycleShutdownOwner,
    mut server: ManagedCodexServer,
) {
    owner
        .push_step(lifecycle_resource::MANAGED_CODEX_CHILD, async move {
            match server.shutdown().await {
                Ok(()) => Ok(()),
                Err(ManagedCodexShutdownError::TimedOut) => Err(LifecycleStepError::timed_out()),
                Err(error) => Err(LifecycleStepError::failed(error.to_string())),
            }
        })
        .await;
}

async fn push_control_runtime_lifecycle_step(
    owner: &LifecycleShutdownOwner,
    control_runtime: ControlRuntimeConfig,
) {
    owner
        .push_step(lifecycle_resource::CONTROL_LOCK, async move {
            control_runtime.cleanup();
            // Prevent Drop from double-cleaning; cleanup is idempotent on disk
            // but we still forget the guard's Drop path by leaking... actually
            // Drop also calls cleanup which is fine/idempotent. Just drop.
            drop(control_runtime);
            Ok(())
        })
        .await;
}

/// Build and run the shared process shutdown owner. Order matches W1-08:
/// stop admitting / finalize runtime turns, then listeners, then managed
/// child, then control lock / temp files (via [`ControlRuntimeConfig`] Drop).
async fn push_local_runtime_lifecycle_step(
    owner: &LifecycleShutdownOwner,
    runtime: crate::internal::ai::runtime::AgentRuntimeHandle,
    worker_task: Option<tokio::task::JoinHandle<()>>,
) {
    owner
        .push_step(lifecycle_resource::RUNTIME_TURN, async move {
            match runtime.shutdown().await {
                Ok(()) => {
                    if let Some(task) = worker_task {
                        let _ = task.await;
                    }
                    Ok(())
                }
                Err(crate::internal::ai::runtime::RuntimeShutdownError::TimedOut {
                    unreleased_resources,
                }) => {
                    if let Some(task) = worker_task {
                        task.abort();
                        let _ = task.await;
                    }
                    Err(LifecycleStepError::timed_out_with(unreleased_resources))
                }
                Err(error) => {
                    if let Some(task) = worker_task {
                        task.abort();
                        let _ = task.await;
                    }
                    Err(LifecycleStepError::failed_with(
                        [lifecycle_resource::RUNTIME_TURN],
                        error.to_string(),
                    ))
                }
            }
        })
        .await;
}

async fn shutdown_code_lifecycle(
    code_ui: Option<Arc<CodeUiRuntimeHandle>>,
    local_runtime: Option<(
        crate::internal::ai::runtime::AgentRuntimeHandle,
        Option<tokio::task::JoinHandle<()>>,
    )>,
    web: Option<WebServerHandle>,
    mcp: Option<McpServerHandle>,
    managed_codex: Option<ManagedCodexServer>,
    control_runtime: Option<ControlRuntimeConfig>,
) -> Result<(), LifecycleShutdownError> {
    let owner = LifecycleShutdownOwner::with_timeout(CODE_LIFECYCLE_SHUTDOWN_TIMEOUT);
    if let Some((runtime, worker_task)) = local_runtime {
        push_local_runtime_lifecycle_step(&owner, runtime, worker_task).await;
    }
    let code_ui_for_lease = code_ui.clone();
    if let Some(runtime) = code_ui {
        push_code_ui_lifecycle_step(&owner, runtime).await;
    }
    if let Some(handle) = web {
        push_web_server_lifecycle_step(&owner, handle).await;
    }
    if let Some(handle) = mcp {
        push_mcp_server_lifecycle_step(&owner, handle).await;
    }
    if let Some(server) = managed_codex {
        push_managed_codex_lifecycle_step(&owner, server).await;
    }
    if let Some(runtime) = code_ui_for_lease {
        push_controller_lease_lifecycle_step(&owner, runtime).await;
    }
    if let Some(control_runtime) = control_runtime {
        push_control_runtime_lifecycle_step(&owner, control_runtime).await;
    }
    owner.shutdown().await
}

// ---------------------------------------------------------------------------
// Mode: Web-only — headless web + MCP servers (no terminal UI)
// ---------------------------------------------------------------------------

/// Which Code UI runtime the default Web launch dispatches to, decided
/// purely from the selected provider.
///
/// This is the single source of truth for the provider branch in
/// [`execute_web_only`]. The exhaustive match in [`web_only_runtime_kind`]
/// means a newly added [`CodeProvider`] variant forces a compile-time routing
/// decision here instead of silently falling through to a default. Per-provider
/// reachability is pinned by the `web_only_runtime_kind_routes_*` unit tests so
/// the Task C2 validation relaxation — which now lets every provider reach this
/// dispatch — cannot regress into a misrouted or unreachable runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebOnlyRuntimeKind {
    /// Codex → managed app-server child process + `start_codex_code_ui_runtime`.
    ManagedCodexAppServer,
    /// Every other accepted provider → `HeadlessCodeRuntime` via
    /// `build_non_codex_headless_runtime` (falling back to the read-only
    /// placeholder only if that dispatcher declines the provider).
    Headless,
}

/// Classify the web-only runtime for `provider`. See [`WebOnlyRuntimeKind`].
fn web_only_runtime_kind(provider: CodeProvider) -> WebOnlyRuntimeKind {
    match provider {
        CodeProvider::Codex => WebOnlyRuntimeKind::ManagedCodexAppServer,
        CodeProvider::Gemini
        | CodeProvider::Openai
        | CodeProvider::Anthropic
        | CodeProvider::Deepseek
        | CodeProvider::Kimi
        | CodeProvider::Zhipu
        | CodeProvider::Ollama => WebOnlyRuntimeKind::Headless,
        #[cfg(feature = "test-provider")]
        CodeProvider::Fake => WebOnlyRuntimeKind::Headless,
    }
}

/// Best-effort open of the Code UI URL in the system browser.
///
/// Never fails the process: missing display, spawn errors, and `LIBRA_TEST`
/// all leave the printed URL + control info as the operator surface.
///
/// When the URL embeds a browser bootstrap secret (`?bt=`), auto-open is
/// skipped so the secret never appears in opener argv (`/proc/*/cmdline` on
/// shared hosts). Operators should open the printed URL themselves.
fn try_open_code_ui_browser(url: &str) {
    if std::env::var_os(LIBRA_TEST_ENV).is_some() {
        return;
    }
    if url_contains_browser_bootstrap_query(url) {
        eprintln!(
            "note: open the printed Code UI URL in your browser (auto-open skipped so the bootstrap token is not exposed on the opener command line)"
        );
        return;
    }
    let result = (|| -> std::io::Result<()> {
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .spawn()?;
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open").arg(url).spawn()?;
        }
        #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
        {
            std::process::Command::new("xdg-open").arg(url).spawn()?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!(
            "warning: could not open a browser for {url} ({error}); use the URL above (Web UI stays running)"
        );
    }
}

fn url_contains_browser_bootstrap_query(url: &str) -> bool {
    url.split_once('?')
        .map(|(_, query)| {
            query
                .split('&')
                .any(|pair| pair == "bt" || pair.starts_with("bt="))
        })
        .unwrap_or(false)
}

/// Runs the web server and MCP server without a terminal UI.
///
/// Blocks on SIGINT or SIGTERM, then performs graceful shutdown of both servers.
/// This mode is useful for remote/headless environments where the user
/// interacts through a browser or external MCP client.
///
/// # Side Effects
/// - Starts the embedded web server and Streamable HTTP MCP server.
/// - For the Codex provider, starts and later shuts down a managed Codex
///   app-server child process.
/// - Prints connection details to stdout and listens for SIGINT/SIGTERM.
/// - Best-effort opens a system browser for the Code UI URL (never fails closed).
///
/// # Errors
/// Returns [`CliError`] when the working directory cannot be resolved, the web
/// or MCP listener cannot bind, the Codex app-server fails to start, or the
/// selected host would expose loopback-only browser control.
async fn execute_web_only(args: &CodeArgs) -> CliResult<()> {
    let working_dir = resolve_code_working_dir(args)?;
    // Keep provider bootstrap on the same env-file → process → Vault lookup
    // chain as the shared Code runtime. The current Web-only flag policy may reject a
    // non-default file, but that policy no longer creates a second factory.
    let env_file = load_code_env_file(args.env_file.as_deref())?;
    let browser_control = resolve_browser_control_mode(args)?;
    // Arm SIGINT/SIGTERM before spawning managed Codex or binding listeners.
    let process_terminate = ProcessTerminateGate::install().map_err(|error| {
        CliError::failure(format!(
            "failed to install the web-only process terminate listener: {error}"
        ))
    })?;
    let check_process_terminate = |gate: &ProcessTerminateGate| -> CliResult<()> {
        if gate.is_signaled() {
            Err(CliError::failure(
                "received a terminate signal while starting Libra Code web-only mode; shutting down"
                    .to_string(),
            ))
        } else {
            Ok(())
        }
    };
    let control_runtime = prepare_control_runtime(args, &working_dir).await?;
    let mcp_server = init_mcp_server(&working_dir).await;

    let mut managed_codex_server = ManagedCodexBootstrapGuard::new(None);
    let code_ui_runtime =
        if web_only_runtime_kind(args.provider) == WebOnlyRuntimeKind::ManagedCodexAppServer {
            let server =
                start_managed_codex_server(&args.codex_bin, args.codex_port, &working_dir).await?;
            if let Err(error) = check_process_terminate(&process_terminate) {
                let shutdown_error =
                    shutdown_code_lifecycle(None, None, None, None, Some(server), None)
                        .await
                        .err();
                if let Some(shutdown_error) = shutdown_error {
                    return Err(error.with_detail("shutdown", shutdown_error.to_string()));
                }
                return Err(error);
            }
            println!("Starting Libra Code Web UI with Codex provider");
            println!("Working directory: {}", working_dir.display());
            println!("Codex WebSocket: {}", server.ws_url);
            println!("Codex app-server: auto-started");
            println!("{}", browser_control_banner_line(browser_control));
            let ws_url = server.ws_url.clone();
            managed_codex_server = ManagedCodexBootstrapGuard::new(Some(server));

            start_codex_code_ui_runtime(
                args,
                &working_dir,
                &ws_url,
                mcp_server.clone(),
                browser_control == BrowserControlMode::Loopback,
                CodeUiInitialController::Unclaimed,
            )
            .await?
        } else {
            // §C.4.1: refuse rather than mint a phantom `<working_dir>/.libra`.
            let storage_root = require_storage_root(&working_dir)?;
            let session_store = Arc::new(SessionStore::from_storage_path(&storage_root));
            session_store
                .rebuild_thread_session_index()
                .map_err(|error| {
                    CliError::io(format!(
                        "failed to rebuild the Code thread→session index under '{}': {error}",
                        storage_root.display()
                    ))
                })?;
            let session_state =
                load_or_create_headless_web_session_state(args, &working_dir, &session_store)?;
            // All accepted non-Codex web-only providers now route through the
            // headless runtime (C2 relaxed the web-only provider gate).
            // Construction errors propagate via `?`; the read-only placeholder
            // below is only the `Ok(None)` (not-wired) fallback — reached when
            // the builder declines a provider (today only `Codex`, which is
            // routed away before this branch), so it is defensive fail-closed
            // code rather than a live path.
            match build_non_codex_headless_runtime(
                args,
                &working_dir,
                &env_file,
                session_store,
                session_state,
                browser_control == BrowserControlMode::Loopback,
                mcp_server.clone(),
            )
            .await?
            {
                Some(runtime) => {
                    println!("Starting Libra Code Web UI in headless mode");
                    println!("Working directory: {}", working_dir.display());
                    println!("Provider: {:?}", args.provider);
                    println!("{}", browser_control_banner_line(browser_control));
                    runtime
                }
                None => build_placeholder_web_code_ui_runtime(args, &working_dir).await,
            }
        };
    mcp_server.set_code_ui_session(code_ui_runtime.adapter().session());

    if let Err(error) = check_process_terminate(&process_terminate) {
        let shutdown_error = shutdown_code_lifecycle(
            Some(code_ui_runtime),
            None,
            None,
            None,
            managed_codex_server.take(),
            Some(control_runtime),
        )
        .await
        .err();
        if let Some(shutdown_error) = shutdown_error {
            return Err(error.with_detail("shutdown", shutdown_error.to_string()));
        }
        return Err(error);
    }

    let browser_bootstrap_token = mint_browser_bootstrap_token(browser_control);
    let web_handle = match start_web_server(
        &args.host,
        args.port,
        working_dir.clone(),
        WebServerOptions {
            code_ui: Some(code_ui_runtime.clone()),
            automation_control_token: control_runtime.token.clone(),
            browser_bootstrap_token: browser_bootstrap_token.clone(),
            audit_sink: None,
            secret_redactor: Some(projection_secret_redactor(&env_file)),
            workflow_hub: None,
        },
    )
    .await
    {
        Ok(handle) => handle,
        Err(err) => {
            let shutdown_error = shutdown_code_lifecycle(
                Some(code_ui_runtime),
                None,
                None,
                None,
                managed_codex_server.take(),
                Some(control_runtime),
            )
            .await
            .err();
            let mut error = CliError::network(describe_web_bind_error(&args.host, args.port, &err))
                .with_detail("component", "web_server");
            if let Some(shutdown_error) = shutdown_error {
                error = error.with_detail("shutdown", shutdown_error.to_string());
            }
            return Err(error);
        }
    };
    let base_url = format!("http://{}", web_handle.addr);
    let open_url = code_ui_url_with_bootstrap(&base_url, browser_bootstrap_token.as_ref());
    let thread_id = code_ui_runtime.snapshot().await.thread_id;
    if let Err(error) =
        control_runtime.write_info_file(&working_dir, base_url.clone(), None, thread_id.clone())
    {
        let shutdown_error = shutdown_code_lifecycle(
            Some(code_ui_runtime),
            None,
            Some(web_handle),
            None,
            managed_codex_server.take(),
            Some(control_runtime),
        )
        .await
        .err();
        if let Some(shutdown_error) = shutdown_error {
            return Err(error.with_detail("shutdown", shutdown_error.to_string()));
        }
        return Err(error);
    }
    println!("Libra Code server running at {open_url}");
    if let Some(token) = browser_bootstrap_token.as_ref() {
        // Printed for harness discovery and operators; also embedded as `?bt=`
        // on the open URL so the SPA can send X-Libra-Browser-Bootstrap.
        println!("Browser bootstrap token: {token}");
    }
    // Best-effort browser open (W4-01): never fail the process; no-TTY / missing
    // display / LIBRA_TEST keep the printed URL + control info and stay resident.
    try_open_code_ui_browser(&open_url);

    // Start MCP Server
    let mcp_handle = match start_mcp_server(&args.host, args.mcp_port, mcp_server.clone()).await {
        Ok(handle) => {
            let mcp_url = format!("http://{}", handle.addr);
            if let Err(error) = control_runtime.write_info_file(
                &working_dir,
                base_url.clone(),
                Some(mcp_url.clone()),
                thread_id.clone(),
            ) {
                let shutdown_error = shutdown_code_lifecycle(
                    Some(code_ui_runtime),
                    None,
                    Some(web_handle),
                    Some(handle),
                    managed_codex_server.take(),
                    Some(control_runtime),
                )
                .await
                .err();
                if let Some(shutdown_error) = shutdown_error {
                    return Err(error.with_detail("shutdown", shutdown_error.to_string()));
                }
                return Err(error);
            }
            println!("MCP: {mcp_url}");
            handle
        }
        Err(err) => {
            let shutdown_error = shutdown_code_lifecycle(
                Some(code_ui_runtime),
                None,
                Some(web_handle),
                None,
                managed_codex_server.take(),
                Some(control_runtime),
            )
            .await
            .err();
            let mut error = CliError::network(format!("failed to start MCP server: {err}"))
                .with_detail("component", "mcp_server");
            if let Some(shutdown_error) = shutdown_error {
                error = error.with_detail("shutdown", shutdown_error.to_string());
            }
            return Err(error);
        }
    };

    process_terminate.wait().await;
    if let Err(error) = shutdown_code_lifecycle(
        Some(code_ui_runtime),
        None,
        Some(web_handle),
        Some(mcp_handle),
        managed_codex_server.take(),
        Some(control_runtime),
    )
    .await
    {
        return Err(lifecycle_shutdown_cli_error(error));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared bootstrap helpers — env files, credentials, provider factories
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct CodeEnvFile {
    values: BTreeMap<String, String>,
}

impl CodeEnvFile {
    fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

fn load_code_env_file(path: Option<&Path>) -> CliResult<CodeEnvFile> {
    let Some(path) = path else {
        return Ok(CodeEnvFile::default());
    };

    let contents = fs::read_to_string(path).map_err(|error| {
        CliError::io(format!(
            "failed to read --env-file {}: {error}",
            path.display()
        ))
    })?;
    parse_code_env_file(&contents, path).map_err(CliError::command_usage)
}

/// Build the Code UI wire-projection redactor, registering A0-08-forbidden
/// `--env-file` values so provider keys cannot leak into snapshot/SSE/
/// diagnostics even if they appear as raw substrings.
fn projection_secret_redactor(env_file: &CodeEnvFile) -> Arc<SecretRedactor> {
    Arc::new(
        SecretRedactor::default_runtime().with_forbidden_env_values(
            env_file
                .values
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
        ),
    )
}

fn parse_code_env_file(contents: &str, path: &Path) -> Result<CodeEnvFile, String> {
    let mut values = BTreeMap::new();
    for (index, raw_line) in contents.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "{}:{line_no}: expected KEY=VALUE entry",
                path.display()
            ));
        };
        let key = key.trim();
        if !is_valid_env_key(key) {
            return Err(format!(
                "{}:{line_no}: invalid environment variable name `{key}`",
                path.display()
            ));
        }

        let value = parse_env_file_value(value).map_err(|message| {
            format!(
                "{}:{line_no}: invalid value for `{key}`: {message}",
                path.display()
            )
        })?;
        values.insert(key.to_string(), value);
    }

    Ok(CodeEnvFile { values })
}

fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn parse_env_file_value(raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Ok(String::new());
    }

    let first = value.as_bytes()[0];
    match first {
        b'\'' | b'"' => {
            if value.as_bytes().last() != Some(&first) || value.len() < 2 {
                return Err("quoted values must end with the matching quote".to_string());
            }
            let inner = &value[1..value.len() - 1];
            if first == b'"' {
                parse_double_quoted_env_value(inner)
            } else {
                Ok(inner.to_string())
            }
        }
        _ => Ok(strip_inline_env_comment(value).trim_end().to_string()),
    }
}

fn parse_double_quoted_env_value(value: &str) -> Result<String, String> {
    let mut parsed = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            parsed.push(ch);
            continue;
        }

        let Some(escaped) = chars.next() else {
            return Err("trailing backslash in quoted value".to_string());
        };
        match escaped {
            'n' => parsed.push('\n'),
            'r' => parsed.push('\r'),
            't' => parsed.push('\t'),
            '\\' => parsed.push('\\'),
            '"' => parsed.push('"'),
            other => parsed.push(other),
        }
    }
    Ok(parsed)
}

fn strip_inline_env_comment(value: &str) -> &str {
    for (index, ch) in value.char_indices() {
        if ch == '#' && (index == 0 || value[..index].ends_with(char::is_whitespace)) {
            return &value[..index];
        }
    }
    value
}

fn provider_env_value_with_lookup(
    env_file: &CodeEnvFile,
    key: &str,
    lookup: impl FnOnce(&str) -> Option<String>,
) -> Option<String> {
    env_file
        .get(key)
        .map(str::to_string)
        .or_else(|| lookup(key))
}

/// Resolve a provider credential / base-URL key with the shared env-file →
/// process/Vault precedence used by the default Web bootstrap.
///
/// Exposed for W4-01 verification (`default_web_env_file_precedence`): an
/// env-file value must win over a competing process-env value for the same key.
pub fn resolve_provider_env_with_process_fallback(
    env_file_path: &Path,
    key: &str,
) -> CliResult<Option<String>> {
    let env_file = load_code_env_file(Some(env_file_path))?;
    Ok(provider_env_value_with_lookup(&env_file, key, |k| {
        std::env::var(k).ok()
    }))
}

/// W4-01 test hook: mirror the default-Web gate used by [`execute`] /
/// [`execute_web_only`] — `code_uses_web_launch` + `validate_mode_args` +
/// `load_code_env_file` + shared env-file → process precedence.
///
/// Returns the resolved credential for `key` so integration tests can assert
/// env-file wins without standing up the full HTTP/MCP lifecycle.
pub fn resolve_default_web_provider_env_key(
    args: &CodeArgs,
    key: &str,
) -> CliResult<Option<String>> {
    if !code_uses_web_launch(args) {
        return Err(CliError::failure(
            "expected default Web Code UI launch (not reachable with --stdio or bare --provider codex --resume)"
                .to_string(),
        ));
    }
    validate_mode_args(args, &OutputConfig::default()).map_err(CliError::failure)?;
    let env_file = load_code_env_file(args.env_file.as_deref())?;
    Ok(provider_env_value_with_lookup(&env_file, key, |k| {
        std::env::var(k).ok()
    }))
}

/// Build an [`AnyCompletionModel`] for every non-Codex provider through the
/// shared [`ProviderFactory`].
///
/// This consolidates what used to be eight near-identical match arms
/// (`Gemini`, `Openai`, `Anthropic`, `Deepseek`, `Kimi`, `Zhipu`, `Ollama`,
/// `Fake`) into a single dispatch. The Codex provider stays on its own path
/// because it bypasses `AnyCompletionModel` entirely (managed app-server
/// runtime).
///
/// Env resolution flows through [`provider_env_value_with_lookup`] for
/// **every** provider, not just Deepseek / Kimi as before. The precedence is
/// `--env-file` first then process env (documented on `--env-file` itself),
/// and applies to API keys, base URLs, and the boolean `OLLAMA_COMPACT_TOOLS`
/// flag. Gemini / OpenAI / Anthropic / Zhipu used to read only from process
/// env via `from_env()`; this widens them to consult `--env-file` first as
/// well, so a value defined in the env-file now wins over a stale process-env
/// value for those providers.
///
/// The function returns the resolved model name AND the effective provider
/// name string so the caller can tag usage / UI metadata against the agent's
/// chosen provider (which may differ from `--provider` after an `--agent`
/// override).
///
/// OC-Phase 2 P2.4 added the `--agent <name>` override path. When the flag
/// is set the helper loads the profile via the same three-tier hierarchy
/// the runtime uses, asserts the agent is primary-eligible, and — if the
/// profile carries a structured `model: provider/model[@variant]` binding —
/// uses that binding **atomically**: provider id, model id, and variant all
/// come from the agent's spec. A separately-supplied `--model` is **ignored**
/// when the binding wins, since mixing an explicit model id with the agent's
/// provider can produce nonsense pairs (e.g. anthropic provider with an
/// OpenAI-shaped model id). When the agent profile does NOT carry a binding,
/// the CLI defaults stand verbatim.
fn build_any_completion_model_for_args(
    args: &CodeArgs,
    env_file: &CodeEnvFile,
    working_dir: &std::path::Path,
) -> CliResult<(
    crate::internal::ai::providers::AnyCompletionModel,
    String,
    String,
)> {
    build_any_completion_model_for_args_with_lookup(args, env_file, working_dir, |key| {
        // Vault-aware fallback chain: try process env first (cheap), then
        // fall back to the libra config DB (repo-local + global
        // `vault.env.<name>`) via the sync resolver. Phase 5 from_env →
        // resolve_env call-site cutover: users who configured an API key
        // once via `libra config --global add vault.env.GEMINI_API_KEY <…>`
        // no longer need to re-export it in every shell.
        //
        // The DB read may fail (e.g. stale global config schema); we treat
        // any error as "value not present" here so the provider bootstrap
        // path falls through to its existing "API key not set" error,
        // matching the v0.17.534 fallback semantics. Hard schema-mismatch
        // chains are still surfaced via `tracing::warn!` inside
        // `resolve_env_for_target`.
        match crate::internal::config::resolve_env_sync(key) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    key = key,
                    error = %format!("{error:#}"),
                    "vault-aware env resolution failed; falling back to None"
                );
                None
            }
        }
    })
}

/// Resolve a provider's API base URL from the CLI `--api-base` flag and the
/// provider-specific `*_BASE_URL` env fallback. Pure and table-testable
/// (`resolve_env` is the env-file→process→vault lookup at the call site).
///
/// Per-provider rules (kept identical to the inline match this replaced):
/// - `openai`/`anthropic`/`kimi`/`zhipu`/`ollama`: CLI flag wins, else the
///   provider's `*_BASE_URL` env var (`OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`,
///   `MOONSHOT_BASE_URL`, `ZHIPU_BASE_URL`, `OLLAMA_BASE_URL`).
/// - `deepseek`/`gemini`: CLI flag only — no env fallback.
/// - anything else (incl. codex, which never reaches the factory): `None`.
fn resolve_provider_api_base(
    provider_id_str: &str,
    cli_api_base: Option<String>,
    resolve_env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    use crate::internal::ai::providers::runtime::provider_id;
    match provider_id_str {
        provider_id::ANTHROPIC => cli_api_base.or_else(|| resolve_env("ANTHROPIC_BASE_URL")),
        provider_id::OPENAI => cli_api_base.or_else(|| resolve_env("OPENAI_BASE_URL")),
        provider_id::DEEPSEEK => cli_api_base,
        provider_id::GEMINI => cli_api_base,
        provider_id::KIMI => cli_api_base.or_else(|| resolve_env("MOONSHOT_BASE_URL")),
        provider_id::ZHIPU => cli_api_base.or_else(|| resolve_env("ZHIPU_BASE_URL")),
        provider_id::OLLAMA => cli_api_base.or_else(|| resolve_env("OLLAMA_BASE_URL")),
        _ => None,
    }
}

fn build_any_completion_model_for_args_with_lookup(
    args: &CodeArgs,
    env_file: &CodeEnvFile,
    working_dir: &std::path::Path,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> CliResult<(
    crate::internal::ai::providers::AnyCompletionModel,
    String,
    String,
)> {
    use crate::internal::ai::{
        agent::profile::ModelBinding,
        providers::{
            ProviderBuildOptions, ProviderFactory, ProviderFactoryError, runtime::provider_id,
        },
    };

    // 1. Map `--provider` to the canonical provider id string (the factory's
    //    dispatch key). Codex bypasses this helper entirely.
    let mut provider_id_str = match args.provider {
        CodeProvider::Gemini => provider_id::GEMINI.to_string(),
        CodeProvider::Openai => provider_id::OPENAI.to_string(),
        CodeProvider::Anthropic => provider_id::ANTHROPIC.to_string(),
        CodeProvider::Deepseek => provider_id::DEEPSEEK.to_string(),
        CodeProvider::Kimi => provider_id::KIMI.to_string(),
        CodeProvider::Zhipu => provider_id::ZHIPU.to_string(),
        CodeProvider::Ollama => provider_id::OLLAMA.to_string(),
        #[cfg(feature = "test-provider")]
        CodeProvider::Fake => provider_id::FAKE.to_string(),
        CodeProvider::Codex => {
            // Codex never reaches this helper — its dispatch path skips the
            // factory entirely. Treat as a programmer error rather than a
            // runtime failure so a future refactor cannot silently misroute.
            return Err(CliError::command_usage(
                "internal error: Codex provider must use the managed runtime path, \
                 not the completion-model factory",
            ));
        }
    };

    // 2. Resolve the default model id from the CLI provider. Ollama errors
    //    if `--model` is omitted (no sensible local default); the rest fall
    //    back to a flagship model constant. Honored only when the agent
    //    override does not supply a binding model id below.
    let cli_default_model = |provider: CodeProvider| -> CliResult<String> {
        Ok(match provider {
            CodeProvider::Gemini => GEMINI_2_5_FLASH.to_string(),
            CodeProvider::Openai => GPT_4O_MINI.to_string(),
            CodeProvider::Anthropic => CLAUDE_3_5_SONNET.to_string(),
            CodeProvider::Deepseek => "deepseek-chat".to_string(),
            CodeProvider::Kimi => KIMI_K2_6.to_string(),
            CodeProvider::Zhipu => GLM_5.to_string(),
            CodeProvider::Ollama => {
                return Err(CliError::command_usage(
                    "--model is required when using --provider ollama \
                     (e.g. --model llama3.2)",
                ));
            }
            #[cfg(feature = "test-provider")]
            CodeProvider::Fake => FAKE_DEFAULT_MODEL.to_string(),
            CodeProvider::Codex => unreachable!("Codex filtered above"),
        })
    };

    let mut variant: Option<String> = None;
    // 3. OC-Phase 2 P2.4: apply `--agent <name>` override atomically.
    //    When the profile carries a structured binding, all three of
    //    (provider_id, model_id, variant) come from the spec — `--model`
    //    is ignored to avoid hybrid pairs like "anthropic + gpt-4o".
    let agent_binding = resolve_agent_binding_override(args, working_dir)?;
    let model_name: String = if let Some(binding) = agent_binding {
        provider_id_str = binding.provider_id;
        variant = binding.variant;
        binding.model_id
    } else {
        match args.model.clone() {
            Some(m) => m,
            None => cli_default_model(args.provider)?,
        }
    };

    // 4. Resolve API key / base URL by provider id (string-keyed so the
    //    agent override flows through to env-var lookup).
    let resolve_env = |key: &str| provider_env_value_with_lookup(env_file, key, &env_lookup);

    let api_key = match provider_id_str.as_str() {
        provider_id::GEMINI => resolve_env("GEMINI_API_KEY"),
        provider_id::OPENAI => resolve_env("OPENAI_API_KEY"),
        provider_id::ANTHROPIC => resolve_env("ANTHROPIC_API_KEY"),
        provider_id::DEEPSEEK => resolve_env("DEEPSEEK_API_KEY"),
        provider_id::KIMI => resolve_env("MOONSHOT_API_KEY"),
        provider_id::ZHIPU => resolve_env("ZHIPU_API_KEY"),
        provider_id::OLLAMA => resolve_env("OLLAMA_API_KEY"),
        #[cfg(feature = "test-provider")]
        provider_id::FAKE => None,
        _ => None,
    };

    let api_base = resolve_provider_api_base(&provider_id_str, args.api_base.clone(), resolve_env);

    #[cfg(feature = "test-provider")]
    let fake_fixture_path = if provider_id_str == provider_id::FAKE {
        Some(args.fake_fixture.clone().ok_or_else(|| {
            CliError::command_usage("--fake-fixture is required with --provider=fake")
        })?)
    } else {
        None
    };
    #[cfg(not(feature = "test-provider"))]
    let fake_fixture_path: Option<std::path::PathBuf> = None;

    // The Ollama client used to read `OLLAMA_COMPACT_TOOLS` from process env
    // at construction time. The factory now sets the flag explicitly, so we
    // need to fold that env var back in when the CLI flag is absent —
    // otherwise users with `OLLAMA_COMPACT_TOOLS=1` in their environment
    // would silently lose compact-schema mode after this migration.
    let ollama_compact_tools = args.ollama_compact_tools
        || resolve_env("OLLAMA_COMPACT_TOOLS")
            .map(|raw| {
                matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);

    let options = ProviderBuildOptions {
        api_key,
        api_base,
        ollama_compact_tools,
        fake_fixture_path,
        // Preserve the pre-factory behaviour of accepting any model string
        // the user passes via `--model`. The capability table is best-effort
        // and the runtime will surface a real provider error if the model
        // does not exist.
        accept_unknown_models: true,
    };

    let binding = ModelBinding {
        provider_id: provider_id_str.clone(),
        model_id: model_name.clone(),
        variant,
    };

    let model = ProviderFactory
        .build(&binding, options)
        .map_err(|err| match err {
            ProviderFactoryError::MissingApiKey { env_var, .. } => {
                if provider_id_str == provider_id::OLLAMA {
                    // Ollama Cloud needs the api key only when the base URL points
                    // at ollama.com; preserve the pre-factory error wording so users
                    // who scripted against it do not see a regression.
                    CliError::auth(
                        "OLLAMA_API_KEY is required when using Ollama Cloud directly \
                     (set --api-base https://ollama.com or OLLAMA_BASE_URL=https://ollama.com)",
                    )
                } else {
                    // Name the missing variable AND how to configure it, so
                    // the user has an actionable next step rather than a bare
                    // "not set" (C3 criterion: missing-key errors must say
                    // which env var and how to set it). Mirrors the
                    // vault-aware resolution chain in
                    // `build_any_completion_model_for_args`.
                    CliError::auth(format!(
                        "{env_var} is not set; export {env_var} or store it with \
                         `libra config --global add vault.env.{env_var} <value>`"
                    ))
                }
            }
            ProviderFactoryError::BuildFailed { reason, .. } => CliError::io(reason),
            ProviderFactoryError::UnknownProvider { .. }
            | ProviderFactoryError::UnknownModel { .. } => CliError::command_usage(err.to_string()),
        })?;

    Ok((model, model_name, provider_id_str))
}

/// Look up the agent profile selected by `--agent <name>` and return its
/// structured `ModelBinding` if the profile carries one (OC-Phase 2 P2.4).
///
/// Returns `Ok(None)` when:
/// - `--agent` was not supplied; the helper is a no-op.
/// - The agent exists but has no `model: provider/model` binding (legacy
///   `model: default` / `fast` / etc.). The CLI defaults stand.
///
/// Returns `Err(_)` when:
/// - The agent name does not match any profile in the three-tier hierarchy.
/// - The agent's `mode` is not primary-eligible (sub-agents are dispatched
///   via the `task` tool in OC-Phase 3, not as the session driver).
fn resolve_agent_binding_override(
    args: &CodeArgs,
    working_dir: &std::path::Path,
) -> CliResult<Option<crate::internal::ai::agent::profile::ModelBinding>> {
    let Some(agent_name) = args.agent.as_deref() else {
        return Ok(None);
    };
    let profiles = load_profiles(working_dir);
    let router = AgentProfileRouter::new(profiles);
    let spec = router.execution_spec(agent_name).ok_or_else(|| {
        let mut suggestions: Vec<&str> =
            router.profiles().iter().map(|p| p.name.as_str()).collect();
        suggestions.sort();
        let suggestion_hint = if suggestions.is_empty() {
            String::from("(no profiles loaded)")
        } else {
            format!("known agents: {}", suggestions.join(", "))
        };
        CliError::command_usage(format!(
            "unknown agent '{agent_name}' for --agent; {suggestion_hint}"
        ))
    })?;
    if !spec.mode.is_primary_eligible() {
        return Err(CliError::command_usage(format!(
            "agent '{agent_name}' has mode '{:?}', which is not primary-eligible. \
             Sub-agents are dispatched via the `task` tool, not selected with --agent.",
            spec.mode
        )));
    }
    Ok(spec.model)
}

fn completion_thinking_for_args(args: &CodeArgs) -> Option<CompletionThinking> {
    completion_thinking_for_provider(args.provider, args)
}

/// Provider-explicit variant of [`completion_thinking_for_args`] used by the
/// `--agent` override path so the resolved provider drives the dispatch.
fn completion_thinking_for_provider(
    provider: CodeProvider,
    args: &CodeArgs,
) -> Option<CompletionThinking> {
    match provider {
        CodeProvider::Ollama => args.ollama_thinking.map(CompletionThinking::from),
        CodeProvider::Deepseek => args.deepseek_thinking.map(CompletionThinking::from),
        CodeProvider::Kimi => args.kimi_thinking.map(CompletionThinking::from),
        _ => None,
    }
}

fn completion_reasoning_effort_for_args(args: &CodeArgs) -> Option<CompletionReasoningEffort> {
    completion_reasoning_effort_for_provider(args.provider, args)
}

/// Provider-explicit variant of [`completion_reasoning_effort_for_args`].
fn completion_reasoning_effort_for_provider(
    provider: CodeProvider,
    args: &CodeArgs,
) -> Option<CompletionReasoningEffort> {
    match provider {
        CodeProvider::Deepseek => args
            .deepseek_reasoning_effort
            .map(CompletionReasoningEffort::from),
        _ => None,
    }
}

fn completion_stream_for_args(args: &CodeArgs) -> Option<bool> {
    completion_stream_for_provider(args.provider, args)
}

/// Provider-explicit variant of [`completion_stream_for_args`].
fn completion_stream_for_provider(provider: CodeProvider, args: &CodeArgs) -> Option<bool> {
    match provider {
        CodeProvider::Deepseek => args.deepseek_stream,
        CodeProvider::Kimi => Some(args.kimi_stream.unwrap_or(true)),
        _ => None,
    }
}

fn preserve_reasoning_content_for_provider(provider: CodeProvider) -> bool {
    matches!(provider, CodeProvider::Deepseek | CodeProvider::Kimi)
}

// ---------------------------------------------------------------------------
// Codex provider — managed app-server lifecycle
// ---------------------------------------------------------------------------

/// Represents a managed Codex app-server child process and its WebSocket URL.
///
/// The server is spawned as a child process and communicated with over WebSocket.
/// [`ManagedCodexServer::shutdown`] sends SIGKILL and waits within a bounded
/// deadline; `kill_on_drop` is also set at spawn time as a final fallback.
struct ManagedCodexServer {
    ws_url: String,
    child: Child,
}

/// Owns a managed Codex child until the process lifecycle owner takes it.
///
/// Bootstrap early-returns (terminal init, session load, runtime construction)
/// drop this guard instead of calling `shutdown`; Drop best-effort kills and
/// reaps so the child cannot outlive the failed start path.
struct ManagedCodexBootstrapGuard {
    server: Option<ManagedCodexServer>,
}

impl ManagedCodexBootstrapGuard {
    fn new(server: Option<ManagedCodexServer>) -> Self {
        Self { server }
    }

    fn take(&mut self) -> Option<ManagedCodexServer> {
        self.server.take()
    }
}

impl Drop for ManagedCodexBootstrapGuard {
    fn drop(&mut self) {
        let Some(mut server) = self.server.take() else {
            return;
        };
        if server.child.id().is_none() {
            return;
        }
        // Prefer bounded async shutdown/reaping on the current runtime so we do
        // not leave a zombie after a nonblocking try_wait race in sync Drop.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = server.shutdown_with_timeout(Duration::from_secs(2)).await;
            });
            return;
        }
        let _ = server.child.start_kill();
        let _ = server.child.try_wait();
    }
}

const MANAGED_CODEX_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
enum ManagedCodexShutdownError {
    #[error("managed_codex_child did not stop before the shutdown deadline")]
    TimedOut,
    #[error("managed_codex_child could not be terminated: {reason}")]
    TerminationFailed { reason: String },
}

impl ManagedCodexServer {
    /// Gracefully shuts down the managed Codex app-server process.
    ///
    /// If the child process has already exited (`id()` returns `None`), this is
    /// a no-op. Otherwise it sends a kill signal via `start_kill()` and waits up
    /// to a bounded deadline for the process to terminate. A deadline failure
    /// is returned to the lifecycle owner rather than silently abandoning a
    /// possibly unreaped child.
    async fn shutdown(&mut self) -> Result<(), ManagedCodexShutdownError> {
        self.shutdown_with_timeout(MANAGED_CODEX_SHUTDOWN_TIMEOUT)
            .await
    }

    async fn shutdown_with_timeout(
        &mut self,
        shutdown_timeout: Duration,
    ) -> Result<(), ManagedCodexShutdownError> {
        if self.child.id().is_none() {
            return Ok(());
        }
        if let Err(error) = self.child.start_kill() {
            return match self.child.try_wait() {
                Ok(Some(_)) => Ok(()),
                Ok(None) => Err(ManagedCodexShutdownError::TerminationFailed {
                    reason: error.to_string(),
                }),
                Err(wait_error) => Err(ManagedCodexShutdownError::TerminationFailed {
                    reason: format!("{error}; failed to inspect child state: {wait_error}"),
                }),
            };
        }
        match tokio::time::timeout(shutdown_timeout, self.child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(ManagedCodexShutdownError::TerminationFailed {
                reason: error.to_string(),
            }),
            Err(_) => Err(ManagedCodexShutdownError::TimedOut),
        }
    }
}

struct ControlRuntimeConfig {
    mode: ControlMode,
    paths: ControlPaths,
    token: Option<Arc<str>>,
    _lock_guard: Option<ControlLockGuard>,
    write_info: bool,
    cleanup_token: bool,
    info_written: AtomicBool,
    started_at: chrono::DateTime<Utc>,
    /// This process's control scope (§C.8 W4), stamped into `control.json`
    /// and re-checked before any overwrite of an existing info file.
    scope: ControlScope,
    /// Linked-worktree evidence snapshot for the legacy-ambiguity rule.
    linked_evidence: bool,
}

impl ControlRuntimeConfig {
    fn mode_name(&self) -> &'static str {
        match self.mode {
            ControlMode::Observe => "observe",
            ControlMode::Write => "write",
            // Client-only; prepare_control_runtime must not build a sidecar for Stdio.
            ControlMode::Stdio => "stdio",
        }
    }

    fn cleanup(&self) {
        cleanup_control_files(
            &self.paths,
            self.cleanup_token,
            self.info_written.load(Ordering::Relaxed),
        );
    }

    fn write_info_file(
        &self,
        working_dir: &Path,
        base_url: String,
        mcp_url: Option<String>,
        thread_id: Option<String>,
    ) -> CliResult<()> {
        if !self.write_info {
            return Ok(());
        }

        // §C.8 W4: never overwrite another scope's control file. Observe mode
        // holds no lock, so this pre-write check is its only takeover gate;
        // for write mode it re-validates under the already-held lock.
        ensure_scope_takeover_allowed(
            &self.paths.info,
            &self.scope,
            ControlScopePolicy::Worktree,
            self.linked_evidence,
            &crate::utils::util::try_get_storage_path(Some(working_dir.to_path_buf()))
                .unwrap_or_default(),
        )
        .map_err(|error| {
            CliError::conflict(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        })?;

        let info = ControlInfo {
            version: CONTROL_INFO_VERSION,
            mode: self.mode_name().to_string(),
            pid: std::process::id(),
            base_url,
            mcp_url,
            working_dir: working_dir.to_path_buf(),
            thread_id,
            started_at: self.started_at,
            repo_id: Some(self.scope.repo_id.clone()),
            worktree_id: self.scope.worktree_id.clone(),
            workspace_id: self.scope.workspace_id.clone(),
            lease_fence: self.scope.lease_fence,
            pid_starttime: current_pid_starttime(),
        };
        write_control_info(&self.paths.info, &info).map_err(|error| {
            CliError::fatal(format!(
                "failed to write local control info '{}': {error}",
                self.paths.info.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
        self.info_written.store(true, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for ControlRuntimeConfig {
    fn drop(&mut self) {
        self.cleanup();
    }
}

async fn prepare_control_runtime(
    args: &CodeArgs,
    working_dir: &Path,
) -> CliResult<ControlRuntimeConfig> {
    let paths = resolve_control_paths(
        working_dir,
        args.control_token_file.as_deref(),
        args.control_info_file.as_deref(),
    );
    let started_at = Utc::now();

    // §C.8 W4: resolve this process's control scope once, up front, so both
    // modes stamp `control.json` identically and the pre-write takeover gate
    // has its inputs. `libra code` holds no workspace lease at startup, so
    // the workspace association stays empty until a leased runtime threads
    // one through.
    let scope = resolve_control_scope(working_dir, None)
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to resolve the control scope for '{}': {error:#}",
                working_dir.display()
            ))
        })?;
    // §C.4.1: an unresolvable storage root is itself evidence something is
    // wrong with this worktree's linkage — treat it as linked (fail closed)
    // rather than probing a path we would have had to invent.
    let linked_evidence = resolve_storage_root(working_dir)
        .as_deref()
        .is_none_or(repo_has_linked_evidence);

    match args.control {
        ControlMode::Observe => Ok(ControlRuntimeConfig {
            mode: ControlMode::Observe,
            paths,
            token: None,
            _lock_guard: None,
            write_info: args.control_info_file.is_some(),
            cleanup_token: false,
            info_written: AtomicBool::new(false),
            started_at,
            scope,
            linked_evidence,
        }),
        ControlMode::Write => {
            let lock_guard = acquire_control_lock(&paths.lock).map_err(|error| match error {
                ControlLockError::AlreadyHeld { .. } => CliError::conflict(error.to_string()),
                ControlLockError::Io(error) => CliError::io(format!(
                    "failed to acquire local control lock '{}': {error}",
                    paths.lock.display()
                )),
            })?;
            let token = ensure_control_token_file(&paths.token)
                .await
                .map_err(|error| {
                    CliError::fatal(format!(
                        "failed to prepare local control token '{}': {error}",
                        paths.token.display()
                    ))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
                })?;

            Ok(ControlRuntimeConfig {
                mode: ControlMode::Write,
                paths,
                token: Some(Arc::<str>::from(token)),
                _lock_guard: Some(lock_guard),
                write_info: true,
                cleanup_token: true,
                info_written: AtomicBool::new(false),
                started_at,
                scope,
                linked_evidence,
            })
        }
        ControlMode::Stdio => Err(CliError::fatal(
            "internal error: `--control stdio` is a client-only mode and must not prepare a control sidecar; report this as a bug",
        )),
    }
}

fn ensure_loopback_browser_control_host(host: &str) -> CliResult<()> {
    let normalized = host.trim().trim_matches('[').trim_matches(']');
    let is_loopback = matches!(normalized, "localhost" | "127.0.0.1" | "::1")
        || normalized
            .parse::<std::net::IpAddr>()
            .map(|addr| addr.is_loopback())
            .unwrap_or(false);

    if is_loopback {
        return Ok(());
    }

    Err(CliError::command_usage(
        "interactive web control is restricted to loopback hosts in v1; use --host 127.0.0.1",
    ))
}

/// Resolve the effective [`BrowserControlMode`] for this invocation.
///
/// User-supplied `--browser-control` always wins. When the flag is omitted
/// the default is mode-aware:
///   - Web launch (the default) → `loopback` (interactive browser writes)
///   - `--stdio` → `off` (no browser surface on the MCP transport)
///
/// `loopback` further requires that `--host` is a loopback address; this is
/// validated up-front so we fail closed before any port is bound. Non-loopback
/// binds must pass `--browser-control off` explicitly.
pub fn resolve_browser_control_mode(args: &CodeArgs) -> CliResult<BrowserControlMode> {
    let mode = match args.browser_control {
        Some(mode) => mode,
        None => default_browser_control_mode(args),
    };
    if mode == BrowserControlMode::Loopback {
        ensure_loopback_browser_control_host(&args.host)?;
    }
    Ok(mode)
}

fn default_browser_control_mode(args: &CodeArgs) -> BrowserControlMode {
    // W4-01: default Web must be interactive. Loopback browser leases mint a
    // per-session bootstrap secret (Origin alone is not enough). Shared
    // machines should pass `--browser-control off`.
    if code_uses_web_launch(args) {
        BrowserControlMode::Loopback
    } else {
        BrowserControlMode::Off
    }
}

fn browser_control_banner_line(mode: BrowserControlMode) -> String {
    match mode {
        BrowserControlMode::Loopback => {
            "Browser control: loopback (bootstrap token required; use --browser-control off on shared machines)"
                .to_string()
        }
        BrowserControlMode::Off => format!("Browser control: {}", mode.as_str()),
    }
}

fn mint_browser_bootstrap_token(browser_control: BrowserControlMode) -> Option<Arc<str>> {
    (browser_control == BrowserControlMode::Loopback)
        .then(|| Arc::<str>::from(Uuid::new_v4().to_string()))
}

fn code_ui_url_with_bootstrap(base_url: &str, bootstrap: Option<&Arc<str>>) -> String {
    match bootstrap {
        Some(token) => format!("{base_url}?bt={token}"),
        None => base_url.to_string(),
    }
}

/// CLI-side wrapper around `code_ui::test_lease_duration_override` that maps
/// the helper's `String` error into `CliError::command_usage` so a bad
/// `LIBRA_CODE_LEASE_DURATION_MS` value fails the command at startup with
/// a stable, user-readable message.
fn code_ui_test_lease_duration_override() -> CliResult<Option<chrono::Duration>> {
    crate::internal::ai::web::code_ui::test_lease_duration_override()
        .map_err(CliError::command_usage)
}

const HEADLESS_CODE_UI_SNAPSHOT_METADATA_KEY: &str = "code_ui_snapshot";

struct HeadlessWebSessionBootstrap {
    store: Arc<SessionStore>,
    state: SessionState,
}

struct HeadlessApprovalChannels {
    exec_approval_tx: mpsc::UnboundedSender<ExecApprovalRequest>,
    exec_approval_rx: mpsc::UnboundedReceiver<ExecApprovalRequest>,
}

/// Bootstraps the `SessionState` for a default Web non-Codex headless run.
///
/// Default Web `--resume <thread_id>` reaches this same load-or-create path for
/// non-Codex providers. The restored state is then folded with the bounded
/// Code UI workflow suffix before the browser server starts. Managed Codex
/// keeps its separate app-server session protocol and remains rejected by the
/// mode validator rather than silently starting a fresh session.
fn load_or_create_headless_web_session_state(
    args: &CodeArgs,
    working_dir: &Path,
    session_store: &Arc<SessionStore>,
) -> CliResult<SessionState> {
    let working_dir_str = working_dir.to_string_lossy().to_string();
    let mut session = if let Some(thread_id) = args.resume.as_deref() {
        if thread_id.trim().is_empty() {
            return Err(CliError::command_usage(
                "--resume requires a non-empty thread_id",
            ));
        }
        match session_store.load_for_thread_id(thread_id, &working_dir_str) {
            Ok(Some(session)) => session,
            Ok(None) => {
                return Err(CliError::fatal(format!(
                    "no Libra Code session found for thread_id '{thread_id}' in working directory '{working_dir_str}'"
                )));
            }
            Err(error) => {
                return Err(CliError::io(format!(
                    "failed to load Libra Code session for thread_id '{thread_id}': {error}"
                )));
            }
        }
    } else {
        SessionState::new(&working_dir_str)
    };

    let thread_id = session_canonical_thread_id(&session).unwrap_or_else(|| session.id.clone());
    session
        .metadata
        .entry("thread_id".to_string())
        .or_insert_with(|| serde_json::json!(thread_id));
    Ok(session)
}

struct CodeUiResumeFold {
    snapshot: CodeUiSessionSnapshot,
    projection_sequence: u64,
}

/// Normalize a legacy/bootstrap snapshot before applying the bounded workflow fold.
///
/// Cancels in-flight streaming transcript rows and preserves durable safety fences
/// (`IndeterminateSideEffect`, pending interactions) that an empty replay suffix
/// would otherwise erase.
fn finalize_code_ui_resume_bootstrap_snapshot(snapshot: &mut CodeUiSessionSnapshot) {
    let now = Utc::now();
    for entry in &mut snapshot.transcript {
        if entry.streaming {
            entry.streaming = false;
            if !matches!(
                entry.status.as_deref(),
                Some("completed" | "error" | "cancelled")
            ) {
                entry.status = Some("cancelled".to_string());
            }
            entry.updated_at = now;
        }
    }
    let has_pending_interaction = snapshot
        .interactions
        .iter()
        .any(|interaction| interaction.status == CodeUiInteractionStatus::Pending);
    // An indeterminate mutation is a durable safety fence.  The projection
    // cursor can already be checkpointed at the end of the event stream, so a
    // resume may have no events left to replay that would restore this state.
    // Do not turn that fence into an idle, writable session while rebuilding
    // the browser snapshot.
    snapshot.status = if snapshot.status == CodeUiSessionStatus::IndeterminateSideEffect {
        CodeUiSessionStatus::IndeterminateSideEffect
    } else if has_pending_interaction {
        CodeUiSessionStatus::AwaitingInteraction
    } else {
        CodeUiSessionStatus::Idle
    };
    snapshot.updated_at = now;
}

/// Build the legacy/bootstrap Code UI snapshot used by Web resume.
fn build_code_ui_resume_bootstrap_snapshot(
    working_dir: impl Into<String>,
    session: &SessionState,
    provider: CodeUiProviderInfo,
    capabilities: CodeUiCapabilities,
    projection_bundle: Option<&ThreadBundle>,
) -> Result<CodeUiSessionSnapshot, String> {
    let working_dir = working_dir.into();
    // Prefer a durable Code UI checkpoint over a ThreadBundle skeleton. The
    // bundle has no transcript/interaction overlays, so using it when a
    // checkpoint exists would drop indeterminate fences and live UI state
    // while replay starts after the projection cursor.
    let checkpoint = match session.metadata.get(HEADLESS_CODE_UI_SNAPSHOT_METADATA_KEY) {
        Some(value) => match serde_json::from_value::<CodeUiSessionSnapshot>(value.clone()) {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                return Err(format!(
                    "session '{}' has a durable Code UI checkpoint that cannot be deserialized ({error}); refusing to resume with a fresh snapshot that could hide an indeterminate reconciliation fence",
                    session.id
                ));
            }
        },
        None => None,
    };
    let mut snapshot = match (checkpoint, projection_bundle) {
        (Some(checkpoint), _) => checkpoint,
        (None, Some(bundle)) => snapshot_from_thread_bundle(
            working_dir.clone(),
            provider.clone(),
            capabilities.clone(),
            bundle,
        ),
        (None, None) => {
            initial_snapshot(working_dir.clone(), provider.clone(), capabilities.clone())
        }
    };

    // Always stamp durable session/thread identity — including the
    // ThreadBundle bootstrap path. `snapshot_from_thread_bundle` only sets
    // thread_id; leaving session_id as a random placeholder (or historically
    // as the thread UUID) breaks SPA `/usage` filters that AND both IDs
    // against rows recorded under SessionState.id + canonical thread_id.
    snapshot.session_id = session.id.clone();
    snapshot.thread_id = session_canonical_thread_id(session)
        .or_else(|| projection_bundle.map(|bundle| bundle.thread.thread_id.to_string()))
        .or_else(|| Some(session.id.clone()));
    snapshot.working_dir = working_dir;
    snapshot.provider = provider;
    snapshot.capabilities = capabilities;
    if snapshot.transcript.is_empty() {
        snapshot.transcript = build_tui_code_ui_transcript(session);
    }

    finalize_code_ui_resume_bootstrap_snapshot(&mut snapshot);
    Ok(snapshot)
}

fn build_headless_web_code_ui_snapshot(
    working_dir: &Path,
    provider: CodeUiProviderInfo,
    capabilities: CodeUiCapabilities,
    session: &SessionState,
) -> Result<CodeUiSessionSnapshot, String> {
    build_code_ui_resume_bootstrap_snapshot(
        working_dir.to_string_lossy(),
        session,
        provider,
        capabilities,
        None,
    )
}

fn fold_code_ui_resume_from_session(
    session_store: &SessionStore,
    session: &SessionState,
    bootstrap: CodeUiSessionSnapshot,
) -> Result<CodeUiResumeFold, String> {
    let projection_store = SessionJsonlStore::new(session_store.session_root(&session.id));
    let projection_cursor = session
        .metadata
        .get("code_ui_projection_cursor")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let projection_replay = projection_store
        .load_code_workflow_replay_since(
            projection_cursor,
            MAX_CODE_UI_PROJECTION_EVENTS,
            MAX_CODE_UI_PROJECTION_REPLAY_BYTES,
        )
        .map_err(|error| {
            format!(
                "failed to load the Code UI workflow projection for session '{}': {error}",
                session.id
            )
        })?;
    let folded =
        rebuild_code_ui_read_model_from_events(bootstrap, &projection_replay).map_err(|error| {
            format!(
                "cannot safely resume the Code UI workflow projection for session '{}': {error}",
                session.id
            )
        })?;
    Ok(CodeUiResumeFold {
        snapshot: folded.snapshot,
        projection_sequence: folded.last_sequence.unwrap_or(projection_cursor),
    })
}

/// Rebuild a browser Code UI snapshot for a selected durable thread.
///
/// Session stores are scoped to the current working directory until
/// `ThreadProjection` records per-thread paths, so callers must provide the
/// server working directory rather than infer it from the repository-wide
/// thread list.
pub enum ResumeCodeUiSessionError {
    NotFound {
        thread_id: String,
        working_dir: String,
    },
    LoadFailed {
        thread_id: String,
        message: String,
    },
}

impl std::fmt::Display for ResumeCodeUiSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound {
                thread_id,
                working_dir,
            } => write!(
                f,
                "no Code session for thread '{thread_id}' exists under '{working_dir}'; resume from that thread's original working directory"
            ),
            Self::LoadFailed { thread_id, message } => write!(
                f,
                "failed to load Code session for thread '{thread_id}': {message}"
            ),
        }
    }
}

pub async fn resume_code_ui_session_to_thread(
    working_dir: &Path,
    thread_id: &str,
    provider: CodeUiProviderInfo,
    capabilities: CodeUiCapabilities,
) -> Result<CodeUiSessionSnapshot, ResumeCodeUiSessionError> {
    let storage_root =
        resolve_storage_root(working_dir).ok_or_else(|| ResumeCodeUiSessionError::LoadFailed {
            thread_id: thread_id.to_string(),
            message: "cannot resolve the repository storage root; if this is a linked worktree, \
                      run `libra worktree repair --confirm <worktree-path>`"
                .to_string(),
        })?;
    // Match the CLI `--resume` path: sessions live on the shared repository
    // storage root, not under a linked worktree's local `.libra/`.
    let session_store = SessionStore::from_storage_path(&storage_root);
    let working_dir_string = working_dir.to_string_lossy().to_string();
    let session = session_store
        .load_for_thread_id(thread_id, &working_dir_string)
        .map_err(|error| ResumeCodeUiSessionError::LoadFailed {
            thread_id: thread_id.to_string(),
            message: error.to_string(),
        })?
        .ok_or_else(|| ResumeCodeUiSessionError::NotFound {
            thread_id: thread_id.to_string(),
            working_dir: working_dir.display().to_string(),
        })?;
    let bootstrap = build_code_ui_resume_bootstrap_snapshot(
        working_dir_string,
        &session,
        provider,
        capabilities,
        None,
    )
    .map_err(|message| ResumeCodeUiSessionError::LoadFailed {
        thread_id: thread_id.to_string(),
        message,
    })?;
    let mut snapshot = fold_code_ui_resume_from_session(&session_store, &session, bootstrap)
        .map(|fold| fold.snapshot)
        .map_err(|message| ResumeCodeUiSessionError::LoadFailed {
            thread_id: thread_id.to_string(),
            message,
        })?;
    attach_indexed_thread_graph(working_dir, &mut snapshot).await;
    Ok(snapshot)
}

/// Hydrate `thread_graph` from the indexed Intent/Plan/Task/Run/PatchSet
/// lineage (`libra graph`), including completed nodes. Missing storage or an
/// unparseable thread id leaves the field unset so the Web UI can fall back.
/// Returns whether a graph was attached.
pub(crate) async fn attach_indexed_thread_graph(
    working_dir: &Path,
    snapshot: &mut CodeUiSessionSnapshot,
) -> bool {
    let Some(storage_root) = resolve_storage_root(working_dir) else {
        snapshot.thread_graph = None;
        return false;
    };
    crate::command::graph::attach_indexed_thread_graph_at(&storage_root, snapshot).await
}

/// Build a headless Code UI runtime for the default Web launch with non-Codex providers.
///
/// Constructs a minimal local-read-only [`ToolRegistry`] and a
/// [`HeadlessCodeRuntime`] lifecycle host, then mounts the production
/// [`AgentRuntimeCodeUiAdapter`] write path on [`CodeUiRuntimeHandle`].
/// Plain browser chat enters Phase 0 plan routing; slash/`/` messages keep an
/// explicit direct tool loop.
///
/// `browser_write_enabled` should mirror the resolved
/// [`BrowserControlMode::Loopback`] so the runtime advertises browser writes
/// in the snapshot capabilities. The initial controller is `Unclaimed` —
/// the browser is the only writer in headless mode, with no local interactive
/// owner to hand off from.
#[allow(clippy::too_many_arguments)]
async fn build_headless_web_code_ui_runtime<M>(
    args: &CodeArgs,
    working_dir: &Path,
    session_bootstrap: HeadlessWebSessionBootstrap,
    model: M,
    model_name: String,
    approval_channels: HeadlessApprovalChannels,
    browser_write_enabled: bool,
    mcp_server: Arc<LibraMcpServer>,
) -> CliResult<Arc<CodeUiRuntimeHandle>>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    use crate::internal::ai::agent::runtime::tool_loop::ToolLoopConfig;

    let HeadlessWebSessionBootstrap {
        store: session_store,
        state: bootstrap_session_state,
    } = session_bootstrap;
    // The initial lookup identifies the durable session. It is not safe to
    // build a writable projection from that pre-lease snapshot: another
    // process may append after the lookup and before releasing its writer
    // lease. Acquire first, then reload and fold exclusively under the lease.
    let session_lease = HeadlessSessionPersistence::acquire_session_lease(
        &session_store,
        &bootstrap_session_state.id,
    )
    .map_err(|error| {
        CliError::fatal(format!(
            "failed to acquire the writable Code session lease for '{}': {error}",
            bootstrap_session_state.id
        ))
    })?;
    let requested_session_id = bootstrap_session_state.id.clone();
    let session_state = match session_store.load(&requested_session_id) {
        Ok(state) => state,
        Err(error) if args.resume.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
            bootstrap_session_state
        }
        Err(error) => {
            return Err(CliError::fatal(format!(
                "failed to reload Code session '{}' after acquiring its writer lease: {error}",
                bootstrap_session_state.id
            )));
        }
    };
    if session_state.id != requested_session_id {
        return Err(CliError::fatal(format!(
            "reloaded Code session identity '{}' does not match leased session '{}'; repair the session log before resuming",
            session_state.id, requested_session_id
        )));
    }
    let HeadlessApprovalChannels {
        exec_approval_tx,
        exec_approval_rx,
    } = approval_channels;
    let provider_name = format!("{:?}", args.provider).to_lowercase();
    let provider = CodeUiProviderInfo {
        provider: provider_name.clone(),
        model: Some(model_name.clone()),
        mode: Some("web-headless".to_string()),
        managed: false,
    };
    let capabilities = headless_capabilities();
    let initial_history = session_state.to_history();
    let bootstrap_snapshot = build_headless_web_code_ui_snapshot(
        working_dir,
        provider.clone(),
        capabilities.clone(),
        &session_state,
    )
    .map_err(CliError::fatal)?;
    let folded =
        fold_code_ui_resume_from_session(&session_store, &session_state, bootstrap_snapshot)
            .map_err(CliError::fatal)?;
    let projection_sequence = folded.projection_sequence;
    let mut snapshot = folded.snapshot;
    attach_indexed_thread_graph(working_dir, &mut snapshot).await;
    let session = CodeUiSession::new(snapshot.clone());

    let (user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let approval_cfg = approval_config_from_args(args, working_dir).map_err(CliError::failure)?;
    let (approval_cfg, approval_cache_scope) = hydrate_approval_runtime(working_dir, approval_cfg)
        .await
        .map_err(CliError::failure)?;
    let runtime_context = Some(default_runtime_context(
        working_dir,
        args.context,
        approval_cfg,
        args.network_access.is_allowed(),
        exec_approval_tx,
        approval_cache_scope,
    ));
    let approval_store = runtime_context
        .as_ref()
        .and_then(|ctx| ctx.approval.as_ref().map(|approval| approval.store.clone()));

    let env_file = load_code_env_file(args.env_file.as_deref())?;
    let registry = build_headless_tool_registry(
        working_dir,
        user_input_tx,
        (*projection_secret_redactor(&env_file)).clone(),
    );
    // Headless Web explicit `task.dispatch` uses the same dispatcher bundle as
    // the historical interactive path. Keep its construction here, before the
    // per-turn config factory,
    // so both model `task` calls and Web controls observe the same budget,
    // approval, depth, and concurrency gates.
    let agents_config = AgentsConfig::load_from_working_dir(working_dir).unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            "failed to load agents.toml for headless task dispatch; using defaults"
        );
        AgentsConfig::default()
    });
    let agent_router = AgentProfileRouter::new(load_profiles(working_dir));
    let subagent_runtime = if agents_config.sub_agents.enabled {
        match resolve_storage_root(working_dir) {
            Some(storage_root) => match build_subagent_runtime_for_session(
                &agents_config,
                registry.clone(),
                &session_state,
                session_store.as_ref(),
                &storage_root,
                &model_name,
                &provider_name,
                &agent_router,
                None,
                runtime_context.clone(),
            )
            .await
            {
                Ok(runtime) => Some(runtime),
                Err(error) => {
                    tracing::warn!(%error, "failed to build headless SubAgentToolLoopRuntime; task.dispatch remains unavailable");
                    None
                }
            },
            None => {
                tracing::warn!(
                    "headless task.dispatch unavailable because Libra storage root could not be resolved"
                );
                None
            }
        }
    } else {
        None
    };
    let preamble = system_preamble(working_dir, args.context, args.provider, Some(&model_name))
        .map_err(CliError::failure)?;
    let preserve_reasoning_content = preserve_reasoning_content_for_provider(args.provider);
    let temperature = args.temperature;
    let thinking = completion_thinking_for_args(args);
    let reasoning_effort = completion_reasoning_effort_for_args(args);
    let stream = completion_stream_for_args(args);
    let usage_storage_root = resolve_storage_root(working_dir);
    let usage_recorder = match usage_storage_root.as_ref() {
        Some(storage_root) => build_usage_recorder(storage_root).await,
        None => None,
    };
    let usage_repo_id = canonical_usage_repo_id(usage_recorder.as_ref()).await;
    let usage_context = usage_recorder.as_ref().map(|_| UsageContext {
        repo_id: usage_repo_id.clone(),
        session_id: Some(session_state.id.clone()),
        thread_id: session_canonical_thread_id(&session_state),
        agent_run_id: None,
        run_id: None,
        turn_id: None,
        event_id: None,
        provider: provider_name.clone(),
        model: model_name.clone(),
        request_kind: "completion".to_string(),
        intent: None,
        agent_name: None,
    });

    let config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync> =
        Arc::new(move || ToolLoopConfig {
            preamble: Some(preamble.clone()),
            temperature,
            thinking,
            reasoning_effort,
            stream,
            preserve_reasoning_content,
            runtime_context: runtime_context.clone(),
            subagent_runtime: subagent_runtime.clone(),
            usage_recorder: usage_recorder.clone(),
            usage_context: usage_context.clone(),
            ..Default::default()
        });

    let persistence = HeadlessSessionPersistence::with_projection_checkpoint_and_lease(
        session_store,
        session_state,
        snapshot,
        projection_sequence,
        session_lease,
    )
    .map_err(|error| {
        CliError::fatal(format!(
            "failed to attach Code UI workflow hub for session persistence: {error}"
        ))
    })?;

    let lifecycle = HeadlessCodeRuntime::new_with_persistence(
        session,
        capabilities,
        model,
        registry,
        user_input_rx,
        exec_approval_rx,
        config_factory,
        initial_history,
        Some(persistence),
        Some(mcp_server),
    )
    .await
    .map_err(|error| {
        CliError::fatal(format!(
            "failed to construct the headless AgentRuntime adapter: {error}"
        ))
    })?;

    // W3-03: mount AgentRuntimeCodeUiAdapter as the production write-path owner.
    // HeadlessCodeRuntime remains lifecycle-only (worker, listeners, shutdown).
    let adapter = lifecycle.command_adapter();
    if let Some(store) = approval_store {
        adapter.set_approval_store(store).await;
    }

    let automation_write_enabled = args.control == ControlMode::Write;
    let mut runtime_options = CodeUiRuntimeOptions::new(
        browser_write_enabled,
        automation_write_enabled,
        CodeUiInitialController::Unclaimed,
    );
    runtime_options.lease_duration = code_ui_test_lease_duration_override()?;
    // Retain the lifecycle host on the handle; the adapter only holds a Weak
    // shutdown hook so drop cannot form an adapter↔host retain cycle.
    Ok(CodeUiRuntimeHandle::build_with_options_and_lifecycle(
        adapter,
        runtime_options,
        Some(lifecycle),
    )
    .await)
}

fn build_headless_tool_registry(
    working_dir: &Path,
    user_input_tx: mpsc::UnboundedSender<UserInputRequest>,
    redactor: SecretRedactor,
) -> Arc<ToolRegistry> {
    CodeAgentServicesBuilder::web_headless_with_redactor(
        working_dir,
        Uuid::new_v4(),
        user_input_tx,
        redactor,
    )
    .build()
    .registry()
}

/// Construct the appropriate provider client and wrap it in
/// [`build_headless_web_code_ui_runtime`]. Returns `None` when the requested
/// provider is not yet wired into the headless path so the caller can fall
/// back to the read-only placeholder gracefully.
///
/// v0 now routes several non-Codex providers through the same provider-factory
/// bootstrap used by the legacy full-workflow path. This keeps API-key/base-URL resolution centralized and
/// ensures the Web launch stays aligned with existing provider construction.
///
/// The placeholder path is still available for providers that are not in this
/// dispatch arm or fail during bootstrap for other reasons.
async fn build_non_codex_headless_runtime(
    args: &CodeArgs,
    working_dir: &Path,
    env_file: &CodeEnvFile,
    session_store: Arc<SessionStore>,
    session_state: SessionState,
    browser_write_enabled: bool,
    mcp_server: Arc<LibraMcpServer>,
) -> CliResult<Option<Arc<CodeUiRuntimeHandle>>> {
    let (exec_approval_tx, exec_approval_rx) =
        tokio::sync::mpsc::unbounded_channel::<ExecApprovalRequest>();

    match args.provider {
        CodeProvider::Gemini
        | CodeProvider::Openai
        | CodeProvider::Anthropic
        | CodeProvider::Deepseek
        | CodeProvider::Kimi
        | CodeProvider::Zhipu
        | CodeProvider::Ollama => {
            let (model, model_name, _) =
                build_any_completion_model_for_args(args, env_file, working_dir)?;
            Ok(Some(
                build_headless_web_code_ui_runtime(
                    args,
                    working_dir,
                    HeadlessWebSessionBootstrap {
                        store: session_store,
                        state: session_state,
                    },
                    model,
                    model_name,
                    HeadlessApprovalChannels {
                        exec_approval_tx,
                        exec_approval_rx,
                    },
                    browser_write_enabled,
                    mcp_server,
                )
                .await?,
            ))
        }
        // Codex is handled by `start_codex_code_ui_runtime` in `execute_web_only`;
        // it must never enter this dispatcher.
        CodeProvider::Codex => Ok(None),
        #[cfg(feature = "test-provider")]
        CodeProvider::Fake => {
            let (model, model_name, _) =
                build_any_completion_model_for_args(args, env_file, working_dir)?;
            Ok(Some(
                build_headless_web_code_ui_runtime(
                    args,
                    working_dir,
                    HeadlessWebSessionBootstrap {
                        store: session_store,
                        state: session_state,
                    },
                    model,
                    model_name,
                    HeadlessApprovalChannels {
                        exec_approval_tx,
                        exec_approval_rx,
                    },
                    browser_write_enabled,
                    mcp_server,
                )
                .await?,
            ))
        }
    }
}

async fn build_placeholder_web_code_ui_runtime(
    args: &CodeArgs,
    working_dir: &Path,
) -> Arc<CodeUiRuntimeHandle> {
    let capabilities = CodeUiCapabilities {
        message_input: false,
        streaming_text: false,
        plan_updates: false,
        tool_calls: false,
        patchsets: false,
        interactive_approvals: false,
        structured_questions: false,
        provider_session_resume: false,
        command_idempotency: false,
    };

    let mut snapshot = initial_snapshot(
        working_dir.to_string_lossy().to_string(),
        CodeUiProviderInfo {
            provider: format!("{:?}", args.provider).to_lowercase(),
            model: args.model.clone(),
            mode: Some("web".to_string()),
            managed: matches!(args.provider, CodeProvider::Codex),
        },
        capabilities.clone(),
    );
    let now = Utc::now();
    snapshot.status = CodeUiSessionStatus::Idle;
    snapshot.transcript.push(CodeUiTranscriptEntry {
        id: "web-ui-placeholder".to_string(),
        kind: CodeUiTranscriptEntryKind::InfoNote,
        title: Some("Web Control Unavailable".to_string()),
        content: Some(
            "The interactive web runtime for this provider could not be started; showing a read-only view. Retry, or use a provider with a supported headless runtime to drive the live session directly."
                .to_string(),
        ),
        status: Some("completed".to_string()),
        streaming: false,
        metadata: serde_json::json!({ "providerAgnostic": true }),
        created_at: now,
        updated_at: now,
    });

    CodeUiRuntimeHandle::build(
        ReadOnlyCodeUiAdapter::new(CodeUiSession::new(snapshot), capabilities),
        false,
        CodeUiInitialController::Unclaimed,
    )
    .await
}

async fn start_codex_code_ui_runtime(
    args: &CodeArgs,
    working_dir: &Path,
    ws_url: &str,
    mcp_server: Arc<LibraMcpServer>,
    browser_write_enabled: bool,
    initial_controller: CodeUiInitialController,
) -> CliResult<Arc<CodeUiRuntimeHandle>> {
    let plan_mode = effective_plan_mode(args);
    let approval_auto_accepts = matches!(
        args.approval_policy,
        CodeApprovalPolicy::Never | CodeApprovalPolicy::AllowAll
    );
    tracing::info!(
        target: "libra::internal::ai::codex",
        plan_mode,
        provider = "codex",
        approval_policy = ?args.approval_policy,
        "starting Codex code-ui runtime; plan_mode {} (defaults to true for codex provider)",
        if plan_mode { "enabled" } else { "disabled" }
    );
    if plan_mode && approval_auto_accepts {
        tracing::warn!(
            target: "libra::internal::ai::codex",
            approval_policy = ?args.approval_policy,
            "plan_mode is enabled but the approval policy auto-accepts every \
             request — Codex will produce a plan and then run it without an \
             explicit operator review. Use --approval-policy on-request to \
             keep the review gate active."
        );
    }
    let agent_args = agent_codex::AgentCodexArgs {
        url: ws_url.to_string(),
        cwd: working_dir.to_string_lossy().to_string(),
        approval: approval_policy_to_codex(args.approval_policy).to_string(),
        model_provider: None,
        service_tier: None,
        personality: None,
        model: args.model.clone(),
        plan_mode,
        debug: false,
    };

    agent_codex::start_code_ui_runtime(
        agent_args,
        mcp_server,
        browser_write_enabled,
        initial_controller,
    )
    .await
    .map_err(|error| CliError::fatal(error.to_string()))
}

// ---------------------------------------------------------------------------
// Approval policy mapping helpers
// ---------------------------------------------------------------------------

/// Maps [`CodeApprovalPolicy`] to the Codex app-server's approval string.
/// Codex only distinguishes between "accept" (auto-approve) and "ask" (prompt).
fn approval_policy_to_codex(policy: CodeApprovalPolicy) -> &'static str {
    match policy {
        CodeApprovalPolicy::Never | CodeApprovalPolicy::AllowAll => "accept",
        CodeApprovalPolicy::OnFailure
        | CodeApprovalPolicy::OnRequest
        | CodeApprovalPolicy::Untrusted => "ask",
    }
}

/// Starts the Codex app-server as a managed child process.
///
/// 1. Resolves the WebSocket URL (using the requested port or auto-selecting a free one).
/// 2. Spawns the Codex binary with `app-server --listen <ws_url>`.
/// 3. Polls the WebSocket endpoint until it becomes reachable (or times out).
///
/// On failure, the child process is killed before returning the error.
async fn start_managed_codex_server(
    codex_bin: &str,
    requested_port: Option<u16>,
    working_dir: &Path,
) -> CliResult<ManagedCodexServer> {
    let ws_url = resolve_codex_ws_url(requested_port)?;
    let child = spawn_codex_app_server(codex_bin, &ws_url, working_dir)?;
    let mut server = ManagedCodexServer {
        ws_url: ws_url.clone(),
        child,
    };

    if let Err(err) = wait_for_codex_ready(&ws_url).await {
        if let Err(cleanup_error) = server.shutdown().await {
            return Err(CliError::failure(format!(
                "Codex app-server did not become ready at {ws_url}: {err}; cleanup failed: {cleanup_error}"
            )));
        }
        return Err(err);
    }

    Ok(server)
}

/// Builds a `tokio::process::Command` for the Codex app-server.
/// Stdin/stdout/stderr are all set to null since the server communicates
/// exclusively over WebSocket.
fn build_codex_command(program: &str, ws_url: &str, working_dir: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .arg("app-server")
        .arg("--listen")
        .arg(ws_url)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

/// Windows fallback: wraps the Codex binary invocation in `cmd /C` to
/// handle `.cmd`/`.bat` shims that are common on Windows (e.g. from npm).
#[cfg(target_os = "windows")]
fn build_windows_shell_codex_command(codex_bin: &str, ws_url: &str, working_dir: &Path) -> Command {
    let mut command = Command::new("cmd");
    command
        .arg("/C")
        .arg(codex_bin)
        .arg("app-server")
        .arg("--listen")
        .arg(ws_url)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

/// Attempts to spawn the Codex app-server process. On Windows, falls back
/// to `cmd /C` if the direct spawn fails with `NotFound` (handles `.cmd` shims).
fn spawn_codex_app_server(codex_bin: &str, ws_url: &str, working_dir: &Path) -> CliResult<Child> {
    match build_codex_command(codex_bin, ws_url, working_dir).spawn() {
        Ok(child) => Ok(child),
        #[cfg(target_os = "windows")]
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            build_windows_shell_codex_command(codex_bin, ws_url, working_dir)
                .spawn()
                .map_err(|shell_err| {
                    CliError::io(format!(
                        "failed to start Codex app-server using '{}': {}. Direct spawn error: {}. Make sure the Codex CLI is installed and available in PATH.",
                        codex_bin, shell_err, err
                    ))
                })
        }
        Err(err) => Err(CliError::io(format!(
            "failed to start Codex app-server using '{}': {}. Make sure the Codex CLI is installed and available in PATH.",
            codex_bin, err
        ))),
    }
}

/// Resolves the WebSocket URL for the Codex app-server.
/// If no port is specified, auto-selects a free local port via [`pick_free_local_port`].
fn resolve_codex_ws_url(requested_port: Option<u16>) -> CliResult<String> {
    let port = match requested_port {
        Some(0) => {
            return Err(CliError::command_usage(
                "--codex-port must be a non-zero TCP port; omit it to auto-select a free port",
            ));
        }
        Some(port) => port,
        None => pick_free_local_port(DEFAULT_BIND_HOST)?,
    };
    Ok(format!("ws://{DEFAULT_BIND_HOST}:{port}"))
}

/// Binds to port 0 on the given host to let the OS assign a free ephemeral
/// port, then returns that port number. The listener is dropped immediately,
/// releasing the port for the Codex server to bind to.
fn pick_free_local_port(host: &str) -> CliResult<u16> {
    let listener = std::net::TcpListener::bind((host, 0)).map_err(|e| {
        CliError::network(format!(
            "failed to reserve a local port for the Codex app-server on {}: {}",
            host, e
        ))
    })?;
    listener.local_addr().map(|addr| addr.port()).map_err(|e| {
        CliError::network(format!(
            "failed to determine the reserved Codex app-server port: {}",
            e
        ))
    })
}

/// Polls the Codex app-server WebSocket endpoint until a connection succeeds
/// or [`CODEX_STARTUP_TIMEOUT`] is exceeded. The probe connection is immediately
/// dropped after a successful handshake.
async fn wait_for_codex_ready(ws_url: &str) -> CliResult<()> {
    wait_for_codex_ready_within(ws_url, CODEX_STARTUP_TIMEOUT).await
}

/// Poll variant with an injectable overall `timeout`, so tests can assert the
/// human-readable startup-timeout diagnostic without waiting the full
/// [`CODEX_STARTUP_TIMEOUT`]. Production always goes through
/// [`wait_for_codex_ready`].
async fn wait_for_codex_ready_within(ws_url: &str, timeout: Duration) -> CliResult<()> {
    let deadline = Instant::now() + timeout;

    loop {
        match connect_async(ws_url).await {
            Ok((stream, _)) => {
                drop(stream);
                return Ok(());
            }
            Err(err) => {
                let detail = err.to_string();
                if Instant::now() >= deadline {
                    return Err(CliError::network(format!(
                        "timed out waiting for Codex app-server at {}: {}",
                        ws_url, detail
                    )));
                }
                sleep(CODEX_STARTUP_POLL_INTERVAL).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Working directory resolution
// ---------------------------------------------------------------------------

/// Resolves the effective working directory for the code session.
///
/// Priority: `--cwd` > `--repo` > current working directory.
/// Validates that the resolved path exists and is a directory.
/// `--cwd` and `--repo` are mutually exclusive.
pub(crate) fn resolve_code_preflight_working_dir(args: &CodeArgs) -> CliResult<PathBuf> {
    resolve_code_working_dir(args)
}

fn resolve_code_working_dir(args: &CodeArgs) -> CliResult<PathBuf> {
    if args.cwd.is_some() && args.repo.is_some() {
        return Err(CliError::command_usage(
            "--cwd and --repo cannot be used together".to_string(),
        ));
    }

    let working_dir = args
        .cwd
        .clone()
        .or_else(|| args.repo.clone())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let flag = if args.repo.is_some() {
        "--repo"
    } else {
        "--cwd"
    };
    validate_code_working_dir(working_dir, flag)
}

fn validate_code_working_dir(working_dir: PathBuf, flag: &str) -> CliResult<PathBuf> {
    if !working_dir.exists() {
        return Err(CliError::command_usage(format!(
            "{flag} path does not exist: {}",
            working_dir.display()
        )));
    }
    if !working_dir.is_dir() {
        return Err(CliError::command_usage(format!(
            "{flag} must point to a directory: {}",
            working_dir.display()
        )));
    }
    Ok(working_dir)
}

fn build_tui_code_ui_transcript(session: &SessionState) -> Vec<CodeUiTranscriptEntry> {
    session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            let kind = match message.role.as_str() {
                "user" => CodeUiTranscriptEntryKind::UserMessage,
                "assistant" => CodeUiTranscriptEntryKind::AssistantMessage,
                _ => return None,
            };
            Some(CodeUiTranscriptEntry {
                id: format!("session-message-{}", index + 1),
                kind,
                title: Some(match message.role.as_str() {
                    "user" => "Developer".to_string(),
                    _ => "Assistant".to_string(),
                }),
                content: Some(message.content.clone()),
                status: Some("completed".to_string()),
                streaming: false,
                metadata: serde_json::json!({ "restored": true }),
                created_at: message.timestamp,
                updated_at: message.timestamp,
            })
        })
        .collect()
}

fn session_canonical_thread_id(session: &SessionState) -> Option<String> {
    ["thread_id", "threadId", "canonical_thread_id"]
        .iter()
        .find_map(|key| session.metadata.get(*key).and_then(|value| value.as_str()))
        .map(str::to_string)
        .or_else(|| {
            Uuid::parse_str(&session.id)
                .ok()
                .map(|thread_id| thread_id.to_string())
        })
}

// ---------------------------------------------------------------------------
// MCP server — Streamable HTTP transport via Hyper
// ---------------------------------------------------------------------------

/// Starts the MCP server using `rmcp`'s Streamable HTTP transport.
///
/// Each incoming TCP connection is handled by a Hyper service that wraps the
/// `StreamableHttpService`. Per-connection tasks are tracked in `connection_tasks`
/// so they can be aborted during shutdown, preventing task leaks.
///
/// Uses `LocalSessionManager` for session management (single-node, in-memory).
async fn start_mcp_server(
    host: &str,
    port: u16,
    mcp_server: Arc<LibraMcpServer>,
) -> anyhow::Result<McpServerHandle> {
    let addr = crate::internal::ai::web::parse_listen_addr(host, port)?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;

    // Use rmcp's Streamable HTTP transport via Hyper directly
    let service = TowerToHyperService::new(StreamableHttpService::new(
        move || Ok(mcp_server.clone()),
        LocalSessionManager::default().into(),
        Default::default(),
    ));

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let connection_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let tracked_connections = connection_tasks.clone();

    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    break;
                }
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _)) => {
                            let io = TokioIo::new(stream);
                            let service = service.clone();
                            let conn_task = tokio::spawn(async move {
                                if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::default())
                                    .serve_connection(io, service)
                                    .await
                                {
                                    cli_error!(e, "warning: MCP connection error");
                                }
                            });
                            match tracked_connections.lock() {
                                Ok(mut tasks) => {
                                    tasks.retain(|task| !task.is_finished());
                                    tasks.push(conn_task);
                                }
                                Err(_) => conn_task.abort(),
                            }
                        }
                        Err(e) => {
                            cli_error!(e, "warning: MCP accept error");
                        }
                    }
                }
            }
        }
        Ok(())
    });

    Ok(McpServerHandle {
        addr: bound_addr,
        shutdown_tx,
        join,
        connection_tasks,
    })
}

// ---------------------------------------------------------------------------
// System prompt and runtime context construction
// ---------------------------------------------------------------------------

/// Builds the system prompt (preamble) for the AI agent, incorporating the
/// working directory context and optional operating mode (dev/review/research).
fn system_preamble(
    working_dir: &std::path::Path,
    context: Option<CodeContext>,
    provider: CodeProvider,
    model: Option<&str>,
) -> Result<String, String> {
    let intent = task_intent_for_context(context);
    let budget = ContextBudget::for_provider_model(
        context_budget_provider_name(provider),
        model.unwrap_or_else(|| default_context_budget_model(provider)),
    );
    let mut builder = SystemPromptBuilder::new(working_dir)?
        .with_intent(intent)
        .with_dynamic_context()
        .with_context_budget(budget);
    if let Some(ctx) = context {
        let mode = match ctx {
            CodeContext::Dev => ContextMode::Dev,
            CodeContext::Review => ContextMode::Review,
            CodeContext::Research => ContextMode::Research,
        };
        builder = builder.with_context(mode);
    }
    builder.build()
}

fn context_budget_provider_name(provider: CodeProvider) -> &'static str {
    match provider {
        CodeProvider::Gemini => "gemini",
        CodeProvider::Openai => "openai",
        CodeProvider::Anthropic => "anthropic",
        CodeProvider::Deepseek => "deepseek",
        CodeProvider::Kimi => "kimi",
        CodeProvider::Zhipu => "zhipu",
        CodeProvider::Ollama => "ollama",
        CodeProvider::Codex => "codex",
        #[cfg(feature = "test-provider")]
        CodeProvider::Fake => "fake",
    }
}

fn default_context_budget_model(provider: CodeProvider) -> &'static str {
    match provider {
        CodeProvider::Gemini => GEMINI_2_5_FLASH,
        CodeProvider::Openai => GPT_4O_MINI,
        CodeProvider::Anthropic => CLAUDE_3_5_SONNET,
        CodeProvider::Deepseek => "deepseek-chat",
        CodeProvider::Kimi => KIMI_K2_6,
        CodeProvider::Zhipu => GLM_5,
        CodeProvider::Ollama => "ollama-default",
        CodeProvider::Codex => "codex",
        #[cfg(feature = "test-provider")]
        CodeProvider::Fake => FAKE_DEFAULT_MODEL,
    }
}

fn task_intent_for_context(context: Option<CodeContext>) -> TaskIntent {
    match context {
        Some(CodeContext::Dev) => TaskIntent::Feature,
        Some(CodeContext::Review) => TaskIntent::Review,
        Some(CodeContext::Research) => TaskIntent::Question,
        None => TaskIntent::Unknown,
    }
}

/// Constructs the default [`ToolRuntimeContext`] for the Code UI runtime,
/// configuring
/// the sandbox policy based on the operating context:
///
/// - **Dev mode (or no context)**: Workspace-write sandbox allowing modifications
///   within the working directory; network access follows the developer's
///   selected policy.
/// - **Review / Research mode**: Read-only sandbox; no writes or network access.
///
/// The approval policy and its communication channel are also wired in here.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DefaultApprovalConfig {
    policy: AskForApproval,
    allow_all_commands: bool,
    ttl: Duration,
    cache_policy: ApprovalCachePolicy,
}

fn default_runtime_context(
    working_dir: &std::path::Path,
    context: Option<CodeContext>,
    approval: DefaultApprovalConfig,
    network_access: bool,
    exec_approval_tx: tokio::sync::mpsc::UnboundedSender<ExecApprovalRequest>,
    approval_cache_scope: impl Into<String>,
) -> ToolRuntimeContext {
    let sandbox_profile = match context {
        Some(CodeContext::Review | CodeContext::Research) => CodeAgentSandboxProfile::ReadOnly,
        Some(CodeContext::Dev) | None => CodeAgentSandboxProfile::WorkspaceWrite { network_access },
    };
    tool_runtime_context(
        working_dir,
        sandbox_profile,
        CodeAgentApprovalConfig {
            policy: approval.policy,
            allow_all_commands: approval.allow_all_commands,
            ttl: approval.ttl,
            cache_policy: approval.cache_policy,
        },
        exec_approval_tx,
        approval_cache_scope,
    )
}

async fn hydrate_approval_runtime(
    working_dir: &Path,
    mut approval: DefaultApprovalConfig,
) -> Result<(DefaultApprovalConfig, String), String> {
    match resolve_approval_runtime_cache(working_dir).await {
        Ok(cache) => {
            approval.cache_policy.approved_ruleset = Some(cache.approved_ruleset);
            Ok((approval, cache.scope_key))
        }
        Err(ApprovalRuntimeCacheError::NotARepository(_)) => {
            Ok((approval, unbound_approval_cache_scope(working_dir)))
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Single source of truth for the approval-related CLI-args -> runtime
/// [`DefaultApprovalConfig`] mapping (C7 criterion 2): `--approval-policy`
/// maps through `.into()`, `--approval-ttl` through `Duration::from_secs`
/// (CLI flag wins over the project `approval.ttl`, else `DEFAULT_APPROVAL_TTL`),
/// and `--approval-policy` also drives `allow_all_commands`. The active Web and
/// headless launch paths derive their approval config from here, so a dropped or
/// hardcoded flag is a single-point regression the unit test guards.
fn approval_config_from_args(
    args: &CodeArgs,
    working_dir: &Path,
) -> Result<DefaultApprovalConfig, String> {
    let approval_config = load_approval_project_config(working_dir)?;
    Ok(DefaultApprovalConfig {
        policy: args.approval_policy.into(),
        allow_all_commands: args.approval_policy.allows_all_commands(),
        ttl: args
            .approval_ttl
            .map(Duration::from_secs)
            .or(approval_config.ttl)
            .unwrap_or(DEFAULT_APPROVAL_TTL),
        cache_policy: approval_config.cache_policy,
    })
}

#[cfg(test)]
fn approval_ttl_from_project_config(working_dir: &Path) -> Option<Duration> {
    load_approval_project_config(working_dir)
        .expect("approval config")
        .ttl
}

#[cfg(test)]
fn approval_cache_policy_from_project_config(working_dir: &Path) -> ApprovalCachePolicy {
    load_approval_project_config(working_dir)
        .expect("approval config")
        .cache_policy
}

// ---------------------------------------------------------------------------
// MCP server initialization — storage and database setup
// ---------------------------------------------------------------------------

/// Initializes the [`LibraMcpServer`] instance with optional history persistence.
///
/// Sets up the local object storage directory and SQLite database under the
/// `.libra/` storage root. If any step fails (directory creation, DB connection),
/// falls back to a read-only MCP server with history disabled, printing a warning.
///
/// # Side Effects
/// - Creates the local object storage directory when possible.
/// - Opens a SQLite connection for intent/run history when the DB path is usable.
/// - Prints warnings to stderr before falling back to history-disabled mode.
///
/// # Errors
/// This helper intentionally does not return errors. It converts storage/DB
/// setup failures into a read-only MCP server so AI clients can still inspect
/// files and continue a degraded session.
async fn init_mcp_server(working_dir: &std::path::Path) -> Arc<LibraMcpServer> {
    // §C.4.1: an unresolvable storage root degrades to the SAME read-only,
    // history-disabled server this function already falls back to when the
    // directory or database cannot be opened — never to a phantom
    // `<working_dir>/.libra` that would start accumulating real history.
    let Some(storage_dir) = resolve_storage_root(working_dir) else {
        eprintln!(
            "Warning: cannot resolve the repository storage root for {}. Running in read-only \
             mode (history/context disabled). If this is a linked worktree, run `libra worktree \
             repair <worktree-path> --confirm`.",
            working_dir.display()
        );
        return Arc::new(LibraMcpServer::new_with_working_dir(
            None,
            None,
            working_dir.to_path_buf(),
        ));
    };
    let objects_dir = storage_dir.join("objects");
    let dot_libra = storage_dir;

    // Try to create the directory. If it fails, we assume read-only or permission issues.
    if let Err(e) = std::fs::create_dir_all(&objects_dir) {
        eprintln!(
            "Warning: Failed to create storage directory: {}. Running in read-only mode (history/context disabled). Error: {}",
            objects_dir.display(),
            e
        );
        return Arc::new(LibraMcpServer::new_with_working_dir(
            None,
            None,
            working_dir.to_path_buf(),
        ));
    }

    // Connect to DB
    let db_path = dot_libra.join("libra.db");
    let Some(db_path_str) = db_path.to_str() else {
        eprintln!(
            "Warning: Database path is not valid UTF-8: {}. History disabled.",
            db_path.display()
        );
        return Arc::new(LibraMcpServer::new_with_working_dir(
            None,
            None,
            working_dir.to_path_buf(),
        ));
    };

    #[cfg(target_os = "windows")]
    let db_path_string = db_path_str.replace("\\", "/");
    #[cfg(target_os = "windows")]
    let db_path_str = &db_path_string;

    let db_conn = match establish_connection(db_path_str).await {
        Ok(conn) => Arc::new(conn),
        Err(e) => {
            eprintln!(
                "Warning: Failed to connect to database: {}. History disabled.",
                e
            );
            return Arc::new(LibraMcpServer::new_with_working_dir(
                None,
                None,
                working_dir.to_path_buf(),
            ));
        }
    };

    let storage = Arc::new(ClientStorage::init(objects_dir));
    let intent_history_manager = Arc::new(HistoryManager::new(storage.clone(), dot_libra, db_conn));
    Arc::new(LibraMcpServer::new_with_working_dir(
        Some(intent_history_manager),
        Some(storage),
        working_dir.to_path_buf(),
    ))
}

/// [`resolve_storage_root`], but for the paths that genuinely cannot proceed
/// without a storage root. `libra code`'s own CLI preflight already resolved
/// one, so a failure here means the repository changed underneath the process
/// — an actionable refusal, not a new repository beside the old one.
pub(crate) fn require_storage_root(working_dir: &std::path::Path) -> CliResult<std::path::PathBuf> {
    resolve_storage_root(working_dir).ok_or_else(|| {
        CliError::fatal(format!(
            "cannot resolve the repository storage root for '{}'",
            working_dir.display()
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::RepoStateInvalid)
        .with_hint(
            "if this is a linked worktree, run `libra worktree repair --confirm \
             <worktree-path>` from the main worktree",
        )
    })
}

/// The repository storage root for `working_dir`, or `None` when it cannot be
/// resolved.
///
/// Part C §C.4.1 forbids the caller-side fallback this used to perform. On a
/// linked worktree with a corrupt or empty `commondir`, minting
/// `<working_dir>/.libra` does not degrade the session — it CREATES a second,
/// phantom repository: a fresh `libra.db` and `objects/` beside the real ones,
/// which then accumulate history, approvals and captured sessions that the
/// actual repository never sees. A warning does not make that safe, because
/// the damage is silent and the writes are real.
///
/// Callers degrade instead: no storage root means no history and no object
/// store, which is the same read-only mode they already fall back to when the
/// directory or database cannot be opened.
pub(crate) fn resolve_storage_root(working_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    match try_get_storage_path(Some(working_dir.to_path_buf())) {
        Ok(root) => Some(root),
        Err(error) => {
            tracing::warn!(
                working_dir = %working_dir.display(),
                %error,
                "storage-root resolution failed; continuing WITHOUT a storage root rather than \
                 minting a phantom one — if this is a linked worktree, run `libra worktree \
                 repair <worktree-path> --confirm`"
            );
            None
        }
    }
}

/// CEX-S2-12 "single sub-agent behind flag" concurrency cap.
///
/// While the `code.sub_agents.enabled` gate is the only path that
/// builds a [`SubAgentToolLoopRuntime`], CEX-S2-12 must run at most one
/// concurrent sub-agent regardless of the operator-configured
/// `code.multi_agent.max_concurrent_subagents` (and the
/// `code.sub_agents.max_parallel` schema default of `2`). Real
/// parallelism stays locked until CEX-S2-14 wires the scheduler-side
/// observer budget — at which point this returns `configured` instead
/// of the forced `1`.
///
/// Kept as a named pure function (rather than a literal `1` at the call
/// site) so the cap is documented, greppable, and pinned by a unit test
/// against a silent regression to passing the operator value through.
const fn cex_s2_12_subagent_concurrency_cap(_configured: u32) -> u32 {
    1
}

/// Construct a [`SubAgentToolLoopRuntime`] from the libra-code
/// session's resolved state. Called from the session bootstrap
/// when `agents_config.sub_agents.enabled = true`; failures
/// degrade to "task tool unavailable" rather than blocking
/// session startup.
///
/// The runtime is shared (cloned by `Option<...>::clone()` since
/// every field is `Arc`-wrapped or trivially copyable inside its
/// own owning newtype). Per-call `dispatch_context(call_id)`
/// captures a fresh `parent_message_id` for each `task` tool
/// invocation; the rest of the parent context is stable for the
/// session.
#[allow(clippy::too_many_arguments)]
async fn build_subagent_runtime_for_session(
    agents_config: &AgentsConfig,
    registry: std::sync::Arc<ToolRegistry>,
    session: &SessionState,
    session_store: &SessionStore,
    storage_root: &Path,
    model_name: &str,
    provider_name: &str,
    agent_router: &AgentProfileRouter,
    hook_runner: Option<std::sync::Arc<crate::internal::ai::hooks::HookRunner>>,
    runtime_context: Option<ToolRuntimeContext>,
) -> anyhow::Result<crate::internal::ai::agent::runtime::SubAgentToolLoopRuntime> {
    use crate::internal::ai::{
        agent::{
            profile::{AgentExecutionSpec, AgentMode, ModelBinding},
            runtime::{
                AbortToken, ChannelPermissionAsker, ContextFrameLoader, DefaultSubAgentDispatcher,
                MultiAgentConfig, PermissionAsker, PermissionReply, PermissionService,
                SubAgentToolLoopRuntime,
            },
        },
        providers::{ProviderBuildOptions, ProviderFactory},
        session::jsonl::SessionJsonlStore,
    };

    let agent_spec_registry = agents_config
        .build_agent_registry()
        .map_err(|err| anyhow::anyhow!("agents.toml validation failed: {err}"))?;

    let dispatcher = DefaultSubAgentDispatcher::new(
        agent_spec_registry,
        MultiAgentConfig {
            enabled: agents_config.multi_agent.enabled,
            // `agents_config.multi_agent` carries u32 for both
            // limits to preserve TOML round-trip; the runtime's
            // `MultiAgentConfig` narrows depth to u8 (a depth of
            // 256+ is meaningless — that's a recursion bug not a
            // legitimate config). Saturating cast keeps the
            // semantics safe when an operator sets a huge u32.
            max_subagent_depth: agents_config
                .multi_agent
                .max_subagent_depth
                .min(u8::MAX as u32) as u8,
            // CEX-S2-12 "single sub-agent behind flag": force the
            // dispatcher concurrency to 1 regardless of the configured
            // value; CEX-S2-14 unlocks the operator's real budget.
            max_concurrent_subagents: cex_s2_12_subagent_concurrency_cap(
                agents_config.multi_agent.max_concurrent_subagents,
            ),
        },
    )
    .with_default_child_runner()
    // CEX-S2-12 / S2-INV-03: confine each dispatched sub-agent to a
    // materialized per-run workspace so its writes never touch the main
    // worktree. `sessions_root` = the `.libra/sessions` dir the per-run
    // `AgentRunEventStore` records the `WorkspaceMaterialized` event
    // under (transcript path `sessions_root/{thread}/agents/{run}.jsonl`).
    .with_workspace_isolation(
        crate::internal::ai::agent::runtime::WorkspaceIsolationConfig {
            fuse_state: crate::internal::ai::orchestrator::workspace::FuseProvisionState::default(),
            sessions_root: storage_root.join("sessions"),
            allow_full_copy: agents_config.multi_agent.allow_full_copy,
        },
    );

    // OC-Phase 3 P3.4 / P3.7 interactive permission asker (v0.17.788):
    // construct a ChannelPermissionAsker + spawn a background
    // consumer task that auto-rejects each ask while emitting a
    // structured tracing event with the full ask context. This is
    // the channel-plumbing wire-up that proves the path end-to-end;
    // the follow-up replaces the auto-reject consumer with a Web Code UI
    // permission widget that surfaces each ask interactively.
    //
    // The consumer task lives for the entire session — when the
    // session exits, the sender drops, the receiver's `recv()`
    // returns None, and the task ends cleanly.
    let (permission_ask_tx, mut permission_ask_rx) = tokio::sync::mpsc::unbounded_channel::<
        crate::internal::ai::agent::runtime::ChannelPermissionAsk,
    >();
    tokio::spawn(async move {
        while let Some(ask) = permission_ask_rx.recv().await {
            tracing::warn!(
                permission = %ask.permission,
                patterns = ?ask.patterns,
                thread_id = %ask.thread_id,
                session_id = %ask.session_id,
                source = ?ask.source,
                "permission ask received via ChannelPermissionAsker; \
                 auto-rejecting until interactive Web Code UI permission widget lands",
            );
            // Send may fail if the dispatcher dropped its
            // oneshot receiver (e.g. cancelled mid-await). Ignore
            // the send error — the dispatcher already handles a
            // closed reply channel by surfacing Reject.
            let _ = ask.reply_tx.send(PermissionReply::Reject {
                feedback: Some(
                    "permission ask auto-rejected by the v0.17.788 channel consumer; \
                     pre-grant the permission via [code.agents.<name>.permission] in \
                     .libra/agents.toml or wait for the interactive Web Code UI permission widget"
                        .to_string(),
                ),
            });
        }
    });
    let permission_service = PermissionService::new(std::sync::Arc::new(
        ChannelPermissionAsker::new(permission_ask_tx),
    ) as std::sync::Arc<dyn PermissionAsker>);

    let parent_model_binding = ModelBinding::parse(&format!("{provider_name}/{model_name}"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "failed to parse parent ModelBinding from provider={provider_name} model={model_name}"
            )
        })?;

    // OC-Phase 3 P3.4 router-resolved parent_agent (v0.17.780):
    // if the operator has authored a `.libra/agents/primary.md`
    // (or any `.md` profile named "primary"), use it as the
    // sub-agent dispatcher's parent_agent. The CLI flags still
    // win for the model binding because the operator's `libra
    // code --model <X>` should override the profile's default
    // model — sub-agents inherit the session's actual model, not
    // the profile's static one. Falls back to the v0.17.776
    // placeholder when no profile is found.
    let parent_agent = match agent_router.execution_spec("primary") {
        Some(mut spec) => {
            // The router-supplied spec carries the profile's
            // declared model binding, but the session's actual
            // model is what the CLI resolved — sub-agents should
            // see the same model the parent is talking to, not
            // the profile's default.
            spec.model = Some(parent_model_binding.clone());
            spec
        }
        None => AgentExecutionSpec {
            name: "parent".to_string(),
            description: "libra-code primary agent (session bootstrap default)".to_string(),
            mode: AgentMode::Primary,
            model: Some(parent_model_binding.clone()),
            ..AgentExecutionSpec::default()
        },
    };

    let session_jsonl_store = SessionJsonlStore::new(session_store.session_root(&session.id));
    let usage_recorder =
        std::sync::Arc::new(build_usage_recorder(storage_root).await.ok_or_else(|| {
            anyhow::anyhow!(
                "usage recorder unavailable; sub-agent dispatcher requires the SQLite DB \
                 — check storage_root permissions"
            )
        })?);
    let context_frame_loader = std::sync::Arc::new(ContextFrameLoader::default());

    // OC-Phase 4 P4.4 compaction model (v0.17.784): when the
    // operator configured `[code.compaction]`, build a
    // `CompletionModel` for it so the dispatcher tail can route
    // parent frames through `run_compaction(...)`. Failures
    // here degrade to None — the v0.17.773 raw-segment handoff
    // path stays operational. We log + warn on failure rather
    // than aborting the whole runtime construction so a
    // misconfigured compaction model doesn't break operators
    // who have correctly configured sub-agents.
    let compaction_model = match agents_config.compaction_model_binding() {
        Some(binding) => match ProviderFactory.build(&binding, ProviderBuildOptions::default()) {
            Ok(model) => Some(std::sync::Arc::new(model)),
            Err(err) => {
                tracing::warn!(
                    %err,
                    provider = %binding.provider_id,
                    model = %binding.model_id,
                    "failed to build compaction model from [code.compaction]; \
                     falling back to raw-segment handoff",
                );
                None
            }
        },
        None => None,
    };

    Ok(SubAgentToolLoopRuntime {
        dispatcher: std::sync::Arc::new(dispatcher),
        parent_thread_id: session_canonical_thread_id(session)
            .unwrap_or_else(|| session.id.clone()),
        parent_turn_id: None,
        parent_session_id: session.id.clone(),
        parent_agent,
        parent_ruleset: Vec::new(),
        parent_model_binding,
        permission_service: std::sync::Arc::new(permission_service),
        session_store: session_jsonl_store,
        provider_factory: std::sync::Arc::new(ProviderFactory),
        provider_build_options: ProviderBuildOptions::default(),
        provider_build_options_resolver: None,
        tool_registry: (*registry).clone(),
        // S2-INV-06: hand the child the parent session's resolved
        // runtime sandbox / approval / file-history authority so its
        // tool invocations run under the same gates the parent does.
        // `DefaultSubAgentChildRunner::run` forwards this into the
        // child's `ToolLoopConfig.runtime_context`; before it was
        // populated here the child ran every tool call with `None`
        // (no sandbox, approval defaulting to `Skip`) — strictly more
        // permissive than the parent. This is authority *inheritance*,
        // not workspace *isolation* (S2-INV-03): the child still shares
        // the parent's `writable_roots`; rebasing those onto a
        // materialized per-run workspace is a separate follow-on.
        runtime_context,
        compaction_model,
        usage_recorder,
        context_frame_loader,
        abort_token: AbortToken::new(),
        depth: 0,
        // v0.17.807 S2-INV-13 hook dispatch: the parent's
        // `HookRunner` (loaded at `code.rs:2554` via
        // `HookRunner::load(...)`) is now threaded through here
        // so child sub-agents inherit the same PreToolUse /
        // PostToolUse hook surface as the parent. Sub-agents
        // cannot disable or supersede the parent's runner.
        hook_runner,
    })
}

async fn build_usage_recorder(storage_root: &Path) -> Option<UsageRecorder> {
    let db_path = storage_root.join(DATABASE);
    let Some(db_path) = db_path.to_str() else {
        tracing::warn!(
            path = %storage_root.display(),
            "usage stats disabled because the repository database path is not valid UTF-8"
        );
        return None;
    };
    match establish_connection(db_path).await {
        Ok(conn) => {
            let pricing = usage_price_table_from_project_config(storage_root);
            Some(UsageRecorder::with_pricing(conn, pricing))
        }
        Err(error) => {
            tracing::warn!("usage stats disabled because database open failed: {error}");
            None
        }
    }
}

async fn canonical_usage_repo_id(usage_recorder: Option<&UsageRecorder>) -> Option<String> {
    let usage_recorder = usage_recorder?;
    match usage_recorder.canonical_repo_id().await {
        Ok(repo_id) => repo_id,
        Err(error) => {
            tracing::warn!(
                %error,
                "usage stats will omit repository attribution because libra.repoid could not be read"
            );
            None
        }
    }
}

fn usage_price_table_from_project_config(storage_root: &Path) -> UsagePriceTable {
    let path = storage_root.join("config.toml");
    let Ok(contents) = fs::read_to_string(&path) else {
        return UsagePriceTable::new();
    };
    match UsagePriceTable::from_project_config_toml(&contents) {
        Ok(pricing) => pricing,
        Err(error) => {
            tracing::warn!(
                target: "libra::command::code",
                path = %path.display(),
                error = %error,
                "failed to parse usage pricing config; using built-in pricing table"
            );
            UsagePriceTable::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Mode: Stdio — MCP server over stdin/stdout
// ---------------------------------------------------------------------------

/// Runs the MCP server over stdin/stdout using `rmcp`'s async read/write
/// transport. This mode is designed for integration with AI clients (e.g.
/// Claude Desktop) that communicate via the Model Context Protocol over pipes.
///
/// Blocks until the MCP session ends (client disconnects or EOF on stdin).
///
/// # Side Effects
/// - Takes ownership of process stdin/stdout for the MCP transport.
/// - Initializes the same history/object-backed MCP server used by other modes.
///
/// # Errors
/// Returns [`CliError`] when working-dir resolution fails, the MCP server cannot
/// start on stdio, or the running MCP session reports an unrecoverable error.
async fn execute_stdio(args: &CodeArgs) -> CliResult<()> {
    warn_deprecated_mcp_stdio();
    let working_dir = resolve_code_working_dir(args)?;

    let mcp_server = init_mcp_server(&working_dir).await;

    use rmcp::{
        service::serve_server,
        transport::{async_rw::AsyncRwTransport, io::stdio},
    };

    let (stdin, stdout) = stdio();
    let transport = AsyncRwTransport::new_server(stdin, stdout);

    match serve_server(mcp_server, transport).await {
        Ok(running) => {
            if let Err(e) = running.waiting().await {
                return Err(CliError::internal(format!("MCP Stdio server error: {}", e)));
            }
        }
        Err(e) => {
            return Err(CliError::network(format!(
                "failed to start MCP Stdio server: {e}"
            )));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// CLI argument validation
// ---------------------------------------------------------------------------

/// Validates CLI flag combinations across the operating modes.
///
/// Enforces constraints such as:
/// - Web and MCP ports must differ (except in stdio mode).
/// - `--stdio` (MCP transport) rejects provider/model/api-base/temperature and
///   the provider-specific tuning flags — it has no provider surface.
/// - The default Web launch relaxes provider/model/api-base/temperature, the
///   provider-specific tuning flags, `--resume` (non-Codex), `--env-file`,
///   `--context`, `--approval-policy`, and `--approval-ttl` so they feed the
///   headless web runtime. It still rejects `--network-access allow` until the
///   Plan network-policy gate owns per-execution sandbox network (see
///   [`reject_non_tui_flags`]).
/// - Provider-specific flags are only accepted for their respective providers.
fn validate_mode_args(args: &CodeArgs, _output: &OutputConfig) -> Result<(), String> {
    if args.control != ControlMode::Stdio && args.control_url.is_some() {
        return Err(
            "`--control-url` is only supported with `--control stdio` (client-only JSON-RPC automation)"
                .to_string(),
        );
    }

    if !args.stdio && args.port == args.mcp_port && args.port != 0 {
        return Err(format!(
            "--port ({}) and --mcp-port ({}) must be different",
            args.port, args.mcp_port
        ));
    }

    // OC-Phase 6 P6.5: validate `--goal "<objective>"` against the
    // same shape rules `GoalSpec::new` enforces (opencode.md
    // lines 538-556). Surfacing the failure at CLI parse keeps the
    // supervisor (P6.3) from booting against a malformed objective
    // and gives the user a precise error string instead of a panic
    // at session-start.
    if let Some(objective) = args.goal.as_deref() {
        use crate::internal::ai::goal::MAX_OBJECTIVE_LEN;
        if objective.trim().is_empty() {
            return Err("--goal requires a non-empty objective string (e.g. \
                 `--goal \"ship feature X\"`)"
                .to_string());
        }
        if objective.len() > MAX_OBJECTIVE_LEN {
            return Err(format!(
                "--goal objective is {} bytes which exceeds the {}-byte cap; \
                 shorten the objective and add detail through the model's \
                 first turn or `/goal criteria add <text>`",
                objective.len(),
                MAX_OBJECTIVE_LEN,
            ));
        }
    }

    if code_uses_web_launch(args) {
        // Web launch (the default): relax provider/model/api-base/temperature
        // and the provider-specific tuning flags (they feed the headless web
        // runtime and still pass through the cross-provider match gate below).
        reject_non_tui_flags(args, "the Web Code UI", true)?;
        // W5-06: the legacy TUI resume driver is removed and managed Codex
        // Web resume has not landed (W4-01 residual), so bare
        // `--provider codex --resume` must fail closed with a migration hint
        // instead of silently starting a fresh, unresumed Web session.
        if args.provider == CodeProvider::Codex && args.resume.is_some() {
            return Err(
                "--resume is not supported with --provider=codex: the legacy TUI resume driver was removed in the W5 breaking release and managed Codex Web resume has not landed yet; start a new session with `libra code --provider codex` (omit --resume), or resume the thread with a non-Codex provider"
                    .to_string(),
            );
        }
        // Managed Codex web owns its own credential/approval surface; these
        // Legacy/headless flags are accepted for non-Codex web but must not be
        // silently ignored under Codex.
        if args.provider == CodeProvider::Codex && args.env_file.is_some() {
            return Err(
                "--env-file is not supported with the Web Code UI and --provider=codex; remove --env-file or use a non-Codex headless provider"
                    .to_string(),
            );
        }
        if args.provider == CodeProvider::Codex && args.approval_ttl.is_some() {
            return Err(
                "--approval-ttl is not supported with the Web Code UI and --provider=codex; remove --approval-ttl or use a non-Codex headless provider"
                    .to_string(),
            );
        }
    }

    if args.control == ControlMode::Stdio {
        // Client-only JSON-RPC NDJSON shim. Must not mix with MCP `--stdio`,
        // Web launch, or provider/host boot flags (W4-02 conflict matrix).
        if args.stdio {
            return Err(
                "`libra code --stdio` is the deprecated MCP-only legacy transport (tools/resources; not turn control); use `libra code --control stdio` for the JSON-RPC automation client (a dedicated `libra mcp --stdio` is planned after W5)"
                    .to_string(),
            );
        }
        // URL/token may be omitted: W4-10 discovers them from --control-info-file
        // (default .libra/code/control.json) + worktree control-token. Explicit
        // --control-url / --control-token-file still override.
        if args.browser_control.is_some() {
            return Err(
                "`--browser-control` is not supported with `--control stdio` (client-only; no Web launch)"
                    .to_string(),
            );
        }
        reject_non_tui_flags(args, "--control stdio", false)?;
        reject_mode_flag(args.host != DEFAULT_BIND_HOST, "--host", "--control stdio")?;
        reject_mode_flag(args.port != DEFAULT_WEB_PORT, "--port", "--control stdio")?;
        reject_mode_flag(
            args.mcp_port != DEFAULT_MCP_PORT,
            "--mcp-port",
            "--control stdio",
        )?;
        reject_mode_flag(args.goal.is_some(), "--goal", "--control stdio")?;
        reject_mode_flag(args.agent.is_some(), "--agent", "--control stdio")?;
        reject_mode_flag(args.cwd.is_some(), "--cwd", "--control stdio")?;
        reject_mode_flag(args.repo.is_some(), "--repo", "--control stdio")?;
        if args.codex_port.is_some() {
            return Err("--codex-port is not supported with `--control stdio`".to_string());
        }
        if args.codex_bin != DEFAULT_CODEX_BIN {
            return Err("--codex-bin is not supported with `--control stdio`".to_string());
        }
        if args.plan_mode.is_some() {
            return Err("--plan-mode is not supported with `--control stdio`".to_string());
        }
        // Skip the rest of the launch-mode gates (provider match, write host, …).
        return Ok(());
    }

    if args.stdio {
        if args.control == ControlMode::Write {
            return Err(
                "--control write is not supported with `libra code --stdio` because --stdio is the deprecated MCP-only legacy transport (tools/resources; not turn control); use `libra code --control stdio` for the JSON-RPC automation client (a dedicated `libra mcp --stdio` is planned after W5)"
                    .to_string(),
            );
        }
        // --stdio is the MCP transport with no provider surface, so it stays
        // fully locked on provider/model/api-base and the provider-specific
        // flags (web_launch = false below).
        reject_non_tui_flags(args, "--stdio", false)?;
        reject_mode_flag(args.host != DEFAULT_BIND_HOST, "--host", "--stdio")?;
        reject_mode_flag(args.port != DEFAULT_WEB_PORT, "--port", "--stdio")?;
        reject_mode_flag(args.mcp_port != DEFAULT_MCP_PORT, "--mcp-port", "--stdio")?;
    }

    if args.control == ControlMode::Write {
        ensure_loopback_control_host_for_validation(&args.host)?;
    }

    if args.provider != CodeProvider::Codex {
        if args.codex_port.is_some() {
            return Err("--codex-port is only supported with --provider=codex".to_string());
        }
        if args.codex_bin != DEFAULT_CODEX_BIN {
            return Err("--codex-bin is only supported with --provider=codex".to_string());
        }
        if matches!(args.plan_mode, Some(true)) {
            return Err("--plan-mode is only supported with --provider=codex".to_string());
        }
    }

    if args.provider == CodeProvider::Codex && args.api_base.is_some() {
        return Err("--api-base is not supported with --provider=codex".to_string());
    }
    if let Some(base_url) = args.api_base.as_deref() {
        match Url::parse(base_url) {
            Ok(u) if u.scheme() == "http" || u.scheme() == "https" => {}
            Ok(u) => {
                return Err(format!(
                    "--api-base must use http or https (got {})",
                    u.scheme()
                ));
            }
            Err(e) => {
                return Err(format!("--api-base is not a valid URL: {e}"));
            }
        }
    }

    // Temperature is mode-independent: the C2 web-only relaxation lets
    // `--temperature` reach the headless `ToolLoopConfig` directly, so its
    // documented 0.0–2.0 contract must be enforced here rather than relying on
    // the legacy-mode reject that previously masked out-of-range values (codex C2
    // review). NaN/inf are rejected too — they would silently corrupt sampling.
    if let Some(temperature) = args.temperature
        && (!temperature.is_finite() || !(0.0..=2.0).contains(&temperature))
    {
        return Err(format!(
            "--temperature must be a finite value between 0.0 and 2.0 (got {temperature})"
        ));
    }

    if args.provider != CodeProvider::Ollama && args.ollama_thinking.is_some() {
        return Err(
            "--ollama-thinking/--thinking is only supported with --provider=ollama".to_string(),
        );
    }

    if args.provider != CodeProvider::Ollama && args.ollama_compact_tools {
        return Err("--ollama-compact-tools is only supported with --provider=ollama".to_string());
    }

    if args.provider != CodeProvider::Deepseek && args.deepseek_thinking.is_some() {
        return Err("--deepseek-thinking is only supported with --provider=deepseek".to_string());
    }

    if args.provider != CodeProvider::Deepseek && args.deepseek_reasoning_effort.is_some() {
        return Err(
            "--deepseek-reasoning-effort is only supported with --provider=deepseek".to_string(),
        );
    }

    if args.provider != CodeProvider::Deepseek && args.deepseek_stream.is_some() {
        return Err(
            "--deepseek-stream/--stream is only supported with --provider=deepseek".to_string(),
        );
    }

    if args.provider != CodeProvider::Kimi && args.kimi_thinking.is_some() {
        return Err("--kimi-thinking is only supported with --provider=kimi".to_string());
    }

    if args.provider != CodeProvider::Kimi && args.kimi_stream.is_some() {
        return Err("--kimi-stream is only supported with --provider=kimi".to_string());
    }

    #[cfg(feature = "test-provider")]
    {
        if args.provider == CodeProvider::Fake {
            if std::env::var_os("LIBRA_ENABLE_TEST_PROVIDER").is_none() {
                return Err(
                    "--provider=fake is test-only; set LIBRA_ENABLE_TEST_PROVIDER=1 to use it"
                        .to_string(),
                );
            }
            if args.fake_fixture.is_none() {
                return Err("--fake-fixture is required with --provider=fake".to_string());
            }
        } else if args.fake_fixture.is_some() {
            return Err("--fake-fixture is only supported with --provider=fake".to_string());
        }
    }

    Ok(())
}

/// Helper: rejects a flag if it was set (`is_invalid == true`) with a
/// standardized error message indicating the flag is not supported in the given
/// mode. The message names the offending flag and the mode and gives an
/// actionable next step so the user is not left guessing.
fn reject_mode_flag(is_invalid: bool, flag: &str, mode: &str) -> Result<(), String> {
    if is_invalid {
        return Err(format!(
            "{flag} is not supported in {mode} mode; remove {flag} and rerun"
        ));
    }
    Ok(())
}

fn ensure_loopback_control_host_for_validation(host: &str) -> Result<(), String> {
    let normalized = host.trim().trim_matches('[').trim_matches(']');
    let is_loopback = matches!(normalized, "localhost" | "127.0.0.1" | "::1")
        || normalized
            .parse::<std::net::IpAddr>()
            .map(|addr| addr.is_loopback())
            .unwrap_or(false);

    if is_loopback {
        Ok(())
    } else {
        Err("--control write requires a loopback --host such as 127.0.0.1 or ::1".to_string())
    }
}

/// Rejects legacy-interactive flags that are invalid in a non-interactive mode.
///
/// Two non-interactive modes reach this helper — the default Web launch and `--stdio`
/// (plus the client-only `--control stdio` shim) — and they receive DIFFERENT
/// relaxations (plan.md Task C2). The `web_launch` argument selects which set
/// applies; `--stdio` and `--control stdio` pass `web_launch = false`.
///
/// * `--stdio` is the deprecated MCP-only legacy transport and has no provider / model / browser
///   surface, so it stays fully locked: `--provider != gemini`, `--model`,
///   `--api-base`, `--temperature`, and every provider-specific tuning flag are
///   rejected here.
/// * The default Web launch drives the headless web runtime, which DOES consume
///   `--provider` (all seven providers plus the Codex branch), `--model`,
///   `--api-base`, `--temperature`, and the provider-specific tuning flags via
///   `build_any_completion_model_for_args` / the headless config factory. Under
///   the Web launch those are therefore NOT blanket-rejected here as legacy-only; they
///   flow through to the cross-provider match gate in `validate_mode_args`,
///   which still rejects a provider-specific flag that does not match the
///   selected provider and still rejects `--api-base` under `--provider=codex`.
///
/// Flags that stay rejected under Web until the Plan network-policy decision
/// owns per-execution sandbox network (W2-03 gate; do not install CLI `allow`
/// as full sandbox network ahead of Allow/Deny):
/// `--network-access allow`.
/// MCP `--stdio` also rejects it (no Plan UI).
/// W3-13: `--env-file`, `--context`, `--approval-policy`, and `--approval-ttl`
/// are accepted under the Web launch (same historical semantics) and remain rejected for
/// MCP `--stdio`.
/// `--resume` is accepted only for the non-Codex Web headless path; it remains
/// rejected for MCP stdio and managed Codex, which do not share that session
/// protocol.
fn reject_non_tui_flags(args: &CodeArgs, mode: &str, web_launch: bool) -> Result<(), String> {
    // Provider / model / api-base / temperature and the provider-specific tuning
    // flags feed the headless web runtime, so they are relaxed under the Web
    // launch and rejected only under stdio. Under the Web launch they still
    // pass through the cross-provider match gate and the Codex `--api-base`
    // rejection in `validate_mode_args` (invoked after this helper), so
    // mismatched flags and `--api-base` under Codex are still rejected there.
    if !web_launch {
        reject_mode_flag(args.provider != CodeProvider::Gemini, "--provider", mode)?;
        reject_mode_flag(args.model.is_some(), "--model", mode)?;
        reject_mode_flag(args.temperature.is_some(), "--temperature", mode)?;
        reject_mode_flag(args.api_base.is_some(), "--api-base", mode)?;
        reject_mode_flag(args.ollama_thinking.is_some(), "--ollama-thinking", mode)?;
        reject_mode_flag(args.ollama_compact_tools, "--ollama-compact-tools", mode)?;
        reject_mode_flag(
            args.deepseek_thinking.is_some(),
            "--deepseek-thinking",
            mode,
        )?;
        reject_mode_flag(
            args.deepseek_reasoning_effort.is_some(),
            "--deepseek-reasoning-effort",
            mode,
        )?;
        reject_mode_flag(args.deepseek_stream.is_some(), "--deepseek-stream", mode)?;
        reject_mode_flag(args.kimi_thinking.is_some(), "--kimi-thinking", mode)?;
        reject_mode_flag(args.kimi_stream.is_some(), "--kimi-stream", mode)?;
        reject_mode_flag(args.context.is_some(), "--context", mode)?;
        reject_mode_flag(
            args.approval_policy != CodeApprovalPolicy::OnRequest,
            "--approval-policy",
            mode,
        )?;
        reject_mode_flag(args.env_file.is_some(), "--env-file", mode)?;
        reject_mode_flag(args.resume.is_some(), "--resume", mode)?;
        reject_mode_flag(args.approval_ttl.is_some(), "--approval-ttl", mode)?;
    }

    // Fail closed: installing CLI `allow` as WorkspaceWrite Full network would
    // let slash-direct tool turns use the network before Plan Allow/Deny.
    if args.network_access != CodeNetworkAccess::Deny {
        return Err(format!(
            "--network-access allow is not supported with {mode} yet: Web/MCP must not grant sandbox network ahead of the Plan network-policy gate. Omit the flag (default deny) or approve network in Plan review"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        future,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use axum::{Json, Router, extract::Request, routing::post};
    use serde_json::{Value, json};
    use tokio::{
        net::TcpListener,
        sync::{Mutex as AsyncMutex, mpsc::unbounded_channel},
    };

    use super::*;
    use crate::internal::ai::{completion::CompletionRequest, sandbox::SandboxPolicy};

    /// CEX-S2-12 "single sub-agent behind flag": the dispatcher
    /// concurrency cap is forced to 1 for every configured value —
    /// including the `sub_agents.max_parallel` schema default of 2 and
    /// larger operator settings — until CEX-S2-14 unlocks real
    /// parallelism. Pins the cap against a silent regression to passing
    /// the operator value through.
    #[test]
    fn s2_12_concurrency_cap_forces_single_sub_agent() {
        for configured in [0_u32, 1, 2, 4, 16, u32::MAX] {
            assert_eq!(
                cex_s2_12_subagent_concurrency_cap(configured),
                1,
                "CEX-S2-12 must cap concurrency to 1, not {configured}",
            );
        }
    }

    #[tokio::test]
    async fn mcp_server_shutdown_reports_a_bounded_timeout_and_aborts_the_task() {
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let handle = McpServerHandle {
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            shutdown_tx,
            join: tokio::spawn(async { future::pending::<anyhow::Result<()>>().await }),
            connection_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
        };

        let result = handle
            .shutdown_with_timeout(Duration::from_millis(10))
            .await;

        assert_eq!(result, Err(McpServerShutdownError::TimedOut));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_terminate_gate_registers_sigterm_listener() {
        let _gate = ProcessTerminateGate::install()
            .expect("interactive modes must subscribe to SIGTERM/SIGINT on Unix");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_codex_shutdown_kills_and_reaps_the_child_within_deadline() {
        let child = Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn temporary managed Codex child");
        let mut server = ManagedCodexServer {
            ws_url: "ws://127.0.0.1:0".to_string(),
            child,
        };

        server
            .shutdown_with_timeout(Duration::from_secs(1))
            .await
            .expect("managed child must be reaped before its shutdown deadline");
        assert!(server.child.id().is_none(), "managed child must be reaped");
    }

    fn base_args() -> CodeArgs {
        CodeArgs {
            port: DEFAULT_WEB_PORT,
            host: DEFAULT_BIND_HOST.to_string(),
            cwd: None,
            repo: None,
            env_file: None,
            control: ControlMode::Observe,
            browser_control: None,
            control_token_file: None,
            control_info_file: None,
            control_url: None,
            provider: CodeProvider::Gemini,
            model: None,
            temperature: None,
            ollama_thinking: None,
            ollama_compact_tools: false,
            deepseek_thinking: None,
            deepseek_reasoning_effort: None,
            deepseek_stream: None,
            kimi_thinking: None,
            kimi_stream: None,
            agent: None,
            #[cfg(feature = "test-provider")]
            fake_fixture: None,
            context: None,
            resume: None,
            approval_policy: CodeApprovalPolicy::OnRequest,
            approval_ttl: None,
            network_access: CodeNetworkAccess::Deny,
            mcp_port: DEFAULT_MCP_PORT,
            stdio: false,
            api_base: None,
            codex_bin: DEFAULT_CODEX_BIN.to_string(),
            codex_port: None,
            plan_mode: None,
            goal: None,
        }
    }

    fn canned_openai_compat_response() -> Value {
        json!({
            "id": "test-completion",
            "object": "chat.completion",
            "created": 0,
            "model": "test-model",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "ok"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            }
        })
    }

    async fn start_chat_completions_stub() -> (
        String,
        Arc<AsyncMutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let (base_url, captured, _auths, handle) = start_chat_completions_stub_with_auth().await;
        (base_url, captured, handle)
    }

    async fn start_chat_completions_stub_with_auth() -> (
        String,
        Arc<AsyncMutex<Vec<Value>>>,
        Arc<AsyncMutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let captured = Arc::new(AsyncMutex::new(Vec::new()));
        let auths = Arc::new(AsyncMutex::new(Vec::new()));
        let app = Router::new().route(
            "/chat/completions",
            post({
                let captured = captured.clone();
                let auths = auths.clone();
                move |req: Request| {
                    let captured = captured.clone();
                    let auths = auths.clone();
                    async move {
                        let auth = req
                            .headers()
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                            .await
                            .expect("read mock provider body");
                        let json: Value =
                            serde_json::from_slice(&body).expect("mock provider JSON body");
                        auths.lock().await.push(auth);
                        captured.lock().await.push(json);
                        Json(canned_openai_compat_response())
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock provider listener");
        let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock provider server runs");
        });
        (base_url, captured, auths, handle)
    }

    #[test]
    fn rejects_same_web_and_mcp_ports() {
        let mut args = base_args();
        args.mcp_port = args.port;
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_err());
    }

    /// OC-Phase 6 P6.5: `--goal` runs the same shape rules
    /// `GoalSpec::new` does so a malformed objective fails CLI
    /// parsing instead of crashing the supervisor at session start.
    #[test]
    fn accepts_well_formed_goal_objective() {
        let mut args = base_args();
        args.goal = Some("ship feature X".to_string());
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn rejects_blank_goal_objective() {
        let mut args = base_args();
        args.goal = Some("   ".to_string());
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("non-empty objective"));
    }

    #[test]
    fn rejects_oversized_goal_objective() {
        use crate::internal::ai::goal::MAX_OBJECTIVE_LEN;
        let mut args = base_args();
        args.goal = Some("z".repeat(MAX_OBJECTIVE_LEN + 1));
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("exceeds the"));
    }

    /// W1-06: the non-Codex headless runtime has a durable JSONL projection
    /// fold, so its CLI surface must make `--resume` reachable on the default
    /// Web launch. Managed Codex `--resume` is rejected outright (pinned by
    /// `bare_codex_resume_is_rejected_after_legacy_tui_removal`).
    #[test]
    fn accepts_resume_in_non_codex_web_mode() {
        let mut args = base_args();
        args.provider = CodeProvider::Ollama;
        args.resume = Some("thread-id".to_string());
        assert!(
            validate_mode_args(&args, &OutputConfig::default()).is_ok(),
            "the generic headless Web runtime must make its resume implementation reachable"
        );
    }

    /// W5-06: the legacy TUI resume driver is gone, so bare
    /// `libra code --provider codex --resume` must fail closed with a
    /// migration hint instead of silently starting a fresh Web session.
    #[test]
    fn bare_codex_resume_is_rejected_after_legacy_tui_removal() {
        let args = CodeArgs::try_parse_from([
            "libra",
            "--provider",
            "codex",
            "--resume",
            "11111111-1111-4111-8111-111111111111",
        ])
        .expect("parse bare codex resume");
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--resume") && err.contains("codex"),
            "bare Codex resume must be rejected naming both the flag and the provider; got: {err}"
        );
        assert!(
            err.contains("legacy TUI resume driver") && err.contains("removed"),
            "rejection must explain the removal and the migration path; got: {err}"
        );
    }

    #[test]
    fn rejects_env_file_and_approval_ttl_for_managed_codex_web() {
        let mut args = base_args();
        args.provider = CodeProvider::Codex;
        args.env_file = Some(PathBuf::from(".env.test"));
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--env-file") && err.contains("Web Code UI") && err.contains("codex"),
            "managed Codex must fail-closed on --env-file; got: {err}"
        );

        let mut ttl_args = base_args();
        ttl_args.provider = CodeProvider::Codex;
        ttl_args.approval_ttl = Some(42);
        let err = validate_mode_args(&ttl_args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--approval-ttl") && err.contains("Web Code UI") && err.contains("codex"),
            "managed Codex must fail-closed on --approval-ttl; got: {err}"
        );
    }

    /// C5: `--resume` is also rejected under `--stdio` (the MCP transport has
    /// no session/resume surface). Pin the actionable message shape — name the
    /// flag, the mode, and a corrective action — so the legacy-only contract has a
    /// regression guard on both non-interactive modes.
    #[test]
    fn rejects_resume_in_stdio_mode() {
        let mut args = base_args();
        args.stdio = true;
        args.resume = Some("11111111-1111-4111-8111-111111111111".to_string());
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--resume") && err.contains("--stdio") && err.contains("remove"),
            "stdio --resume rejection must name the flag, the mode, and an action; got: {err}"
        );
    }

    #[test]
    fn headless_web_resume_loads_the_requested_persisted_session() {
        let temp = tempfile::TempDir::new().expect("temporary headless session root");
        let working_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&working_dir).expect("create working directory");
        let session_store = Arc::new(SessionStore::from_storage_path(&working_dir.join(".libra")));
        let mut original = SessionState::new(&working_dir.to_string_lossy());
        original.metadata.insert(
            "thread_id".to_string(),
            serde_json::json!("headless-thread"),
        );
        let session_id = original.id.clone();
        session_store
            .save(&original)
            .expect("persist headless session");

        let mut args = base_args();
        args.provider = CodeProvider::Ollama;
        args.resume = Some("headless-thread".to_string());
        let restored =
            load_or_create_headless_web_session_state(&args, &working_dir, &session_store)
                .expect("resume persisted headless session");

        assert_eq!(restored.id, session_id);
        assert_eq!(
            restored
                .metadata
                .get("thread_id")
                .and_then(serde_json::Value::as_str),
            Some("headless-thread")
        );
    }

    #[test]
    fn headless_web_resume_preserves_indeterminate_side_effect_fence() {
        let working_dir = tempfile::tempdir().expect("create temporary workspace");
        let provider = CodeUiProviderInfo {
            provider: "test".to_string(),
            model: Some("test-model".to_string()),
            mode: Some("web-headless".to_string()),
            managed: false,
        };
        let capabilities = headless_capabilities();
        let mut persisted = initial_snapshot(
            working_dir.path().to_string_lossy().to_string(),
            provider.clone(),
            capabilities.clone(),
        );
        persisted.status = CodeUiSessionStatus::IndeterminateSideEffect;

        let mut session = SessionState::new(&working_dir.path().to_string_lossy());
        session.metadata.insert(
            HEADLESS_CODE_UI_SNAPSHOT_METADATA_KEY.to_string(),
            serde_json::to_value(persisted).expect("serialize persisted Code UI snapshot"),
        );

        let restored = build_headless_web_code_ui_snapshot(
            working_dir.path(),
            provider,
            capabilities,
            &session,
        )
        .expect("valid checkpoint must resume");

        assert_eq!(
            restored.status,
            CodeUiSessionStatus::IndeterminateSideEffect,
            "resume must retain the reconciliation fence even when projection replay is empty"
        );
    }

    #[test]
    fn code_ui_resume_rejects_malformed_durable_checkpoint() {
        let working_dir = tempfile::tempdir().expect("create temporary workspace");
        let provider = CodeUiProviderInfo {
            provider: "test".to_string(),
            model: Some("test-model".to_string()),
            mode: Some("web-headless".to_string()),
            managed: false,
        };
        let capabilities = headless_capabilities();
        let mut session = SessionState::new(&working_dir.path().to_string_lossy());
        session.metadata.insert(
            HEADLESS_CODE_UI_SNAPSHOT_METADATA_KEY.to_string(),
            serde_json::json!({"not": "a CodeUiSessionSnapshot"}),
        );
        session.metadata.insert(
            "code_ui_projection_cursor".to_string(),
            serde_json::json!(7),
        );

        let headless_error = build_headless_web_code_ui_snapshot(
            working_dir.path(),
            provider.clone(),
            capabilities.clone(),
            &session,
        )
        .expect_err("malformed checkpoint must fail closed");
        assert!(
            headless_error.contains("cannot be deserialized"),
            "headless resume must reject a corrupt durable checkpoint: {headless_error}"
        );

        let bootstrap_error = build_code_ui_resume_bootstrap_snapshot(
            working_dir.path().to_string_lossy(),
            &session,
            provider,
            capabilities,
            None,
        )
        .expect_err("malformed checkpoint must fail closed for the shared resume bootstrap");
        assert!(
            bootstrap_error.contains("cannot be deserialized"),
            "resume bootstrap must reject a corrupt durable checkpoint: {bootstrap_error}"
        );
    }

    #[tokio::test]
    async fn attach_indexed_thread_graph_skips_without_storage_or_uuid() {
        let working_dir = tempfile::tempdir().expect("create temporary workspace");
        let mut snapshot = initial_snapshot(
            working_dir.path().to_string_lossy().to_string(),
            CodeUiProviderInfo {
                provider: "test".to_string(),
                model: Some("test-model".to_string()),
                mode: Some("web-headless".to_string()),
                managed: false,
            },
            headless_capabilities(),
        );
        snapshot.thread_id = Some("not-a-uuid".to_string());
        attach_indexed_thread_graph(working_dir.path(), &mut snapshot).await;
        assert!(
            snapshot.thread_graph.is_none(),
            "non-UUID thread ids must not invent a graph"
        );

        snapshot.thread_id = Some("11111111-1111-4111-8111-111111111111".to_string());
        snapshot.thread_graph = Some(crate::internal::ai::web::code_ui::CodeUiThreadGraph {
            thread_id: "11111111-1111-4111-8111-111111111111".to_string(),
            title: Some("stale checkpoint".to_string()),
            ..Default::default()
        });
        attach_indexed_thread_graph(working_dir.path(), &mut snapshot).await;
        assert!(
            snapshot.thread_graph.is_none(),
            "failed hydration must clear a stale checkpoint graph"
        );
    }

    #[test]
    fn code_ui_resume_fold_preserves_indeterminate_side_effect_fence() {
        let working_dir = tempfile::tempdir().expect("create temporary workspace");
        let session_store = SessionStore::from_storage_path(&working_dir.path().join(".libra"));
        let provider = CodeUiProviderInfo {
            provider: "test".to_string(),
            model: Some("test-model".to_string()),
            mode: Some("web-headless".to_string()),
            managed: false,
        };
        let capabilities = headless_capabilities();
        let mut persisted = initial_snapshot(
            working_dir.path().to_string_lossy().to_string(),
            provider.clone(),
            capabilities.clone(),
        );
        persisted.status = CodeUiSessionStatus::IndeterminateSideEffect;

        let mut session = SessionState::new(&working_dir.path().to_string_lossy());
        session.metadata.insert(
            HEADLESS_CODE_UI_SNAPSHOT_METADATA_KEY.to_string(),
            serde_json::to_value(persisted).expect("serialize persisted Code UI snapshot"),
        );
        session.metadata.insert(
            "code_ui_projection_cursor".to_string(),
            serde_json::json!(42),
        );

        let bootstrap = build_code_ui_resume_bootstrap_snapshot(
            working_dir.path().to_string_lossy(),
            &session,
            provider,
            capabilities,
            None,
        )
        .expect("valid checkpoint must bootstrap");
        let folded = fold_code_ui_resume_from_session(&session_store, &session, bootstrap)
            .expect("fold resume snapshot with empty replay suffix");

        assert_eq!(
            folded.snapshot.status,
            CodeUiSessionStatus::IndeterminateSideEffect,
            "legacy/headless shared fold must retain the reconciliation fence when replay is empty"
        );
    }

    #[test]
    fn rejects_web_flags_in_stdio_mode() {
        let mut args = base_args();
        args.stdio = true;
        args.host = "0.0.0.0".to_string();
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_err());
    }

    #[test]
    fn browser_bootstrap_query_detection_for_opener_skip() {
        assert!(url_contains_browser_bootstrap_query(
            "http://127.0.0.1:4317?bt=secret-token"
        ));
        assert!(url_contains_browser_bootstrap_query(
            "http://127.0.0.1:4317?x=1&bt=secret-token"
        ));
        assert!(!url_contains_browser_bootstrap_query(
            "http://127.0.0.1:4317"
        ));
        assert!(!url_contains_browser_bootstrap_query(
            "http://127.0.0.1:4317?other=1"
        ));
        assert!(!url_contains_browser_bootstrap_query(
            "http://127.0.0.1:4317?btn=1"
        ));
    }

    /// C2 (GAP-1): web-only now accepts every supported provider — the headless
    /// web runtime + Codex web branch are reachable, not just Gemini.
    #[test]
    fn accepts_all_supported_providers_in_web_only_mode() {
        let providers = [
            CodeProvider::Gemini,
            CodeProvider::Openai,
            CodeProvider::Anthropic,
            CodeProvider::Deepseek,
            CodeProvider::Kimi,
            CodeProvider::Zhipu,
            CodeProvider::Ollama,
            CodeProvider::Codex,
        ];
        for provider in providers {
            let mut args = base_args();
            args.provider = provider;
            assert!(
                validate_mode_args(&args, &OutputConfig::default()).is_ok(),
                "web-only must accept --provider {provider:?}"
            );
        }
    }

    /// C2 (GAP-3): web-only accepts `--model`, a non-Codex `--api-base`, and
    /// `--temperature` — all consumed by the headless runtime.
    #[test]
    fn accepts_model_api_base_and_temperature_in_web_only_mode() {
        let mut args = base_args();
        args.provider = CodeProvider::Ollama;
        args.model = Some("llama3".to_string());
        args.api_base = Some("http://127.0.0.1:11434/v1".to_string());
        args.temperature = Some(0.2);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    /// C2 (GAP-3): a provider-specific flag that MATCHES the selected provider is
    /// accepted under web-only.
    #[test]
    fn accepts_matching_provider_flag_in_web_only_mode() {
        let mut args = base_args();
        args.provider = CodeProvider::Ollama;
        args.ollama_thinking = Some(OllamaThinkingArg::High);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    /// C2 (GAP-3, codex review): the matching-provider-flag acceptance must be
    /// pinned across the relaxed provider surface, not just Ollama — DeepSeek
    /// and Kimi tuning flags are accepted under web-only with their provider.
    #[test]
    fn accepts_matching_deepseek_flag_in_web_only_mode() {
        let mut args = base_args();
        args.provider = CodeProvider::Deepseek;
        args.deepseek_thinking = Some(DeepSeekThinkingArg::Enabled);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_matching_kimi_flag_in_web_only_mode() {
        let mut args = base_args();
        args.provider = CodeProvider::Kimi;
        args.kimi_thinking = Some(KimiThinkingArg::Enabled);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    /// C2 (P1, codex review): `--temperature` reaches the headless runtime after
    /// the web relaxation, so its 0.0–2.0 contract is enforced
    /// mode-independently. Out-of-range and non-finite values are rejected.
    #[test]
    fn rejects_out_of_range_temperature() {
        for bad in [2.5_f64, -0.1, f64::NAN, 3.0] {
            let mut args = base_args();
            args.provider = CodeProvider::Ollama;
            args.temperature = Some(bad);
            let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
            assert!(
                err.contains("--temperature"),
                "temperature {bad} must be rejected; got: {err}"
            );
        }
        // Boundary values are accepted.
        for good in [0.0_f64, 2.0, 1.0] {
            let mut args = base_args();
            args.provider = CodeProvider::Ollama;
            args.temperature = Some(good);
            assert!(
                validate_mode_args(&args, &OutputConfig::default()).is_ok(),
                "temperature {good} must be accepted"
            );
        }
    }

    /// C2 (R4): relaxing the web-only legacy-only blanket must NOT weaken the
    /// cross-provider match gate — a provider-specific flag that does not match
    /// the selected provider is still rejected under web-only.
    #[test]
    fn rejects_mismatched_provider_flag_in_web_only_mode() {
        let mut args = base_args();
        args.provider = CodeProvider::Deepseek;
        args.ollama_thinking = Some(OllamaThinkingArg::High);
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--ollama-thinking") && err.contains("ollama"),
            "mismatched provider flag must still be rejected under web-only; got: {err}"
        );
    }

    /// C2 (R2): the Codex `--api-base` rejection survives the web-only relaxation.
    #[test]
    fn rejects_api_base_under_codex_in_web_only_mode() {
        let mut args = base_args();
        args.provider = CodeProvider::Codex;
        args.api_base = Some("http://127.0.0.1:8080".to_string());
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--api-base") && err.contains("codex"),
            "web-only --api-base under Codex must still be rejected; got: {err}"
        );
    }

    /// W3-13: `--env-file` / `--approval-ttl` are accepted under the Web launch;
    /// `--network-access allow` stays rejected until Plan owns sandbox network.
    #[test]
    fn accepts_env_file_and_approval_ttl_in_web_only_mode_but_rejects_network_allow() {
        let mut env_file_args = base_args();
        env_file_args.env_file = Some(PathBuf::from(".env.test"));
        assert!(
            validate_mode_args(&env_file_args, &OutputConfig::default()).is_ok(),
            "web-only must accept --env-file"
        );

        let mut ttl_args = base_args();
        ttl_args.approval_ttl = Some(42);
        assert!(
            validate_mode_args(&ttl_args, &OutputConfig::default()).is_ok(),
            "web-only must accept --approval-ttl"
        );

        let mut net_args = base_args();
        net_args.network_access = CodeNetworkAccess::Allow;
        let err = validate_mode_args(&net_args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--network-access") && err.contains("Plan network-policy"),
            "web-only must reject --network-access allow with a Plan-gate explanation; got: {err}"
        );
    }

    /// C2 (R1 + codex R2, critical): `--stdio` stays fully provider-locked. One
    /// regression per class — provider, model, api-base, provider-specific flag.
    #[test]
    fn stdio_mode_stays_provider_locked() {
        // provider != gemini
        let mut args = base_args();
        args.stdio = true;
        args.provider = CodeProvider::Openai;
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--provider") && err.contains("--stdio"),
            "stdio must reject non-Gemini --provider; got: {err}"
        );

        // --model
        let mut args = base_args();
        args.stdio = true;
        args.model = Some("gpt-foo".to_string());
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--model") && err.contains("--stdio"),
            "stdio must reject --model; got: {err}"
        );

        // --api-base
        let mut args = base_args();
        args.stdio = true;
        args.api_base = Some("http://127.0.0.1:11434/v1".to_string());
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--api-base") && err.contains("--stdio"),
            "stdio must reject --api-base; got: {err}"
        );

        // provider-specific flag (blanket-rejected under stdio regardless of provider)
        let mut args = base_args();
        args.stdio = true;
        args.ollama_thinking = Some(OllamaThinkingArg::High);
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--ollama-thinking") && err.contains("--stdio"),
            "stdio must reject provider-specific flags; got: {err}"
        );
    }

    #[test]
    fn accepts_default_web_mode() {
        let args = base_args();
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_control_write_in_default_web_mode_without_alias() {
        let mut args = base_args();
        args.control = ControlMode::Write;

        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn browser_control_resolution_matrix_pins_mode_provider_and_host_contract() {
        #[derive(Copy, Clone)]
        struct BrowserControlCase {
            name: &'static str,
            provider: CodeProvider,
            explicit: Option<BrowserControlMode>,
            host: &'static str,
            expected: Result<BrowserControlMode, &'static str>,
        }

        let cases = [
            BrowserControlCase {
                name: "default web non-codex defaults to loopback on loopback host",
                provider: CodeProvider::Gemini,
                explicit: None,
                host: "127.0.0.1",
                expected: Ok(BrowserControlMode::Loopback),
            },
            BrowserControlCase {
                name: "default web non-codex default loopback rejects non-loopback host",
                provider: CodeProvider::Gemini,
                explicit: None,
                host: "0.0.0.0",
                expected: Err("loopback"),
            },
            BrowserControlCase {
                name: "default web explicit off allows non-loopback host",
                provider: CodeProvider::Gemini,
                explicit: Some(BrowserControlMode::Off),
                host: "0.0.0.0",
                expected: Ok(BrowserControlMode::Off),
            },
            BrowserControlCase {
                name: "default web explicit loopback allows loopback host",
                provider: CodeProvider::Gemini,
                explicit: Some(BrowserControlMode::Loopback),
                host: "127.0.0.1",
                expected: Ok(BrowserControlMode::Loopback),
            },
            BrowserControlCase {
                name: "default web explicit loopback rejects non-loopback host",
                provider: CodeProvider::Gemini,
                explicit: Some(BrowserControlMode::Loopback),
                host: "0.0.0.0",
                expected: Err("loopback"),
            },
            BrowserControlCase {
                name: "non-codex web defaults to loopback on loopback host",
                provider: CodeProvider::Ollama,
                explicit: None,
                host: "127.0.0.1",
                expected: Ok(BrowserControlMode::Loopback),
            },
            BrowserControlCase {
                name: "non-codex web default loopback rejects non-loopback host",
                provider: CodeProvider::Ollama,
                explicit: None,
                host: "0.0.0.0",
                expected: Err("loopback"),
            },
            BrowserControlCase {
                name: "non-codex web explicit loopback rejects non-loopback host",
                provider: CodeProvider::Ollama,
                explicit: Some(BrowserControlMode::Loopback),
                host: "0.0.0.0",
                expected: Err("loopback"),
            },
            BrowserControlCase {
                name: "codex web defaults to loopback on loopback host",
                provider: CodeProvider::Codex,
                explicit: None,
                host: "localhost",
                expected: Ok(BrowserControlMode::Loopback),
            },
            BrowserControlCase {
                name: "codex web default loopback rejects non-loopback host",
                provider: CodeProvider::Codex,
                explicit: None,
                host: "0.0.0.0",
                expected: Err("loopback"),
            },
            BrowserControlCase {
                name: "codex web explicit off allows non-loopback host",
                provider: CodeProvider::Codex,
                explicit: Some(BrowserControlMode::Off),
                host: "0.0.0.0",
                expected: Ok(BrowserControlMode::Off),
            },
            BrowserControlCase {
                name: "codex web explicit loopback allows ipv6 loopback host",
                provider: CodeProvider::Codex,
                explicit: Some(BrowserControlMode::Loopback),
                host: "::1",
                expected: Ok(BrowserControlMode::Loopback),
            },
        ];

        for case in cases {
            let mut args = base_args();
            args.provider = case.provider;
            args.browser_control = case.explicit;
            args.host = case.host.to_string();

            match (resolve_browser_control_mode(&args), case.expected) {
                (Ok(actual), Ok(expected)) => {
                    assert_eq!(actual, expected, "case: {}", case.name);
                }
                (Err(error), Err(expected_text)) => {
                    let rendered = error.to_string();
                    assert!(
                        rendered.contains(expected_text),
                        "case: {}; expected error containing {expected_text:?}, got {rendered}",
                        case.name
                    );
                }
                (actual, expected) => {
                    panic!(
                        "case: {}; browser-control resolution mismatch; actual={actual:?}, expected={expected:?}",
                        case.name
                    );
                }
            }
        }
    }

    #[test]
    fn rejects_control_write_in_stdio_mode() {
        let mut args = base_args();
        args.stdio = true;
        args.control = ControlMode::Write;

        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("deprecated MCP-only legacy")
                && err.contains("--control stdio")
                && err.contains("libra mcp --stdio"),
            "expected MCP vs --control stdio guidance; got: {err}"
        );
    }

    #[test]
    fn control_stdio_allows_discovery_without_explicit_url_or_token() {
        let mut args = base_args();
        args.control = ControlMode::Stdio;
        // W4-10: validate_mode_args must not require URL/token; discovery fills
        // them (or fails closed later with CONTROL_* codes).
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());

        args.control_info_file = Some(PathBuf::from(".libra/code/control.json"));
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());

        args.control_url = Some("http://127.0.0.1:3000".to_string());
        args.control_token_file = Some(PathBuf::from(".libra/code/control-token"));
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn rejects_mcp_stdio_combined_with_control_stdio() {
        let mut args = base_args();
        args.control = ControlMode::Stdio;
        args.stdio = true;
        args.control_url = Some("http://127.0.0.1:3000".to_string());
        args.control_token_file = Some(PathBuf::from("token"));
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("deprecated MCP-only legacy")
                && err.contains("--control stdio")
                && err.contains("libra mcp --stdio"),
            "expected MCP vs control-stdio conflict; got: {err}"
        );
    }

    #[test]
    fn rejects_plan_mode_false_with_control_stdio() {
        let mut args = base_args();
        args.control = ControlMode::Stdio;
        args.control_url = Some("http://127.0.0.1:3000".to_string());
        args.control_token_file = Some(PathBuf::from("token"));
        args.plan_mode = Some(false);
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--plan-mode") && err.contains("--control stdio"),
            "explicit --plan-mode=false must be rejected; got: {err}"
        );
    }

    #[test]
    fn rejects_provider_flags_with_control_stdio() {
        let mut args = base_args();
        args.control = ControlMode::Stdio;
        args.control_url = Some("http://127.0.0.1:3000".to_string());
        args.control_token_file = Some(PathBuf::from("token"));
        args.provider = CodeProvider::Ollama;
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--provider") && err.contains("--control stdio"),
            "expected provider conflict; got: {err}"
        );
    }

    #[test]
    fn rejects_control_write_with_non_loopback_host() {
        let mut args = base_args();
        args.control = ControlMode::Write;
        args.host = "0.0.0.0".to_string();

        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("loopback"));
    }

    #[test]
    fn accepts_env_file_cli_arg_in_default_web_mode() {
        let args = CodeArgs::try_parse_from(["libra", "--env-file", ".env.test"]).unwrap();

        assert_eq!(args.env_file.as_deref(), Some(Path::new(".env.test")));
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_env_file_in_web_mode() {
        let mut args = base_args();
        args.env_file = Some(PathBuf::from(".env.test"));

        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_approval_ttl_in_web_mode() {
        let mut args = base_args();
        args.approval_ttl = Some(42);

        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn rejects_env_file_and_approval_ttl_in_stdio_mode() {
        let mut env_args = base_args();
        env_args.stdio = true;
        env_args.env_file = Some(PathBuf::from(".env.test"));
        let err = validate_mode_args(&env_args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--env-file") && err.contains("--stdio"));

        let mut ttl_args = base_args();
        ttl_args.stdio = true;
        ttl_args.approval_ttl = Some(42);
        let err = validate_mode_args(&ttl_args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--approval-ttl") && err.contains("--stdio"));
    }

    #[test]
    fn parses_dotenv_style_env_file() {
        let env_file = parse_code_env_file(
            r#"
            # comments and blank lines are ignored
            export DEEPSEEK_API_KEY='deepseek-key'
            OPENAI_BASE_URL="https://example.test/v1"
            UNQUOTED=value # inline comment
            "#,
            Path::new(".env.test"),
        )
        .unwrap();

        assert_eq!(env_file.get("DEEPSEEK_API_KEY"), Some("deepseek-key"));
        assert_eq!(
            env_file.get("OPENAI_BASE_URL"),
            Some("https://example.test/v1")
        );
        assert_eq!(env_file.get("UNQUOTED"), Some("value"));
    }

    #[test]
    fn projection_redactor_scrubs_forbidden_env_file_values_only() {
        let env_file = parse_code_env_file(
            "OPENAI_API_KEY=sk-envfile-must-not-leak\nLIBRA_MODEL=gpt-test\n",
            Path::new(".env.test"),
        )
        .unwrap();
        let redactor = projection_secret_redactor(&env_file);
        let scrubbed =
            redactor.redact("bootstrap used sk-envfile-must-not-leak with model gpt-test");
        assert!(
            !scrubbed.contains("sk-envfile-must-not-leak"),
            "forbidden env-file values must be scrubbed: {scrubbed}"
        );
        assert!(
            scrubbed.contains("gpt-test"),
            "non-forbidden env-file values must not become secrets: {scrubbed}"
        );
    }

    #[test]
    fn control_info_schema_excludes_env_file_and_token_fields() {
        let info = ControlInfo {
            version: CONTROL_INFO_VERSION,
            mode: "web-only".to_string(),
            pid: 1,
            base_url: "http://127.0.0.1:3000".to_string(),
            mcp_url: None,
            working_dir: PathBuf::from("/tmp/repo"),
            thread_id: None,
            started_at: Utc::now(),
            repo_id: Some("repo".to_string()),
            worktree_id: None,
            workspace_id: None,
            lease_fence: None,
            pid_starttime: None,
        };
        let value = serde_json::to_value(&info).expect("control info serializes");
        let object = value.as_object().expect("object");
        for forbidden in [
            "envFile",
            "env_file",
            "env",
            "apiKey",
            "api_key",
            "token",
            "controlToken",
            "values",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "ControlInfo must not expose `{forbidden}`"
            );
        }
        let rendered = value.to_string();
        assert!(!rendered.contains("OPENAI_API_KEY"));
        assert!(!rendered.contains("sk-"));
    }

    #[test]
    fn provider_env_file_value_overrides_process_lookup() {
        let env_file =
            parse_code_env_file("DEEPSEEK_API_KEY=file-key", Path::new(".env.test")).unwrap();

        let value = provider_env_value_with_lookup(&env_file, "DEEPSEEK_API_KEY", |_| {
            Some("old-key".into())
        });

        assert_eq!(value.as_deref(), Some("file-key"));
    }

    #[test]
    fn rejects_network_access_on_default_web_launch() {
        let args = CodeArgs::try_parse_from(["libra", "--network-access", "allow"]).unwrap();
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--network-access") && err.contains("Plan network-policy"),
            "default Web must reject --network-access allow until Plan owns sandbox network; got: {err}"
        );
    }

    #[test]
    fn accepts_allow_all_approval_policy_in_default_web_mode() {
        let args = CodeArgs::try_parse_from(["libra", "--approval-policy", "allow-all"]).unwrap();

        assert_eq!(args.approval_policy, CodeApprovalPolicy::AllowAll);
        assert!(args.approval_policy.allows_all_commands());
        assert_eq!(
            AskForApproval::from(args.approval_policy),
            AskForApproval::OnRequest
        );
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_approval_ttl_cli_arg_in_default_web_mode() {
        let args = CodeArgs::try_parse_from(["libra", "--approval-ttl", "42"]).unwrap();

        assert_eq!(args.approval_ttl, Some(42));
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn loads_approval_ttl_from_project_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let libra_dir = temp_dir.path().join(".libra");
        fs::create_dir_all(&libra_dir).unwrap();
        fs::write(
            libra_dir.join("config.toml"),
            "[approval]\nttl_seconds = 123\n",
        )
        .unwrap();

        assert_eq!(
            approval_ttl_from_project_config(temp_dir.path()),
            Some(Duration::from_secs(123))
        );
    }

    #[test]
    fn loads_approval_cache_policy_from_project_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let libra_dir = temp_dir.path().join(".libra");
        fs::create_dir_all(&libra_dir).unwrap();
        fs::write(
            libra_dir.join("config.toml"),
            r#"[approval]
protected_branches = ["main", "release"]
allowed_network_domains = ["github.com"]
no_cache_unknown_network = true
"#,
        )
        .unwrap();

        assert_eq!(
            approval_cache_policy_from_project_config(temp_dir.path()),
            ApprovalCachePolicy {
                protected_branches: vec!["main".to_string(), "release".to_string()],
                allowed_network_domains: vec!["github.com".to_string()],
                no_cache_unknown_network: true,
                approved_ruleset: None,
            }
        );
    }

    #[test]
    fn plan_mode_defaults_to_none_when_omitted() {
        let args = CodeArgs::try_parse_from(["libra"]).unwrap();
        assert_eq!(args.plan_mode, None);
    }

    #[test]
    fn plan_mode_bare_flag_is_true() {
        let args = CodeArgs::try_parse_from(["libra", "--plan-mode"]).unwrap();
        assert_eq!(args.plan_mode, Some(true));
    }

    #[test]
    fn plan_mode_explicit_true_is_true() {
        let args = CodeArgs::try_parse_from(["libra", "--plan-mode=true"]).unwrap();
        assert_eq!(args.plan_mode, Some(true));
    }

    #[test]
    fn plan_mode_explicit_false_is_false() {
        let args = CodeArgs::try_parse_from(["libra", "--plan-mode=false"]).unwrap();
        assert_eq!(args.plan_mode, Some(false));
    }

    #[test]
    fn effective_plan_mode_defaults_to_true_for_codex() {
        let mut args = base_args();
        args.provider = CodeProvider::Codex;
        assert!(effective_plan_mode(&args));
    }

    #[test]
    fn effective_plan_mode_defaults_to_false_for_non_codex_providers() {
        let providers = [
            CodeProvider::Gemini,
            CodeProvider::Openai,
            CodeProvider::Anthropic,
            CodeProvider::Deepseek,
            CodeProvider::Kimi,
            CodeProvider::Zhipu,
            CodeProvider::Ollama,
        ];
        for provider in providers {
            let mut args = base_args();
            args.provider = provider;
            assert!(
                !effective_plan_mode(&args),
                "expected plan_mode=false default for provider {provider:?}"
            );
        }
    }

    #[test]
    fn effective_plan_mode_respects_explicit_user_value() {
        let mut args = base_args();
        args.provider = CodeProvider::Codex;
        args.plan_mode = Some(false);
        assert!(
            !effective_plan_mode(&args),
            "explicit --plan-mode=false must override the codex default"
        );

        args.provider = CodeProvider::Gemini;
        args.plan_mode = Some(true);
        assert!(
            effective_plan_mode(&args),
            "explicit --plan-mode=true must take effect even for non-codex providers \
             at the resolution layer (validate_mode_args is responsible for rejecting \
             that combination separately)"
        );
    }

    #[test]
    fn rejects_explicit_plan_mode_true_for_non_codex_provider() {
        let mut args = base_args();
        args.provider = CodeProvider::Gemini;
        args.plan_mode = Some(true);
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--plan-mode"));
    }

    #[test]
    fn accepts_explicit_plan_mode_false_for_non_codex_provider() {
        let mut args = base_args();
        args.provider = CodeProvider::Gemini;
        args.plan_mode = Some(false);
        validate_mode_args(&args, &OutputConfig::default()).unwrap();
    }

    #[test]
    fn rejects_network_access_cli_arg_with_invalid_value() {
        let result = CodeArgs::try_parse_from(["libra", "--network-access", "sometimes"]);

        assert!(result.is_err());
    }

    #[test]
    fn rejects_network_access_flag_in_web_mode() {
        let mut args = base_args();
        args.network_access = CodeNetworkAccess::Allow;

        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--network-access") && err.contains("Plan network-policy"));
    }

    #[test]
    fn rejects_network_access_flag_in_stdio_mode() {
        let mut args = base_args();
        args.stdio = true;
        args.network_access = CodeNetworkAccess::Allow;

        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--network-access") && err.contains("--stdio"));
    }

    #[test]
    fn accepts_anthropic_provider_in_default_web_mode() {
        let mut args = base_args();
        args.provider = CodeProvider::Anthropic;
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn rejects_ollama_thinking_for_non_ollama_provider() {
        let mut args = base_args();
        args.ollama_thinking = Some(OllamaThinkingArg::High);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_err());
    }

    #[test]
    fn accepts_ollama_thinking_for_ollama_provider() {
        let mut args = base_args();
        args.provider = CodeProvider::Ollama;
        args.ollama_thinking = Some(OllamaThinkingArg::High);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn rejects_ollama_compact_tools_for_non_ollama_provider() {
        let mut args = base_args();
        args.ollama_compact_tools = true;
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_err());
    }

    #[test]
    fn accepts_ollama_compact_tools_for_ollama_provider() {
        let mut args = base_args();
        args.provider = CodeProvider::Ollama;
        args.ollama_compact_tools = true;
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_deepseek_reasoning_flags_for_deepseek_provider() {
        let args = CodeArgs::try_parse_from([
            "libra",
            "--provider",
            "deepseek",
            "--model",
            "deepseek-v4-pro",
            "--deepseek-thinking",
            "enabled",
            "--deepseek-reasoning-effort",
            "high",
            "--deepseek-stream",
            "true",
        ])
        .unwrap();

        assert_eq!(args.provider, CodeProvider::Deepseek);
        assert_eq!(args.deepseek_thinking, Some(DeepSeekThinkingArg::Enabled));
        assert_eq!(
            args.deepseek_reasoning_effort,
            Some(DeepSeekReasoningEffortArg::High)
        );
        assert_eq!(
            completion_thinking_for_args(&args),
            Some(CompletionThinking::Enabled)
        );
        assert_eq!(
            completion_reasoning_effort_for_args(&args),
            Some(CompletionReasoningEffort::High)
        );
        assert_eq!(completion_stream_for_args(&args), Some(true));
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_deepseek_max_reasoning_alias() {
        let args = CodeArgs::try_parse_from([
            "libra",
            "--provider",
            "deepseek",
            "--deepseek-reasoning-effort",
            "xhigh",
        ])
        .unwrap();

        assert_eq!(
            args.deepseek_reasoning_effort,
            Some(DeepSeekReasoningEffortArg::Max)
        );
        assert_eq!(
            completion_reasoning_effort_for_args(&args),
            Some(CompletionReasoningEffort::Max)
        );
    }

    #[test]
    fn rejects_deepseek_reasoning_flags_for_non_deepseek_provider() {
        let mut args = base_args();
        args.deepseek_thinking = Some(DeepSeekThinkingArg::Enabled);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_err());

        let mut args = base_args();
        args.deepseek_reasoning_effort = Some(DeepSeekReasoningEffortArg::High);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_err());

        let mut args = base_args();
        args.deepseek_stream = Some(true);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_err());
    }

    #[test]
    fn accepts_kimi_thinking_for_kimi_provider() {
        let args = CodeArgs::try_parse_from([
            "libra",
            "--provider",
            "kimi",
            "--model",
            "kimi-k2.6",
            "--kimi-thinking",
            "disabled",
        ])
        .unwrap();

        assert_eq!(args.provider, CodeProvider::Kimi);
        assert_eq!(args.kimi_thinking, Some(KimiThinkingArg::Disabled));
        assert_eq!(
            completion_thinking_for_args(&args),
            Some(CompletionThinking::Disabled)
        );
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn defaults_kimi_stream_for_kimi_provider() {
        let args = CodeArgs::try_parse_from(["libra", "--provider", "kimi"]).unwrap();

        assert_eq!(args.provider, CodeProvider::Kimi);
        assert_eq!(args.kimi_stream, None);
        assert_eq!(completion_stream_for_args(&args), Some(true));
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_kimi_stream_override_for_kimi_provider() {
        let args =
            CodeArgs::try_parse_from(["libra", "--provider", "kimi", "--kimi-stream", "false"])
                .unwrap();

        assert_eq!(args.provider, CodeProvider::Kimi);
        assert_eq!(args.kimi_stream, Some(false));
        assert_eq!(completion_stream_for_args(&args), Some(false));
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn rejects_kimi_thinking_for_non_kimi_provider() {
        let mut args = base_args();
        args.kimi_thinking = Some(KimiThinkingArg::Enabled);

        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--kimi-thinking"));
    }

    #[test]
    fn rejects_kimi_stream_for_non_kimi_provider() {
        let mut args = base_args();
        args.kimi_stream = Some(true);

        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--kimi-stream"));
    }

    #[test]
    fn accepts_deepseek_stream_alias_for_deepseek_provider() {
        let args =
            CodeArgs::try_parse_from(["libra", "--provider", "deepseek", "--stream", "false"])
                .unwrap();

        assert_eq!(args.deepseek_stream, Some(false));
        assert_eq!(completion_stream_for_args(&args), Some(false));
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn tui_preserves_reasoning_content_for_reasoning_providers() {
        assert!(preserve_reasoning_content_for_provider(
            CodeProvider::Deepseek
        ));
        assert!(!preserve_reasoning_content_for_provider(
            CodeProvider::Gemini
        ));
        assert!(!preserve_reasoning_content_for_provider(
            CodeProvider::Ollama
        ));
        assert!(preserve_reasoning_content_for_provider(CodeProvider::Kimi));
    }

    #[test]
    fn codex_preflight_rejects_file_cwd() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cwd_file = temp_dir.path().join("README.md");
        std::fs::write(&cwd_file, "not a directory").unwrap();

        let mut args = base_args();
        args.provider = CodeProvider::Codex;
        args.cwd = Some(cwd_file.clone());

        let err = resolve_code_preflight_working_dir(&args).unwrap_err();
        assert!(
            err.to_string().contains("--cwd must point to a directory"),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains(&cwd_file.display().to_string()),
            "error should identify the invalid --cwd path: {err}"
        );
    }

    #[test]
    fn code_ui_runtime_uses_canonical_thread_id_metadata() {
        let mut session = SessionState::new("/tmp/workspace");
        session.id = "legacy-session".to_string();
        session.metadata.insert(
            "thread_id".to_string(),
            serde_json::json!("11111111-1111-4111-8111-111111111111"),
        );

        assert_eq!(
            session_canonical_thread_id(&session).as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
    }

    #[test]
    fn resume_bootstrap_snapshot_prefers_projection_bundle_identity() {
        let thread_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let actor = git_internal::internal::object::types::ActorRef::human("tester").unwrap();
        let bundle = ThreadBundle {
            thread: crate::internal::ai::projection::ThreadProjection {
                thread_id,
                title: Some("projection thread".to_string()),
                owner: actor.clone(),
                participants: vec![crate::internal::ai::projection::ThreadParticipant {
                    actor,
                    role: crate::internal::ai::projection::ThreadParticipantRole::Owner,
                    joined_at: Utc::now(),
                }],
                current_intent_id: None,
                latest_intent_id: None,
                intents: Vec::new(),
                metadata: None,
                archived: false,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                version: 1,
            },
            scheduler: crate::internal::ai::projection::SchedulerState {
                thread_id,
                selected_plan_id: None,
                selected_plan_ids: Vec::new(),
                current_plan_heads: Vec::new(),
                active_task_id: None,
                active_run_id: None,
                live_context_window: Vec::new(),
                metadata: None,
                updated_at: Utc::now(),
                version: 1,
            },
            freshness: crate::internal::ai::runtime::contracts::ProjectionFreshness::Fresh,
        };
        let mut session = SessionState::new("/tmp/workspace");
        session.id = "legacy-session".to_string();

        let snapshot = build_code_ui_resume_bootstrap_snapshot(
            "/tmp/workspace",
            &session,
            CodeUiProviderInfo {
                provider: "ollama".to_string(),
                model: Some("gemma4:31b".to_string()),
                mode: Some("web-headless".to_string()),
                managed: false,
            },
            headless_capabilities(),
            Some(&bundle),
        )
        .expect("build resume bootstrap snapshot from projection bundle");

        // plan-20260715 W3-01 (74d3ab2): the bootstrap always stamps durable
        // `SessionState.id` as session_id — mirroring the thread UUID into
        // sessionId breaks SPA `/usage` filters that AND both IDs. The bundle
        // identity only fills thread_id when the session has no canonical
        // thread metadata (CHANGELOG "Added (plan-20260715 W3-01, 2026-08-11)").
        assert_eq!(snapshot.session_id, "legacy-session");
        assert_eq!(snapshot.thread_id, Some(thread_id.to_string()));
    }

    #[test]
    fn code_context_maps_to_task_intent_for_prompt_and_tool_policy() {
        assert_eq!(
            task_intent_for_context(Some(CodeContext::Dev)),
            TaskIntent::Feature
        );
        assert_eq!(
            task_intent_for_context(Some(CodeContext::Review)),
            TaskIntent::Review
        );
        assert_eq!(
            task_intent_for_context(Some(CodeContext::Research)),
            TaskIntent::Question
        );
        assert_eq!(task_intent_for_context(None), TaskIntent::Unknown);
    }

    #[test]
    fn system_preamble_includes_explicit_context_intent_and_dynamic_context() {
        let temp_dir = tempfile::tempdir().unwrap();
        let prompt = system_preamble(
            temp_dir.path(),
            Some(CodeContext::Review),
            CodeProvider::Openai,
            Some("gpt-test"),
        )
        .expect("system preamble");

        assert!(prompt.contains("Code Review Mode"));
        assert!(prompt.contains("## Task Intent"));
        assert!(prompt.contains("intent=review"));
        assert!(prompt.contains("## Dynamic Workspace Context"));
        assert!(prompt.contains("source=libra status --short"));
        assert!(prompt.contains("## Context Budget Plan"));
    }

    #[test]
    fn default_runtime_context_denies_network_in_dev_mode() {
        let (tx, _rx) = unbounded_channel();
        let runtime = default_runtime_context(
            Path::new("/tmp/workspace"),
            Some(CodeContext::Dev),
            DefaultApprovalConfig {
                policy: AskForApproval::OnRequest,
                allow_all_commands: false,
                ttl: DEFAULT_APPROVAL_TTL,
                cache_policy: ApprovalCachePolicy::default(),
            },
            false,
            tx,
            "repo:test-runtime",
        );

        let sandbox = runtime.sandbox.expect("sandbox context should be present");
        assert!(matches!(
            sandbox.policy,
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                network_access,
                ..
            } if writable_roots == vec![PathBuf::from("/tmp/workspace")] && network_access.is_denied()
        ));
    }

    #[test]
    fn default_runtime_context_allows_network_when_requested_in_dev_mode() {
        let (tx, _rx) = unbounded_channel();
        let runtime = default_runtime_context(
            Path::new("/tmp/workspace"),
            Some(CodeContext::Dev),
            DefaultApprovalConfig {
                policy: AskForApproval::OnRequest,
                allow_all_commands: false,
                ttl: DEFAULT_APPROVAL_TTL,
                cache_policy: ApprovalCachePolicy::default(),
            },
            true,
            tx,
            "repo:test-runtime",
        );

        let sandbox = runtime.sandbox.expect("sandbox context should be present");
        assert!(matches!(
            sandbox.policy,
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                network_access,
                ..
            } if writable_roots == vec![PathBuf::from("/tmp/workspace")] && network_access.is_full()
        ));
    }

    #[tokio::test]
    async fn default_runtime_context_can_allow_all_commands() {
        let (tx, _rx) = unbounded_channel();
        let runtime = default_runtime_context(
            Path::new("/tmp/workspace"),
            Some(CodeContext::Dev),
            DefaultApprovalConfig {
                policy: AskForApproval::OnRequest,
                allow_all_commands: true,
                ttl: DEFAULT_APPROVAL_TTL,
                cache_policy: ApprovalCachePolicy::default(),
            },
            true,
            tx,
            "repo:test-runtime",
        );

        let approval = runtime
            .approval
            .expect("approval context should be present");
        assert_eq!(
            approval.scope_key_prefix.as_deref(),
            Some("repo:test-runtime")
        );
        assert!(
            approval
                .store
                .lock()
                .await
                .allow_all_commands_for_scope("repo:test-runtime")
        );
    }

    #[test]
    fn default_runtime_context_is_read_only_for_review_and_research() {
        for context in [CodeContext::Review, CodeContext::Research] {
            let (tx, _rx) = unbounded_channel();
            let runtime = default_runtime_context(
                Path::new("/tmp/workspace"),
                Some(context),
                DefaultApprovalConfig {
                    policy: AskForApproval::OnRequest,
                    allow_all_commands: false,
                    ttl: DEFAULT_APPROVAL_TTL,
                    cache_policy: ApprovalCachePolicy::default(),
                },
                true,
                tx,
                "repo:test-runtime",
            );

            let sandbox = runtime.sandbox.expect("sandbox context should be present");
            assert!(matches!(sandbox.policy, SandboxPolicy::ReadOnly));
        }
    }

    /// C7 (plan.md:1376): the three runtime-shaping flags must be visible at
    /// tool invocation through the `ToolRuntimeContext` the tool loop reads.
    /// The `--network-access` and allow-all axes are pinned by the tests
    /// above; this pins that a non-default `--approval-policy` and
    /// `--approval-ttl` both land on the `ToolApprovalContext` (`policy` +
    /// `approval_ttl`) rather than being silently dropped between the CLI
    /// mapping and the runtime context. `shell`/`apply_patch` read exactly
    /// these fields to gate execution, so observing them here is the
    /// "visible at invocation" contract.
    #[test]
    fn default_runtime_context_exposes_approval_policy_and_ttl() {
        // Exercise the PRODUCTION mapping (codex C7 review): the args ->
        // DefaultApprovalConfig mapping is now the shared helper
        // `approval_config_from_args`, which the Web launch path calls. Feeding
        // it parsed CLI args and running the result through
        // `default_runtime_context` catches a regression where a
        // flag is dropped or hardcoded on the real production path — not just
        // inside the runtime-context builder.
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let args = CodeArgs::try_parse_from([
            "libra",
            "--approval-policy",
            "untrusted",
            "--approval-ttl",
            "4242",
        ])
        .expect("parse code args");

        let (tx, _rx) = unbounded_channel();
        let runtime = default_runtime_context(
            workspace.path(),
            Some(CodeContext::Dev),
            approval_config_from_args(&args, workspace.path()).expect("approval config"),
            args.network_access.is_allowed(),
            tx,
            "repo:test-runtime",
        );

        let approval = runtime
            .approval
            .expect("approval context should be present");
        // `--approval-policy untrusted` must map through the helper's `.into()`
        // to AskForApproval::UnlessTrusted.
        assert_eq!(approval.policy, AskForApproval::UnlessTrusted);
        // `--approval-ttl 4242` must map through the helper's Duration::from_secs.
        assert_eq!(approval.approval_ttl, Duration::from_secs(4242));

        // Control: with no --approval-ttl and no project config, the helper
        // falls back to the 300s default — proving the 4242s above came from
        // the flag, not a hardcode.
        let default_args = CodeArgs::try_parse_from(["libra"]).expect("parse defaults");
        let default_cfg =
            approval_config_from_args(&default_args, workspace.path()).expect("default approval");
        assert_eq!(default_cfg.ttl, DEFAULT_APPROVAL_TTL);
        assert_ne!(default_cfg.ttl, Duration::from_secs(4242));
    }

    // ─── OC-Phase 2 P2.4: --agent override ────────────────────────────────

    /// Build a working directory with a `.libra/agents/` profile that pins a
    /// structured `provider/model` binding so the override path has
    /// something to lift.
    fn write_agent_profile(working_dir: &Path, name: &str, body: &str) {
        let agents_dir = working_dir.join(".libra").join("agents");
        std::fs::create_dir_all(&agents_dir).expect("create agents dir");
        std::fs::write(agents_dir.join(format!("{name}.md")), body).expect("write profile");
    }

    /// Scenario: `--agent` is unset → helper is a no-op and returns `None`.
    /// This is the flag-off baseline OC-Phase 2 P2.4 must preserve.
    #[test]
    fn resolve_agent_override_noop_when_flag_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let args = base_args();
        let result = resolve_agent_binding_override(&args, tmp.path()).unwrap();
        assert!(result.is_none());
    }

    /// Scenario: `--agent <name>` lifts a profile that carries
    /// `model: anthropic/claude-3-5-sonnet-latest` into a structured
    /// `ModelBinding`. The legacy `model_preference` form is irrelevant
    /// here; only the binding goes through.
    #[test]
    fn resolve_agent_override_lifts_provider_slash_model_binding() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_agent_profile(
            tmp.path(),
            "planner",
            "---\n\
             name: planner\n\
             description: Implementation planner\n\
             tools: []\n\
             model: anthropic/claude-3-5-sonnet-latest\n\
             ---\n\
             You plan.",
        );
        let mut args = base_args();
        args.agent = Some("planner".to_string());

        let binding = resolve_agent_binding_override(&args, tmp.path())
            .unwrap()
            .expect("binding lifts");
        assert_eq!(binding.provider_id, "anthropic");
        assert_eq!(binding.model_id, "claude-3-5-sonnet-latest");
        assert!(binding.variant.is_none());
    }

    /// Scenario: an `--agent` profile that carries only a legacy alias
    /// (`model: default`) yields `Ok(None)` — there is no structured
    /// binding to override the CLI defaults with, so the rest of
    /// `build_any_completion_model_for_args` falls through to the CLI
    /// provider/model defaults.
    #[test]
    fn resolve_agent_override_returns_none_for_legacy_model_alias() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_agent_profile(
            tmp.path(),
            "planner",
            "---\nname: planner\nmodel: default\n---\nbody",
        );
        let mut args = base_args();
        args.agent = Some("planner".to_string());

        let result = resolve_agent_binding_override(&args, tmp.path()).unwrap();
        assert!(result.is_none());
    }

    /// Scenario: an unknown agent name surfaces a `command_usage` error
    /// listing the known profiles. Embedded defaults always load, so the
    /// suggestion list is never empty.
    #[test]
    fn resolve_agent_override_unknown_name_lists_known_profiles() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut args = base_args();
        args.agent = Some("does-not-exist".to_string());

        let err = resolve_agent_binding_override(&args, tmp.path())
            .expect_err("unknown agent must error");
        let msg = err.to_string();
        assert!(
            msg.contains("does-not-exist"),
            "error must mention the bad name: {msg}"
        );
        // Embedded `planner` is one of the catalogued profiles, so the
        // suggestion list must include it.
        assert!(
            msg.contains("planner"),
            "error must list known profiles: {msg}"
        );
    }

    /// Scenario: a profile whose `mode: subagent` is selected by `--agent`
    /// is rejected. Sub-agents are dispatched via the `task` tool in
    /// OC-Phase 3, not as the session driver.
    #[test]
    fn resolve_agent_override_rejects_non_primary_eligible_mode() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_agent_profile(
            tmp.path(),
            "explorer",
            "---\n\
             name: explorer\n\
             mode: subagent\n\
             model: anthropic/claude-3-5-haiku-latest\n\
             ---\n\
             body",
        );
        let mut args = base_args();
        args.agent = Some("explorer".to_string());

        let err = resolve_agent_binding_override(&args, tmp.path())
            .expect_err("subagent-only profile must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("explorer"),
            "error must mention agent name: {msg}"
        );
        assert!(
            msg.contains("Subagent") || msg.contains("subagent"),
            "error must mention the offending mode: {msg}"
        );
    }

    /// Scenario: a `mode: all` profile IS primary-eligible, so the override
    /// surfaces the binding rather than erroring. This pins the doc rule
    /// "Primary | All" → primary-eligible.
    #[test]
    fn resolve_agent_override_accepts_mode_all() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_agent_profile(
            tmp.path(),
            "swiss",
            "---\n\
             name: swiss\n\
             mode: all\n\
             model: openai/gpt-4o-mini\n\
             ---\n\
             body",
        );
        let mut args = base_args();
        args.agent = Some("swiss".to_string());

        let binding = resolve_agent_binding_override(&args, tmp.path())
            .unwrap()
            .expect("binding lifts");
        assert_eq!(binding.provider_id, "openai");
        assert_eq!(binding.model_id, "gpt-4o-mini");
    }

    /// Scenario (OC-Phase 3 P3.1 flag-off invariant — production path):
    /// the headless tool registry built by [`build_headless_tool_registry`]
    /// MUST NOT register a `task` tool. P3.1 only ships the schema
    /// constructor; runtime wiring lives in P3.2+ behind
    /// `code.multi_agent.enabled` (OC-Phase 5). A regression that wires
    /// the dispatcher unconditionally would fail this test by surfacing
    /// `task` in the registry's `tool_names()`.
    ///
    /// The unit-level guard at
    /// `internal::ai::tools::registry::tests::registry_does_not_expose_task_tool_in_flag_off_default`
    /// covers the fixture-level invariant for registry construction.
    #[test]
    fn build_headless_tool_registry_omits_task_tool_in_flag_off_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry =
            build_headless_tool_registry(tmp.path(), tx, SecretRedactor::default_runtime());
        let names = registry.tool_names();
        assert!(
            !names.contains(&"task".to_string()),
            "OC-Phase 3 P3.1 invariant: `task` must not be registered in the \
             headless registry until the dispatcher lands and is gated; \
             got tool_names = {names:?}"
        );
    }

    /// Scenario: headless web mode now has a browser approval channel, a
    /// ToolRuntimeContext, and snapshot projection for direct plan updates, so
    /// the registry may expose the same guarded network/mutating/basic plan
    /// tools as the legacy full-workflow path without bypassing sandbox, approval, or `--network-access
    /// deny`.
    #[test]
    fn build_headless_tool_registry_exposes_runtime_guarded_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry =
            build_headless_tool_registry(tmp.path(), tx, SecretRedactor::default_runtime());
        let names = registry.tool_names();

        for tool in [
            "web_search",
            "apply_patch",
            "shell",
            "update_plan",
            "submit_plan_draft",
        ] {
            assert!(
                names.iter().any(|name| name == tool),
                "headless registry must expose guarded tool `{tool}` after runtime context wiring; got {names:?}"
            );
        }
    }

    #[test]
    fn build_helper_missing_api_key_errors_name_canonical_env_vars() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cases: &[(CodeProvider, Option<&str>, Option<&str>, &str)] = &[
            (CodeProvider::Gemini, None, None, "GEMINI_API_KEY"),
            (CodeProvider::Openai, None, None, "OPENAI_API_KEY"),
            (CodeProvider::Anthropic, None, None, "ANTHROPIC_API_KEY"),
            (CodeProvider::Deepseek, None, None, "DEEPSEEK_API_KEY"),
            (CodeProvider::Kimi, None, None, "MOONSHOT_API_KEY"),
            (CodeProvider::Zhipu, None, None, "ZHIPU_API_KEY"),
            (
                CodeProvider::Ollama,
                Some("llama3.2"),
                Some("https://ollama.com"),
                "OLLAMA_API_KEY",
            ),
        ];

        for (provider, model, api_base, expected_env) in cases {
            let mut args = base_args();
            args.provider = *provider;
            args.model = model.map(str::to_string);
            args.api_base = api_base.map(str::to_string);
            let err = build_any_completion_model_for_args_with_lookup(
                &args,
                &CodeEnvFile::default(),
                tmp.path(),
                |_| None,
            )
            .expect_err("missing api key path must fire");
            let msg = err.to_string();
            assert!(
                msg.contains(expected_env),
                "expected {expected_env} in missing-key error for {provider:?}, got: {msg}"
            );
            assert!(
                msg.contains("is not set") || msg.contains("is required"),
                "missing-key error should be readable and actionable for {provider:?}, got: {msg}"
            );
            // C3 criterion: the error must also explain HOW to configure the
            // key, not just name it. Non-Ollama providers point at the
            // vault/export path; Ollama's cloud message points at
            // `--api-base` / `OLLAMA_BASE_URL`.
            assert!(
                msg.contains("vault.env")
                    || msg.contains("OLLAMA_BASE_URL")
                    || msg.contains("--api-base"),
                "missing-key error must explain how to configure {provider:?}, got: {msg}"
            );
        }
    }

    #[tokio::test]
    async fn build_helper_honors_cli_api_base_for_deepseek() {
        let (base_url, captured, server) = start_chat_completions_stub().await;
        let tmp = tempfile::TempDir::new().unwrap();
        let mut args = base_args();
        args.provider = CodeProvider::Deepseek;
        args.model = Some("deepseek-chat".to_string());
        args.api_base = Some(base_url);
        let mut env_file = CodeEnvFile::default();
        env_file
            .values
            .insert("DEEPSEEK_API_KEY".to_string(), "test-key".to_string());

        let (model, model_name, provider_id) =
            build_any_completion_model_for_args(&args, &env_file, tmp.path())
                .expect("DeepSeek model builds with API key and custom base URL");
        assert_eq!(provider_id, "deepseek");
        assert_eq!(model_name, "deepseek-chat");

        let request = CompletionRequest::new(vec![crate::internal::ai::completion::Message::user(
            "hello",
        )]);
        let _response = model
            .completion(request)
            .await
            .expect("custom --api-base endpoint should receive the request");

        let bodies = captured.lock().await;
        assert_eq!(bodies.len(), 1, "expected exactly one provider POST");
        assert_eq!(
            bodies[0].get("model").and_then(|value| value.as_str()),
            Some("deepseek-chat"),
            "DeepSeek request should reach the CLI-provided --api-base endpoint"
        );
        server.abort();
    }

    #[tokio::test]
    async fn headless_ollama_reuses_provider_factory_bootstrap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut args = base_args();
        args.provider = CodeProvider::Ollama;
        args.model = Some("llama3.2".to_string());
        let session_store = Arc::new(SessionStore::from_storage_path(&tmp.path().join(".libra")));
        let session_state = SessionState::new(&tmp.path().to_string_lossy());

        let runtime = build_non_codex_headless_runtime(
            &args,
            tmp.path(),
            &CodeEnvFile::default(),
            session_store,
            session_state,
            false,
            init_mcp_server(tmp.path()).await,
        )
        .await
        .expect("headless Ollama should build through ProviderFactory")
        .expect("Ollama is the supported non-Codex headless provider");
        let snapshot = runtime.snapshot().await;

        assert_eq!(snapshot.provider.provider, "ollama");
        assert_eq!(snapshot.provider.mode.as_deref(), Some("web-headless"));
        assert_eq!(snapshot.provider.model.as_deref(), Some("llama3.2"));
    }

    #[tokio::test]
    async fn headless_provider_boot_uses_the_shared_env_file_lookup() {
        let tmp = tempfile::TempDir::new().expect("temporary workspace");
        let mut args = base_args();
        args.provider = CodeProvider::Openai;
        args.model = Some("gpt-test".to_string());
        let mut env_file = CodeEnvFile::default();
        env_file
            .values
            .insert("OPENAI_API_KEY".to_string(), "from-env-file".to_string());
        let session_store = Arc::new(SessionStore::from_storage_path(&tmp.path().join(".libra")));
        let session_state = SessionState::new(&tmp.path().to_string_lossy());

        let runtime = build_non_codex_headless_runtime(
            &args,
            tmp.path(),
            &env_file,
            session_store,
            session_state,
            false,
            init_mcp_server(tmp.path()).await,
        )
        .await
        .expect("headless OpenAI should use the shared provider factory")
        .expect("OpenAI is a supported non-Codex headless provider");
        let snapshot = runtime.snapshot().await;

        assert_eq!(snapshot.provider.provider, "openai");
        assert_eq!(snapshot.provider.model.as_deref(), Some("gpt-test"));
    }

    #[cfg(feature = "test-provider")]
    #[tokio::test]
    async fn headless_non_ollama_provider_reuses_provider_factory_bootstrap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut args = base_args();
        args.provider = CodeProvider::Fake;
        let fixture_path = tmp.path().join("fake-fixture.json");
        args.fake_fixture = Some({
            std::fs::write(
                &fixture_path,
                r#"{"responses":[],"fallback":{"type":"text","text":"ok"}}"#,
            )
            .expect("fixture payload should be written");
            fixture_path
        });
        let session_store = Arc::new(SessionStore::from_storage_path(&tmp.path().join(".libra")));
        let session_state = SessionState::new(&tmp.path().to_string_lossy());

        let runtime = build_non_codex_headless_runtime(
            &args,
            tmp.path(),
            &CodeEnvFile::default(),
            session_store,
            session_state,
            false,
            init_mcp_server(tmp.path()).await,
        )
        .await
        .expect("headless Fake should build through ProviderFactory")
        .expect("Fake provider is now supported in headless provider factory path");
        let snapshot = runtime.snapshot().await;

        assert_eq!(snapshot.provider.provider, "fake");
        assert_eq!(snapshot.provider.mode.as_deref(), Some("web-headless"));
        assert_eq!(snapshot.provider.model.as_deref(), Some("fake-local"));
    }

    /// C4 reachability regression (first dispatch layer): the web-only
    /// provider branch in `execute_web_only` decides purely through
    /// `web_only_runtime_kind`. Pin every accepted provider to its intended
    /// runtime so the Task C2 validation relaxation — which now lets the
    /// non-Gemini providers reach this dispatch — cannot silently misroute a
    /// provider or strand one on the read-only placeholder.
    #[test]
    fn web_only_runtime_kind_routes_each_provider_to_its_runtime() {
        // Codex is the only provider that drives the managed app-server child.
        assert_eq!(
            web_only_runtime_kind(CodeProvider::Codex),
            WebOnlyRuntimeKind::ManagedCodexAppServer,
        );
        // Every other accepted provider reaches the headless runtime via
        // `build_non_codex_headless_runtime`.
        for provider in [
            CodeProvider::Gemini,
            CodeProvider::Openai,
            CodeProvider::Anthropic,
            CodeProvider::Deepseek,
            CodeProvider::Kimi,
            CodeProvider::Zhipu,
            CodeProvider::Ollama,
        ] {
            assert_eq!(
                web_only_runtime_kind(provider),
                WebOnlyRuntimeKind::Headless,
                "provider {provider:?} must reach the headless web runtime",
            );
        }
        #[cfg(feature = "test-provider")]
        assert_eq!(
            web_only_runtime_kind(CodeProvider::Fake),
            WebOnlyRuntimeKind::Headless,
        );
    }

    /// C4 reachability regression (second dispatch layer): Codex must never
    /// enter `build_non_codex_headless_runtime`. `execute_web_only` already
    /// routes it to the managed app-server path via `web_only_runtime_kind`,
    /// but the dispatcher itself also fails closed with `Ok(None)` so a future
    /// refactor cannot silently build a headless completion model for Codex.
    #[tokio::test]
    async fn build_non_codex_headless_runtime_excludes_codex_provider() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut args = base_args();
        args.provider = CodeProvider::Codex;
        let session_store = Arc::new(SessionStore::from_storage_path(&tmp.path().join(".libra")));
        let session_state = SessionState::new(&tmp.path().to_string_lossy());

        let runtime = build_non_codex_headless_runtime(
            &args,
            tmp.path(),
            &CodeEnvFile::default(),
            session_store,
            session_state,
            false,
            init_mcp_server(tmp.path()).await,
        )
        .await
        .expect("Codex arm must return Ok(None), not an error");
        assert!(
            runtime.is_none(),
            "Codex must be excluded from the non-Codex headless dispatcher",
        );
    }

    /// Scenario: `--provider gemini --model gpt-foo --agent planner`
    /// (where `planner` carries `model: anthropic/claude-3-5-sonnet-latest`)
    /// — the agent's binding wins **atomically**. The CLI `--model gpt-foo`
    /// is dropped because it would otherwise pair an OpenAI-style model id
    /// with the agent's anthropic provider. Smoke tests the integration of
    /// `resolve_agent_binding_override` with the rest of
    /// `build_any_completion_model_for_args`.
    #[cfg(feature = "test-provider")]
    #[test]
    fn build_helper_treats_agent_binding_atomically() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_agent_profile(
            tmp.path(),
            "planner",
            "---\n\
             name: planner\n\
             model: anthropic/claude-3-5-sonnet-latest\n\
             ---\n\
             body",
        );
        let mut args = base_args();
        args.provider = CodeProvider::Gemini;
        args.model = Some("gemini-2.0-flash".to_string()); // would-be hybrid
        args.agent = Some("planner".to_string());
        let env_file = CodeEnvFile::default();

        // The build call would fail (no API key in CodeEnvFile), but the
        // failure path tells us which provider we ended up dispatching to:
        // an Anthropic build complains about ANTHROPIC_API_KEY, NOT
        // GEMINI_API_KEY.
        let err = build_any_completion_model_for_args(&args, &env_file, tmp.path())
            .expect_err("missing api key path must fire");
        let msg = err.to_string();
        assert!(
            msg.contains("ANTHROPIC_API_KEY"),
            "agent override must point env-var lookup at anthropic, got: {msg}"
        );
        assert!(
            !msg.contains("GEMINI_API_KEY"),
            "CLI --provider gemini must NOT win after agent override, got: {msg}"
        );
    }

    /// C3 criterion 1 (default model id): with `--model` omitted the build
    /// helper must fall back to each provider's documented flagship default,
    /// and Ollama must instead demand an explicit `--model`. A lookup that
    /// only answers `*_API_KEY` keeps every base URL at its provider default
    /// so the client constructs without touching a bogus endpoint.
    #[test]
    fn build_helper_defaults_model_id_per_provider() {
        let tmp = tempfile::TempDir::new().unwrap();
        let api_key_only = |key: &str| -> Option<String> {
            key.ends_with("_API_KEY").then(|| "dummy-key".to_string())
        };
        let cases: &[(CodeProvider, &str, &str)] = &[
            (CodeProvider::Gemini, GEMINI_2_5_FLASH, "gemini"),
            (CodeProvider::Openai, GPT_4O_MINI, "openai"),
            (CodeProvider::Anthropic, CLAUDE_3_5_SONNET, "anthropic"),
            (CodeProvider::Deepseek, "deepseek-chat", "deepseek"),
            (CodeProvider::Kimi, KIMI_K2_6, "kimi"),
            (CodeProvider::Zhipu, GLM_5, "zhipu"),
        ];
        for (provider, expected_model, expected_provider_id) in cases {
            let mut args = base_args();
            args.provider = *provider;
            args.model = None;
            let (_model, model_name, provider_id) =
                build_any_completion_model_for_args_with_lookup(
                    &args,
                    &CodeEnvFile::default(),
                    tmp.path(),
                    api_key_only,
                )
                .unwrap_or_else(|err| panic!("default-model build for {provider:?} failed: {err}"));
            assert_eq!(
                model_name, *expected_model,
                "wrong default model for {provider:?}"
            );
            assert_eq!(
                provider_id, *expected_provider_id,
                "wrong provider id for {provider:?}"
            );
        }

        // Ollama has no sensible local default — omitting `--model` must be a
        // usage error, not a silent fallback.
        let mut ollama = base_args();
        ollama.provider = CodeProvider::Ollama;
        ollama.model = None;
        let err = build_any_completion_model_for_args_with_lookup(
            &ollama,
            &CodeEnvFile::default(),
            tmp.path(),
            api_key_only,
        )
        .expect_err("ollama without --model must error");
        assert!(
            err.to_string().contains("--model is required"),
            "ollama default-model error must be actionable: {err}"
        );
    }

    /// C3 criterion 1 (api-base rules): for the OpenAI-compat family a
    /// `*_BASE_URL` value supplied through `--env-file` is honored when the
    /// CLI `--api-base` flag is absent (the `.or_else(resolve_env(...))`
    /// fallback arm). Complements `build_helper_honors_cli_api_base_for_deepseek`,
    /// which pins the CLI-flag arm.
    #[tokio::test]
    async fn build_helper_honors_env_file_base_url_for_openai() {
        let (base_url, captured, server) = start_chat_completions_stub().await;
        let tmp = tempfile::TempDir::new().unwrap();
        let mut args = base_args();
        args.provider = CodeProvider::Openai;
        args.model = Some("gpt-4o-mini".to_string());
        // No CLI --api-base; the base URL must come from the env-file.
        args.api_base = None;
        let mut env_file = CodeEnvFile::default();
        env_file
            .values
            .insert("OPENAI_API_KEY".to_string(), "test-key".to_string());
        env_file
            .values
            .insert("OPENAI_BASE_URL".to_string(), base_url);

        let (model, _model_name, provider_id) =
            build_any_completion_model_for_args_with_lookup(&args, &env_file, tmp.path(), |_| None)
                .expect("OpenAI model builds with env-file base URL");
        assert_eq!(provider_id, "openai");

        let request = CompletionRequest::new(vec![crate::internal::ai::completion::Message::user(
            "hello",
        )]);
        let _response = model
            .completion(request)
            .await
            .expect("env-file OPENAI_BASE_URL endpoint should receive the request");

        let bodies = captured.lock().await;
        assert_eq!(
            bodies.len(),
            1,
            "OpenAI request should reach the env-file OPENAI_BASE_URL endpoint"
        );
        server.abort();
    }

    /// W4-01: default `libra code` must load `--env-file` through the same
    /// `execute_web_only` bootstrap chain and send the env-file credential on
    /// the wire — not a competing process-env value.
    #[tokio::test]
    async fn default_web_execute_path_sends_env_file_credential_on_wire() {
        let _process = crate::utils::test::ScopedEnvVar::set("OPENAI_API_KEY", "from-process-env");

        let (base_url, captured, auths, server) = start_chat_completions_stub_with_auth().await;
        let tmp = tempfile::TempDir::new().expect("temporary workspace");
        let env_path = tmp.path().join(".env.w4-01");
        std::fs::write(
            &env_path,
            format!("OPENAI_API_KEY=from-env-file\nOPENAI_BASE_URL={base_url}\n"),
        )
        .expect("write env file");

        let args = CodeArgs::try_parse_from([
            "libra",
            "--env-file",
            env_path.to_str().expect("utf8 path"),
            "--provider",
            "openai",
            "--model",
            "gpt-test",
        ])
        .expect("default Web must accept --env-file");
        assert!(
            code_uses_web_launch(&args),
            "bare libra code must select the Web Code UI launch path"
        );
        assert!(
            validate_mode_args(&args, &OutputConfig::default()).is_ok(),
            "default Web validation must accept --env-file"
        );

        // Same first steps as `execute_web_only` before servers bind.
        let env_file = load_code_env_file(args.env_file.as_deref())
            .expect("load --env-file for Web bootstrap");
        let (model, model_name, provider_id) =
            build_any_completion_model_for_args(&args, &env_file, tmp.path())
                .expect("default Web provider boot must use env-file → process lookup");
        assert_eq!(provider_id, "openai");
        assert_eq!(model_name, "gpt-test");

        let request = CompletionRequest::new(vec![crate::internal::ai::completion::Message::user(
            "hello",
        )]);
        let _response = model
            .completion(request)
            .await
            .expect("env-file OPENAI_BASE_URL must receive the completion");

        let bodies = captured.lock().await;
        assert_eq!(bodies.len(), 1, "expected one provider POST");
        let auth_headers = auths.lock().await;
        assert_eq!(
            auth_headers.as_slice(),
            ["Bearer from-env-file"],
            "wire Authorization must use the env-file key, not the process-env competitor"
        );
        server.abort();
    }

    /// C3 criterion 1 (api-base rules across ALL providers, codex review):
    /// pins the per-provider api-base source — CLI `--api-base` always wins,
    /// and only openai/anthropic/kimi/zhipu/ollama fall back to their
    /// `*_BASE_URL` env var; deepseek/gemini are CLI-only; codex/unknown
    /// resolve to None. Guards each arm against silent regression.
    #[test]
    fn resolve_provider_api_base_matches_per_provider_rules() {
        use crate::internal::ai::providers::runtime::provider_id;
        let env = |var: &str, val: &str| {
            let var = var.to_string();
            let val = val.to_string();
            move |k: &str| if k == var { Some(val.clone()) } else { None }
        };

        // (provider_id, env_var_name_or_empty_if_cli_only)
        let env_fallback = [
            (provider_id::OPENAI, "OPENAI_BASE_URL"),
            (provider_id::ANTHROPIC, "ANTHROPIC_BASE_URL"),
            (provider_id::KIMI, "MOONSHOT_BASE_URL"),
            (provider_id::ZHIPU, "ZHIPU_BASE_URL"),
            (provider_id::OLLAMA, "OLLAMA_BASE_URL"),
        ];
        for (pid, var) in env_fallback {
            // CLI flag wins over the env fallback.
            assert_eq!(
                resolve_provider_api_base(
                    pid,
                    Some("https://cli.example".to_string()),
                    env(var, "https://env.example")
                ),
                Some("https://cli.example".to_string()),
                "{pid}: CLI --api-base must win over {var}"
            );
            // Env fallback used when the CLI flag is absent.
            assert_eq!(
                resolve_provider_api_base(pid, None, env(var, "https://env.example")),
                Some("https://env.example".to_string()),
                "{pid}: must fall back to {var}"
            );
            // The env var name is provider-specific: another provider's
            // *_BASE_URL must NOT leak through.
            assert_eq!(
                resolve_provider_api_base(pid, None, env("SOME_OTHER_BASE_URL", "https://x")),
                None,
                "{pid}: must only read {var}"
            );
        }

        // deepseek/gemini: CLI-only, no env fallback.
        for pid in [provider_id::DEEPSEEK, provider_id::GEMINI] {
            assert_eq!(
                resolve_provider_api_base(
                    pid,
                    Some("https://cli.example".to_string()),
                    env("DEEPSEEK_BASE_URL", "https://env.example")
                ),
                Some("https://cli.example".to_string()),
                "{pid}: CLI --api-base honored"
            );
            assert_eq!(
                resolve_provider_api_base(
                    pid,
                    None,
                    env("DEEPSEEK_BASE_URL", "https://env.example")
                ),
                None,
                "{pid}: CLI-only, no env fallback"
            );
        }

        // codex never reaches the factory; an unknown id resolves to None
        // even with a CLI flag (the `_ => None` arm), so a future misroute
        // cannot smuggle a base URL into the managed Codex runtime.
        assert_eq!(
            resolve_provider_api_base("codex", None, env("ANYTHING", "https://x")),
            None
        );
        assert_eq!(
            resolve_provider_api_base("codex", Some("https://cli.example".to_string()), |_| None),
            None,
            "codex/unknown resolves to None regardless of the CLI flag"
        );
    }

    /// C3 criterion 4 (Codex preflight): a WebSocket startup that never
    /// becomes reachable must surface a human-readable, url-bearing timeout
    /// diagnostic rather than a bare error or a hang. Uses a freed local port
    /// (nothing listening) and a short injected timeout.
    #[tokio::test]
    async fn codex_ready_probe_times_out_with_human_readable_diagnostic() {
        let ws_url = {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener); // release the port so the probe connection is refused
            format!("ws://127.0.0.1:{port}")
        };
        let err = wait_for_codex_ready_within(&ws_url, Duration::from_millis(50))
            .await
            .expect_err("connecting to a dead port must time out");
        let msg = err.to_string();
        assert!(
            msg.contains("timed out waiting for Codex app-server"),
            "startup-timeout diagnostic must be human-readable: {msg}"
        );
        assert!(
            msg.contains(&ws_url),
            "startup-timeout diagnostic must name the WebSocket url: {msg}"
        );
    }
}
