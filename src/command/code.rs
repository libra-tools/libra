//! # Code Command — Interactive AI-Powered Coding Sessions
//!
//! This module implements the `libra code` subcommand, which is the primary entry point
//! for AI-agent-driven and human-collaborative development within a Libra repository.
//!
//! ## Architecture Overview
//!
//! The command orchestrates several concurrent subsystems:
//!
//! - **TUI (Terminal UI)**: A `ratatui`/`crossterm`-based interactive terminal interface
//!   that renders the chat conversation, tool outputs, and approval prompts.
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
//! | **TUI** (default) | *(none)* | Full interactive terminal UI with background web + MCP servers |
//! | **Web-only** | `--web` | Headless web server + MCP server; no terminal UI |
//! | **Stdio** | `--stdio` | MCP server over stdin/stdout for AI client integration |
//!
//! ## Provider Dispatch
//!
//! The `--provider` flag selects the AI backend. Each provider follows the same pattern:
//! 1. Create a client from environment variables (API keys).
//! 2. Instantiate a completion model with the selected (or default) model name.
//! 3. Pass the model into the shared `run_tui_with_model` function.
//!
//! The `codex` provider bypasses the generic completion model path and uses its
//! managed app-server runtime with a dedicated execution flow.
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
        ControlInfo, ControlLockError, ControlLockGuard, ControlPaths, acquire_control_lock,
        cleanup_control_files, ensure_control_token_file, resolve_control_paths,
        write_control_info,
    },
    internal::{
        ai::{
            agent::{
                TaskIntent, ToolLoopConfig,
                profile::{AgentProfileRouter, AgentsConfig, load_profiles},
            },
            codex as agent_codex,
            commands::{CommandDispatcher, load_commands},
            completion::{
                CompletionError, CompletionModel, CompletionReasoningEffort, CompletionRequest,
                CompletionResponse, CompletionThinking, CompletionUsage,
            },
            context_budget::ContextBudget,
            history::HistoryManager,
            hooks::HookRunner,
            mcp::server::LibraMcpServer,
            projection::{ProjectionRebuilder, ProjectionResolver, ThreadBundle},
            prompt::{ContextMode, SystemPromptBuilder},
            providers::{
                anthropic::CLAUDE_3_5_SONNET, gemini::GEMINI_2_5_FLASH, kimi::KIMI_K2_6,
                openai::GPT_4O_MINI, zhipu::GLM_5,
            },
            runtime::{ToolBoundaryRuntime, TracingAuditSink},
            sandbox::{
                ApprovalCachePolicy, ApprovalStore, AskForApproval, DEFAULT_APPROVAL_TTL,
                ExecApprovalRequest, NetworkAccess, SandboxPermissions, SandboxPolicy,
                ToolApprovalContext, ToolRuntimeContext, ToolSandboxContext,
            },
            session::{SessionState, SessionStore},
            skills::{SkillDispatcher, load_skills},
            sources::{SourcePool, register_builtin_mcp_source_from_project_config},
            tools::{
                ToolRegistry, ToolRegistryBuilder,
                context::UserInputRequest,
                handlers::{
                    ApplyPatchHandler, GrepFilesHandler, ListDirHandler, McpBridgeHandler,
                    PlanHandler, ReadFileHandler, RequestUserInputHandler, SearchFilesHandler,
                    ShellHandler, SubmitIntentDraftHandler, SubmitPlanDraftHandler,
                    SubmitTaskCompleteHandler, WebSearchHandler, register_semantic_handlers,
                },
            },
            usage::{UsageContext, UsagePriceTable, UsageRecorder},
            web::{
                WebServerHandle, WebServerOptions,
                code_ui::{
                    CodeUiCapabilities, CodeUiControllerKind, CodeUiInitialController,
                    CodeUiInteractionStatus, CodeUiProviderAdapter, CodeUiProviderInfo,
                    CodeUiRuntimeHandle, CodeUiRuntimeOptions, CodeUiSession,
                    CodeUiSessionSnapshot, CodeUiSessionStatus, CodeUiTranscriptEntry,
                    CodeUiTranscriptEntryKind, ReadOnlyCodeUiAdapter, initial_snapshot,
                    snapshot_from_thread_bundle,
                },
                headless::{
                    HeadlessCodeRuntime, HeadlessSessionPersistence, headless_capabilities,
                },
                start as start_web_server,
            },
        },
        db::establish_connection,
        tui::{
            App, AppConfig, ExitReason, Tui, TuiCodeUiAdapter, control::TuiControlCommand,
            tui_init, tui_restore,
        },
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        storage::local::LocalStorage,
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

/// Local TUI automation control mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlMode {
    /// Keep the current loopback-only read behavior; no write token is created.
    Observe,
    /// Enable local automation write control with token and controller checks.
    Write,
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
    /// Browser controllers cannot attach. Default for normal TUI sessions and
    /// for `--web-only` against non-Codex providers.
    #[default]
    Off,
    /// Browser controllers may attach as long as the bound `--host` is
    /// loopback. Default for `--web-only --provider codex`.
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
/// explicit human confirmation in the TUI.
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

/// Developer-selected network access policy for TUI execution.
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
/// `code` launches the interactive Libra Code session in one of three
/// modes: TUI (the default), web-only (`--web` / `--web-only`), or
/// stdio. The banner pins the most common invocations across modes
/// (TUI default, web-only with a specific provider, `--browser-control
/// loopback`, `--control write` for local automation write control,
/// resume by thread id, plan mode, and `--env-file` for vault-less
/// provider bootstrap) so users see the right entry point without
/// reading the design doc. Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
pub const CODE_EXAMPLES: &str = "\
EXAMPLES:
    libra code                                       Launch the default TUI session
    libra code --provider deepseek --model deepseek-reasoner
                                                     Pick a provider/model at startup
    libra code --web                                 Run the web server only (no TUI); alias for --web-only
    libra code --web-only --provider ollama --port 4400
                                                     Browser-driven session against a local Ollama
    libra code --web-only --provider codex --browser-control loopback
                                                     Allow browser write control over loopback
    libra code --control write                       Enable local automation write control (token + controller checks)
    libra code --resume <thread-uuid>                Resume a prior canonical thread
    libra code --plan-mode                           Start in plan-only mode (no apply)
    libra code --env-file .env.test                  Load provider keys from a dotenv-style file
    libra code --stdio                               Pipe-driven session for embedding";

/// Command-line arguments for `libra code`.
///
/// This struct is parsed by `clap` and drives all three operating modes
/// (TUI, web-only, stdio). Many flags are mode-specific and validated
/// at runtime by [`validate_mode_args`].
#[derive(Parser, Debug)]
#[command(after_help = CODE_EXAMPLES)]
pub struct CodeArgs {
    /// Run the web server only (no TUI). Alias: `--web`.
    #[arg(long, alias = "web", conflicts_with = "stdio")]
    pub web_only: bool,

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

    /// Local TUI automation control mode.
    #[arg(long, value_enum, default_value_t = ControlMode::Observe)]
    pub control: ControlMode,

    /// Browser write-control posture (`off` | `loopback`).
    ///
    /// Defaults are mode-specific:
    /// - normal TUI session → `off`
    /// - `--web-only --provider codex` → `loopback`
    /// - `--web-only` with any other provider → `off`
    ///
    /// Selecting `loopback` is rejected when `--host` is not a loopback
    /// address, and the flag is incompatible with `--stdio`.
    #[arg(long = "browser-control", value_enum, conflicts_with = "stdio")]
    pub browser_control: Option<BrowserControlMode>,

    /// Path to the local automation control token file
    #[arg(long, value_name = "PATH")]
    pub control_token_file: Option<PathBuf>,

    /// Path to the local automation control discovery info file
    #[arg(long, value_name = "PATH")]
    pub control_info_file: Option<PathBuf>,

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

    /// Network access policy for TUI shell and gate execution.
    #[arg(long, value_enum, default_value_t = CodeNetworkAccess::Deny)]
    pub network_access: CodeNetworkAccess,

    /// Port for the embedded MCP server to listen on
    #[arg(long, value_name = "PORT", default_value_t = DEFAULT_MCP_PORT)]
    pub mcp_port: u16,

    /// Run the MCP server over Stdio (for Claude Desktop integration)
    #[arg(long, alias = "mcp-stdio", conflicts_with = "web_only")]
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
/// loop. Libra's own intent / plan drafting tool loop (`phase0_plan_tool_loop_config` /
/// `phase1_plan_tool_loop_config` in `src/internal/tui/app.rs`) requires a
/// generic `CompletionModel` and is bypassed when `managed_code_ui_runtime`
/// is set (the Codex runtime is a managed backend, not a completion model —
/// see the bypass at `src/internal/tui/app.rs` near
/// `if self.managed_code_ui_runtime.is_none() && should_route_plain_message_to_plan(...)`).
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

/// Entry point for the `libra code` subcommand.
///
/// Validates CLI flag combinations, then dispatches to one of three mode-specific
/// execution paths: stdio (MCP over stdin/stdout), web-only (headless HTTP servers),
/// or TUI (full interactive terminal with background servers).
///
/// # Side Effects
/// - May start local web, MCP, and Codex app-server processes depending on mode.
/// - May create `.libra/objects` and connect to `.libra/libra.db` for history.
/// - In TUI mode, may mutate the workspace through registered tools, subject to
///   sandbox and approval policy.
/// - In stdio mode, owns stdin/stdout for the MCP session.
///
/// # Errors
/// Returns [`CliError`] for invalid mode combinations, provider credential
/// failures, network bind failures, Codex app-server startup failures, or
/// terminal/session initialization failures. Error classification follows
/// `docs/development/cli-error-contract-design.md`.
pub async fn execute(args: CodeArgs, output: &OutputConfig) -> CliResult<()> {
    validate_mode_args(&args, output).map_err(CliError::command_usage)?;
    if args.stdio {
        execute_stdio(&args).await
    } else if args.web_only {
        execute_web_only(&args).await
    } else {
        execute_tui(args).await
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

impl McpServerHandle {
    async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.join.await;
        let pending = match self.connection_tasks.lock() {
            Ok(mut handles) => std::mem::take(&mut *handles),
            Err(_) => Vec::new(),
        };
        for handle in pending {
            handle.abort();
            let _ = handle.await;
        }
    }
}

// ---------------------------------------------------------------------------
// Mode: Web-only — headless web + MCP servers (no TUI)
// ---------------------------------------------------------------------------

/// Which Code UI runtime a `--web-only` invocation dispatches to, decided
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

/// Runs the web server and MCP server without a terminal UI.
///
/// Blocks on `Ctrl-C`, then performs graceful shutdown of both servers.
/// This mode is useful for remote/headless environments where the user
/// interacts through a browser or external MCP client.
///
/// # Side Effects
/// - Starts the embedded web server and Streamable HTTP MCP server.
/// - For the Codex provider, starts and later shuts down a managed Codex
///   app-server child process.
/// - Prints connection details to stdout and listens for `Ctrl-C`.
///
/// # Errors
/// Returns [`CliError`] when the working directory cannot be resolved, the web
/// or MCP listener cannot bind, the Codex app-server fails to start, or the
/// selected host would expose loopback-only browser control.
async fn execute_web_only(args: &CodeArgs) -> CliResult<()> {
    let working_dir = resolve_code_working_dir(args)?;
    let browser_control = resolve_browser_control_mode(args)?;
    let control_runtime = prepare_control_runtime(args, &working_dir).await?;
    let mcp_server = init_mcp_server(&working_dir).await;

    let mut managed_codex_server = None;
    let code_ui_runtime =
        if web_only_runtime_kind(args.provider) == WebOnlyRuntimeKind::ManagedCodexAppServer {
            let server =
                start_managed_codex_server(&args.codex_bin, args.codex_port, &working_dir).await?;
            println!("Starting Libra Code Web UI with Codex provider");
            println!("Working directory: {}", working_dir.display());
            println!("Codex WebSocket: {}", server.ws_url);
            println!("Codex app-server: auto-started");
            println!("Browser control: {}", browser_control.as_str());
            managed_codex_server = Some(server);

            let ws_url = managed_codex_server
                .as_ref()
                .map(|server| server.ws_url.as_str())
                .unwrap_or_default();
            start_codex_code_ui_runtime(
                args,
                &working_dir,
                ws_url,
                mcp_server.clone(),
                browser_control == BrowserControlMode::Loopback,
                CodeUiInitialController::Unclaimed,
            )
            .await?
        } else {
            let storage_root = resolve_storage_root(&working_dir);
            let session_store = Arc::new(SessionStore::from_storage_path(&storage_root));
            let session_state =
                load_or_create_headless_web_session_state(args, &working_dir, &session_store)?;
            // Phase 3 v0 routes the supported providers through the new
            // headless runtime. Anything not yet hooked up keeps the read-only
            // placeholder so we fail closed rather than panicking on attach.
            match build_non_codex_headless_runtime(
                args,
                &working_dir,
                session_store,
                session_state,
                browser_control == BrowserControlMode::Loopback,
            )
            .await?
            {
                Some(runtime) => {
                    println!("Starting Libra Code Web UI in headless mode");
                    println!("Working directory: {}", working_dir.display());
                    println!("Provider: {:?}", args.provider);
                    println!("Browser control: {}", browser_control.as_str());
                    runtime
                }
                None => build_placeholder_web_code_ui_runtime(args, &working_dir).await,
            }
        };
    mcp_server.set_code_ui_session(code_ui_runtime.adapter().session());

    let web_handle = match start_web_server(
        &args.host,
        args.port,
        working_dir.clone(),
        WebServerOptions {
            code_ui: Some(code_ui_runtime.clone()),
            automation_control_token: control_runtime.token.clone(),
            audit_sink: None,
        },
    )
    .await
    {
        Ok(handle) => handle,
        Err(err) => {
            let _ = code_ui_runtime.shutdown().await;
            if let Some(server) = managed_codex_server.as_mut() {
                server.shutdown().await;
            }
            return Err(
                CliError::network(format!("failed to start web server: {err}"))
                    .with_detail("component", "web_server"),
            );
        }
    };
    let base_url = format!("http://{}", web_handle.addr);
    let thread_id = code_ui_runtime.snapshot().await.thread_id;
    if let Err(error) =
        control_runtime.write_info_file(&working_dir, base_url.clone(), None, thread_id.clone())
    {
        let _ = code_ui_runtime.shutdown().await;
        if let Some(server) = managed_codex_server.as_mut() {
            server.shutdown().await;
        }
        web_handle.shutdown().await;
        return Err(error);
    }
    println!("Libra Code server running at {base_url}");

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
                let _ = code_ui_runtime.shutdown().await;
                if let Some(server) = managed_codex_server.as_mut() {
                    server.shutdown().await;
                }
                web_handle.shutdown().await;
                handle.shutdown().await;
                return Err(error);
            }
            println!("MCP: {mcp_url}");
            handle
        }
        Err(err) => {
            let _ = code_ui_runtime.shutdown().await;
            if let Some(server) = managed_codex_server.as_mut() {
                server.shutdown().await;
            }
            web_handle.shutdown().await;
            return Err(
                CliError::network(format!("failed to start MCP server: {err}"))
                    .with_detail("component", "mcp_server"),
            );
        }
    };

    let _ = tokio::signal::ctrl_c().await;
    let _ = code_ui_runtime.shutdown().await;
    web_handle.shutdown().await;
    mcp_handle.shutdown().await;
    if let Some(server) = managed_codex_server.as_mut() {
        server.shutdown().await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mode: TUI — full interactive terminal with background servers
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

/// Resolve the **effective** [`CodeProvider`] enum that downstream
/// provider-specific helpers should dispatch on (OC-Phase 2 P2.4).
///
/// When `--agent <name>` is set and the agent's profile carries a structured
/// `model: provider/model` binding, the effective provider is the one named
/// by the binding's `provider_id`. Otherwise the effective provider is the
/// CLI `--provider` default.
///
/// An agent binding whose `provider_id` does NOT map to a known
/// [`CodeProvider`] variant is rejected with a `command_usage` error.
/// Silently falling back to `args.provider` would leave the system prompt /
/// context-budget / completion knobs computed against the CLI provider
/// while the model is ultimately built for a different (or non-existent)
/// provider — a partial-misconfiguration trap. The list of known provider
/// ids stays in lock-step with [`provider_id::ALL_PRODUCTION`] (plus
/// `FAKE` under the `test-provider` feature).
fn effective_code_provider_for_args(
    args: &CodeArgs,
    working_dir: &std::path::Path,
) -> CliResult<CodeProvider> {
    use crate::internal::ai::providers::runtime::provider_id;

    let Some(binding) = resolve_agent_binding_override(args, working_dir)? else {
        return Ok(args.provider);
    };
    let mapped = match binding.provider_id.as_str() {
        provider_id::GEMINI => Some(CodeProvider::Gemini),
        provider_id::OPENAI => Some(CodeProvider::Openai),
        provider_id::ANTHROPIC => Some(CodeProvider::Anthropic),
        provider_id::DEEPSEEK => Some(CodeProvider::Deepseek),
        provider_id::KIMI => Some(CodeProvider::Kimi),
        provider_id::ZHIPU => Some(CodeProvider::Zhipu),
        provider_id::OLLAMA => Some(CodeProvider::Ollama),
        #[cfg(feature = "test-provider")]
        provider_id::FAKE => Some(CodeProvider::Fake),
        _ => None,
    };
    mapped.ok_or_else(|| {
        CliError::command_usage(format!(
            "agent '{}' selects provider '{}', which is not a known `--provider` value. \
             Pick a binding whose provider id is one of: {}",
            args.agent.as_deref().unwrap_or("?"),
            binding.provider_id,
            provider_id::ALL_PRODUCTION.join(", "),
        ))
    })
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

/// Main TUI execution path: initializes the AI provider, builds the tool
/// registry, starts background web/MCP servers, and launches the interactive
/// terminal application.
///
/// This function handles provider-specific client creation (API key validation,
/// model selection) and delegates the actual TUI lifecycle to [`run_tui_with_model`].
///
/// # Side Effects
/// - Reads provider credentials from environment variables and optional dotenv
///   files.
/// - Registers local file, shell, planning, and MCP bridge tools for the agent.
/// - May start web/MCP background services and a managed Codex app-server.
/// - May mutate the workspace through tools when the selected context permits it.
///
/// # Errors
/// Returns [`CliError`] for missing credentials, invalid provider configuration,
/// unsafe mode/host combinations, provider bootstrap failures, or failures from
/// the shared TUI lifecycle.
async fn execute_tui(args: CodeArgs) -> CliResult<()> {
    let working_dir = resolve_code_working_dir(&args)?;
    let env_file = load_code_env_file(args.env_file.as_deref())?;
    let browser_control = resolve_browser_control_mode(&args)?;
    let control_runtime = prepare_control_runtime(&args, &working_dir).await?;

    let task_intent = task_intent_for_context(args.context);
    // OC-Phase 2 P2.4: resolve `--agent <name>` once before any provider-
    // specific knob (context budget, completion thinking / reasoning /
    // stream, preamble) is computed. When the agent's spec carries a
    // structured binding, the effective provider may differ from the CLI
    // `--provider` default; downstream computations need the agent's
    // provider, not the CLI one.
    let effective_provider = effective_code_provider_for_args(&args, &working_dir)?;
    let effective_model_for_preamble = if effective_provider == args.provider {
        args.model.as_deref().map(str::to_string)
    } else {
        // The agent override path resolves the concrete model id later
        // inside `build_any_completion_model_for_args`; here we only need
        // it for `system_preamble`'s context budget defaulting, where
        // `None` falls back to the provider's flagship via
        // [`default_context_budget_model`].
        None
    };
    let preamble = system_preamble(
        &working_dir,
        args.context,
        effective_provider,
        effective_model_for_preamble.as_deref(),
    );
    let temperature = args.temperature;
    let thinking = completion_thinking_for_provider(effective_provider, &args);
    let reasoning_effort = completion_reasoning_effort_for_provider(effective_provider, &args);
    let stream = completion_stream_for_provider(effective_provider, &args);
    let preserve_reasoning_content = preserve_reasoning_content_for_provider(effective_provider);
    let resume_thread_id = args.resume.clone();
    let host = args.host.clone();
    let trace_id = resume_thread_id
        .as_deref()
        .and_then(|thread_id| Uuid::parse_str(thread_id).ok())
        .unwrap_or_else(Uuid::new_v4);

    // Prepare MCP server instance shared between the HTTP transport and TUI bridge.
    // INVARIANT: the same server instance backs both transports so an agent sees
    // one coherent history/object store regardless of whether a tool is invoked
    // through HTTP MCP or the in-process TUI bridge.
    let mcp_server = init_mcp_server(&working_dir).await;

    // Create the bridge channel for request_user_input tool <-> TUI communication.
    let (user_input_tx, user_input_rx) = tokio::sync::mpsc::unbounded_channel::<UserInputRequest>();
    let (exec_approval_tx, exec_approval_rx) =
        tokio::sync::mpsc::unbounded_channel::<ExecApprovalRequest>();

    // Build registry: basic file tools + MCP workflow tools.
    //
    // AI user story: let a coding agent inspect files, search context, make
    // bounded edits, run verification commands, ask the human for missing
    // choices, and record structured planning artifacts without leaving the
    // sandbox/approval model.
    let mut builder = ToolRegistryBuilder::with_working_dir(working_dir.clone())
        .hardening(ToolBoundaryRuntime::system(
            trace_id,
            Arc::new(TracingAuditSink),
        ))
        .register("read_file", Arc::new(ReadFileHandler))
        .register("list_dir", Arc::new(ListDirHandler))
        .register("grep_files", Arc::new(GrepFilesHandler))
        .register("search_files", Arc::new(SearchFilesHandler))
        .register("web_search", Arc::new(WebSearchHandler))
        .register("apply_patch", Arc::new(ApplyPatchHandler))
        .register("shell", Arc::new(ShellHandler))
        .register("update_plan", Arc::new(PlanHandler))
        .register("submit_intent_draft", Arc::new(SubmitIntentDraftHandler))
        .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
        .register("submit_task_complete", Arc::new(SubmitTaskCompleteHandler))
        .register(
            "request_user_input",
            Arc::new(RequestUserInputHandler::new(user_input_tx.clone())),
        );
    builder = register_semantic_handlers(builder);

    // AI user story: MCP bridge tools let the agent persist intent/task/run,
    // evidence, provenance, and Libra VCS operations in the same workflow graph
    // that external MCP clients use. Keep these names aligned with
    // `docs/ai/intentspec_typical.yaml` and `docs/ai/workflow.md`.
    for (name, handler) in McpBridgeHandler::all_handlers(mcp_server.clone()) {
        builder = builder.register(name, handler);
    }

    let registry = Arc::new(builder.build());
    let allowed_tools = registry.filter_by_intent(task_intent);

    // Single source of truth for the args -> approval-context mapping
    // (criterion 2), shared with the headless launch path.
    let approval_cfg = tui_approval_config_from_args(&args, registry.working_dir());
    let provider_name = format!("{:?}", args.provider).to_lowercase();
    let launch_config = TuiLaunchConfig {
        host,
        port: args.port,
        mcp_port: args.mcp_port,
        registry,
        preamble,
        temperature,
        thinking,
        reasoning_effort,
        stream,
        preserve_reasoning_content,
        allowed_tools: Some(allowed_tools),
        auto_classify_first_user_message: args.context.is_none(),
        context: args.context,
        resume_thread_id,
        approval_policy: approval_cfg.policy,
        allow_all_commands: approval_cfg.allow_all_commands,
        approval_ttl: approval_cfg.ttl,
        approval_cache_policy: approval_cfg.cache_policy,
        network_access: args.network_access.is_allowed(),
        user_input_rx,
        exec_approval_rx,
        exec_approval_tx,
        mcp_server,
        control_runtime,
        browser_control,
        initial_goal: args.goal.clone(),
    };

    // Create agent based on provider. Every non-Codex provider funnels
    // through `ProviderFactory`; Codex keeps its own managed-runtime path.
    match args.provider {
        CodeProvider::Codex => {
            let mut server =
                start_managed_codex_server(&args.codex_bin, args.codex_port, &working_dir).await?;
            let browser_write_enabled =
                launch_config.browser_control == BrowserControlMode::Loopback;
            // `LocalTui` keeps the terminal as the visible owner while letting
            // browser/automation leases attach when their writer is enabled.
            // Fall back to `Fixed { Tui }` only when both writers are off
            // (read-only observe).
            let initial_controller =
                if launch_config.control_runtime.is_write() || browser_write_enabled {
                    CodeUiInitialController::LocalTui {
                        owner_label: "Terminal UI".to_string(),
                        reason: Some("The terminal UI controls this live Codex run".to_string()),
                    }
                } else {
                    CodeUiInitialController::Fixed {
                        kind: CodeUiControllerKind::Tui,
                        owner_label: "Terminal UI".to_string(),
                        reason: Some("The terminal UI controls this live Codex run".to_string()),
                    }
                };
            let code_ui_runtime = match start_codex_code_ui_runtime(
                &args,
                &working_dir,
                &server.ws_url,
                launch_config.mcp_server.clone(),
                browser_write_enabled,
                initial_controller,
            )
            .await
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    server.shutdown().await;
                    return Err(error);
                }
            };
            let model_name = args.model.clone().unwrap_or_else(|| "codex".to_string());
            let result = run_tui_with_managed_code_runtime(
                code_ui_runtime,
                launch_config,
                model_name,
                provider_name,
            )
            .await;
            server.shutdown().await;
            result?;
        }
        _ => {
            // OC-Phase 2 P2.4: the helper returns the *effective* provider
            // name so usage / UI metadata reports the agent-selected
            // provider after a `--agent <name>` override, not the CLI
            // `--provider` default that the helper started from.
            let (model, model_name, effective_provider_name) =
                build_any_completion_model_for_args(&args, &env_file, &working_dir)?;
            run_tui_with_model(model, launch_config, model_name, effective_provider_name).await?;
        }
    }

    Ok(())
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
/// [`ManagedCodexServer::shutdown`] sends SIGKILL and waits up to 5 seconds.
struct ManagedCodexServer {
    ws_url: String,
    child: Child,
}

impl ManagedCodexServer {
    /// Gracefully shuts down the managed Codex app-server process.
    ///
    /// If the child process has already exited (`id()` returns `None`), this is
    /// a no-op. Otherwise it sends a kill signal via `start_kill()` and waits up
    /// to 5 seconds for the process to terminate. If the timeout expires the
    /// process is abandoned (the OS will reap it when the handle is dropped).
    async fn shutdown(&mut self) {
        if self.child.id().is_none() {
            return;
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
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
}

impl ControlRuntimeConfig {
    fn is_write(&self) -> bool {
        self.mode == ControlMode::Write
    }

    fn mode_name(&self) -> &'static str {
        match self.mode {
            ControlMode::Observe => "observe",
            ControlMode::Write => "write",
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

        let info = ControlInfo {
            version: 1,
            mode: self.mode_name().to_string(),
            pid: std::process::id(),
            base_url,
            mcp_url,
            working_dir: working_dir.to_path_buf(),
            thread_id,
            started_at: self.started_at,
        };
        write_control_info(&self.paths.info, &info).map_err(|error| {
            CliError::fatal(format!(
                "failed to write local TUI control info '{}': {error}",
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
        }),
        ControlMode::Write => {
            let lock_guard = acquire_control_lock(&paths.lock).map_err(|error| match error {
                ControlLockError::AlreadyHeld { .. } => CliError::conflict(error.to_string()),
                ControlLockError::Io(error) => CliError::io(format!(
                    "failed to acquire local TUI control lock '{}': {error}",
                    paths.lock.display()
                )),
            })?;
            let token = ensure_control_token_file(&paths.token)
                .await
                .map_err(|error| {
                    CliError::fatal(format!(
                        "failed to prepare local TUI control token '{}': {error}",
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
            })
        }
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
///   - `--web-only --provider codex` → `loopback` (matches the existing
///     "browser write enabled" default for managed Codex sessions),
///   - all other entry points → `off` (TUI sessions and non-Codex
///     `--web-only` placeholders).
///
/// `loopback` further requires that `--host` is a loopback address; this is
/// validated up-front so we fail closed before any port is bound.
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
    if args.web_only && matches!(args.provider, CodeProvider::Codex) {
        BrowserControlMode::Loopback
    } else {
        BrowserControlMode::Off
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

/// Bootstraps the `SessionState` for a `--web-only` non-Codex headless run.
///
/// The `create` path (`SessionState::new`) is the only one reachable through
/// the CLI: `validate_mode_args`/`reject_non_tui_flags` reject `--resume` in
/// every non-TUI mode, so `args.resume` is always `None` here. `--resume` is
/// TUI-only by design (see `docs/development/tracing/code.md` §"Session /
/// graph" and `docs/commands/code.md`), not deferred work — persisted headless
/// resume is reachable only through the TUI path.
///
/// The session-layer `load_for_thread_id` branch below is therefore retained
/// only as defense-in-depth so this helper keeps a single, correct
/// load-or-create shape (identical to the TUI resume bootstrap): if a future
/// caller ever supplies a resume id, it loads the right session instead of
/// silently discarding it. It is intentionally not reachable via `libra code
/// --web-only --resume`.
fn load_or_create_headless_web_session_state(
    args: &CodeArgs,
    working_dir: &Path,
    session_store: &Arc<SessionStore>,
) -> CliResult<SessionState> {
    let working_dir_str = working_dir.to_string_lossy().to_string();
    // NOTE: unreachable via the CLI today — `--resume` is rejected before we
    // get here in web-only mode (TUI-only by design). Kept for a uniform
    // load-or-create shape; see this function's doc comment.
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

fn build_headless_web_code_ui_snapshot(
    working_dir: &Path,
    provider: CodeUiProviderInfo,
    capabilities: CodeUiCapabilities,
    session: &SessionState,
) -> CodeUiSessionSnapshot {
    let working_dir = working_dir.to_string_lossy().to_string();
    let mut snapshot = session
        .metadata
        .get(HEADLESS_CODE_UI_SNAPSHOT_METADATA_KEY)
        .and_then(|value| serde_json::from_value::<CodeUiSessionSnapshot>(value.clone()).ok())
        .unwrap_or_else(|| {
            initial_snapshot(working_dir.clone(), provider.clone(), capabilities.clone())
        });

    snapshot.session_id = session.id.clone();
    snapshot.thread_id =
        Some(session_canonical_thread_id(session).unwrap_or_else(|| session.id.clone()));
    snapshot.working_dir = working_dir;
    snapshot.provider = provider;
    snapshot.capabilities = capabilities;
    if snapshot.transcript.is_empty() {
        snapshot.transcript = build_tui_code_ui_transcript(session);
    }

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
    snapshot.status = if has_pending_interaction {
        CodeUiSessionStatus::AwaitingInteraction
    } else {
        CodeUiSessionStatus::Idle
    };
    snapshot.updated_at = now;
    snapshot
}

/// Build a headless Code UI runtime for `--web-only` non-Codex providers.
///
/// Constructs a minimal local-read-only [`ToolRegistry`]
/// and wires it into a [`HeadlessCodeRuntime`] so the browser composer can
/// drive a real agent turn against the supplied `model`. The result is
/// exposed through [`CodeUiRuntimeHandle`] just like the TUI flow, so the
/// rest of `start_web_server` can use it without per-mode special cases.
///
/// `browser_write_enabled` should mirror the resolved
/// [`BrowserControlMode::Loopback`] so the runtime advertises browser writes
/// in the snapshot capabilities. The initial controller is `Unclaimed` —
/// the browser is the only writer in headless mode, no TUI to hand off from.
async fn build_headless_web_code_ui_runtime<M>(
    args: &CodeArgs,
    working_dir: &Path,
    session_bootstrap: HeadlessWebSessionBootstrap,
    model: M,
    model_name: String,
    approval_channels: HeadlessApprovalChannels,
    browser_write_enabled: bool,
) -> CliResult<Arc<CodeUiRuntimeHandle>>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    use crate::internal::ai::agent::runtime::tool_loop::ToolLoopConfig;

    let HeadlessWebSessionBootstrap {
        store: session_store,
        state: session_state,
    } = session_bootstrap;
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
    let snapshot = build_headless_web_code_ui_snapshot(
        working_dir,
        provider,
        capabilities.clone(),
        &session_state,
    );
    let session = CodeUiSession::new(snapshot);
    let persistence = HeadlessSessionPersistence::new(session_store, session_state);

    let (user_input_tx, user_input_rx) = mpsc::unbounded_channel::<UserInputRequest>();
    let runtime_context = Some(default_tui_runtime_context(
        working_dir,
        args.context,
        tui_approval_config_from_args(args, working_dir),
        args.network_access.is_allowed(),
        exec_approval_tx,
    ));

    let registry = build_headless_tool_registry(working_dir, user_input_tx);
    let preamble = system_preamble(working_dir, args.context, args.provider, Some(&model_name));
    let preserve_reasoning_content = preserve_reasoning_content_for_provider(args.provider);
    let temperature = args.temperature;
    let thinking = completion_thinking_for_args(args);
    let reasoning_effort = completion_reasoning_effort_for_args(args);
    let stream = completion_stream_for_args(args);

    let config_factory: Arc<dyn Fn() -> ToolLoopConfig + Send + Sync> =
        Arc::new(move || ToolLoopConfig {
            preamble: Some(preamble.clone()),
            temperature,
            thinking,
            reasoning_effort,
            stream,
            preserve_reasoning_content,
            runtime_context: runtime_context.clone(),
            ..Default::default()
        });

    let adapter = HeadlessCodeRuntime::new_with_persistence(
        session,
        capabilities,
        model,
        registry,
        user_input_rx,
        exec_approval_rx,
        config_factory,
        initial_history,
        Some(persistence),
    );

    let mut runtime_options = CodeUiRuntimeOptions::new(
        browser_write_enabled,
        false,
        CodeUiInitialController::Unclaimed,
    );
    runtime_options.lease_duration = code_ui_test_lease_duration_override()?;
    Ok(CodeUiRuntimeHandle::build_with_options(adapter, runtime_options).await)
}

fn build_headless_tool_registry(
    working_dir: &Path,
    user_input_tx: mpsc::UnboundedSender<UserInputRequest>,
) -> Arc<ToolRegistry> {
    // Headless web mode now reuses the same ToolRuntimeContext path as TUI:
    // shell/apply_patch route through sandbox + exec approval, web_search sees
    // the CLI network policy, and pending approvals surface through
    // CodeUiInteractionRequest. `submit_plan_draft` is exposed because
    // headless projects it into plans[]; workflow tools that require a
    // session driver (`task`, `submit_intent_draft`) remain gated.
    let trace_id = uuid::Uuid::new_v4();
    let builder = ToolRegistryBuilder::with_working_dir(working_dir.to_path_buf())
        .hardening(ToolBoundaryRuntime::system(
            trace_id,
            Arc::new(TracingAuditSink),
        ))
        .register("read_file", Arc::new(ReadFileHandler))
        .register("list_dir", Arc::new(ListDirHandler))
        .register("grep_files", Arc::new(GrepFilesHandler))
        .register("search_files", Arc::new(SearchFilesHandler))
        .register("web_search", Arc::new(WebSearchHandler))
        .register("apply_patch", Arc::new(ApplyPatchHandler))
        .register("shell", Arc::new(ShellHandler))
        .register("update_plan", Arc::new(PlanHandler))
        .register("submit_plan_draft", Arc::new(SubmitPlanDraftHandler))
        .register(
            "request_user_input",
            Arc::new(RequestUserInputHandler::new(user_input_tx)),
        );
    Arc::new(register_semantic_handlers(builder).build())
}

/// Construct the appropriate provider client and wrap it in
/// [`build_headless_web_code_ui_runtime`]. Returns `None` when the requested
/// provider is not yet wired into the headless path so the caller can fall
/// back to the read-only placeholder gracefully.
///
/// v0 now routes several non-Codex providers through the same provider-factory
/// bootstrap used by TUI. This keeps API-key/base-URL resolution centralized and
/// ensures `--web-only` behavior stays aligned with existing provider construction.
///
/// The placeholder path is still available for providers that are not in this
/// dispatch arm or fail during bootstrap for other reasons.
async fn build_non_codex_headless_runtime(
    args: &CodeArgs,
    working_dir: &Path,
    session_store: Arc<SessionStore>,
    session_state: SessionState,
    browser_write_enabled: bool,
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
                build_any_completion_model_for_args(args, &CodeEnvFile::default(), working_dir)?;
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
                build_any_completion_model_for_args(args, &CodeEnvFile::default(), working_dir)?;
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
            "Interactive browser control is fully implemented for `--provider codex`. For other providers, launch `libra code` without `--web-only` to observe the live terminal session in the browser."
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
    let ui_mode = match &initial_controller {
        CodeUiInitialController::Fixed {
            kind: CodeUiControllerKind::Tui,
            ..
        } => Some("tui".to_string()),
        CodeUiInitialController::Fixed {
            kind: CodeUiControllerKind::Cli,
            ..
        } => Some("cli".to_string()),
        CodeUiInitialController::LocalTui { .. } => Some("managed-tui".to_string()),
        _ => Some("web".to_string()),
    };
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
        ui_mode,
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
    let mut child = spawn_codex_app_server(codex_bin, &ws_url, working_dir)?;

    if let Err(err) = wait_for_codex_ready(&ws_url).await {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(err);
    }

    Ok(ManagedCodexServer { ws_url, child })
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
        .stderr(Stdio::null());
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
        .stderr(Stdio::null());
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

// ---------------------------------------------------------------------------
// TUI launch configuration and model abstraction
// ---------------------------------------------------------------------------

/// Aggregates all parameters needed to launch the TUI application.
///
/// This struct is built once in [`execute_tui`] and consumed by
/// [`run_tui_with_model`]. It bundles network config, tool registry,
/// prompt/temperature settings, session state, and inter-component channels.
struct TuiLaunchConfig {
    host: String,
    port: u16,
    mcp_port: u16,
    registry: Arc<ToolRegistry>,
    preamble: String,
    temperature: Option<f64>,
    thinking: Option<CompletionThinking>,
    reasoning_effort: Option<CompletionReasoningEffort>,
    stream: Option<bool>,
    preserve_reasoning_content: bool,
    allowed_tools: Option<Vec<String>>,
    auto_classify_first_user_message: bool,
    context: Option<CodeContext>,
    resume_thread_id: Option<String>,
    approval_policy: AskForApproval,
    allow_all_commands: bool,
    approval_ttl: Duration,
    approval_cache_policy: ApprovalCachePolicy,
    network_access: bool,
    user_input_rx: tokio::sync::mpsc::UnboundedReceiver<UserInputRequest>,
    exec_approval_rx: tokio::sync::mpsc::UnboundedReceiver<ExecApprovalRequest>,
    exec_approval_tx: tokio::sync::mpsc::UnboundedSender<ExecApprovalRequest>,
    mcp_server: Arc<LibraMcpServer>,
    control_runtime: ControlRuntimeConfig,
    browser_control: BrowserControlMode,
    /// Goal objective passed via `libra code --goal`. The TUI app
    /// uses this to bootstrap a `GoalSpec` and seed
    /// [`AppConfig::initial_goal`] before the first turn.
    initial_goal: Option<String>,
}

#[derive(Clone)]
struct ManagedCodeRuntimeModel;

impl CompletionModel for ManagedCodeRuntimeModel {
    type Response = ();

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::NotImplemented(
            "managed code runtime handles turns outside the generic completion model".to_string(),
        ))
    }
}

fn build_tui_code_ui_capabilities() -> CodeUiCapabilities {
    CodeUiCapabilities {
        message_input: true,
        streaming_text: true,
        plan_updates: true,
        tool_calls: true,
        patchsets: true,
        interactive_approvals: true,
        structured_questions: true,
        provider_session_resume: false,
    }
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

#[allow(clippy::too_many_arguments)]
async fn build_tui_code_ui_runtime(
    working_dir: &str,
    session: &SessionState,
    provider_name: &str,
    model_name: &str,
    projection_bundle: Option<&ThreadBundle>,
    code_control_tx: Option<tokio::sync::mpsc::UnboundedSender<TuiControlCommand>>,
    automation_write_enabled: bool,
    browser_write_enabled: bool,
    lease_duration_override: Option<chrono::Duration>,
) -> Arc<CodeUiRuntimeHandle> {
    let capabilities = build_tui_code_ui_capabilities();
    let provider = CodeUiProviderInfo {
        provider: provider_name.to_string(),
        model: Some(model_name.to_string()),
        mode: Some("tui".to_string()),
        managed: false,
    };
    let mut snapshot = if let Some(bundle) = projection_bundle {
        snapshot_from_thread_bundle(
            working_dir.to_string(),
            provider,
            capabilities.clone(),
            bundle,
        )
    } else {
        initial_snapshot(working_dir.to_string(), provider, capabilities.clone())
    };
    if projection_bundle.is_none() {
        snapshot.session_id = session.id.clone();
        snapshot.thread_id = session_canonical_thread_id(session);
    }
    snapshot.transcript = build_tui_code_ui_transcript(session);
    snapshot.updated_at = Utc::now();

    let code_ui_session = CodeUiSession::new(snapshot);
    let adapter: Arc<dyn CodeUiProviderAdapter> = if let Some(control_tx) = code_control_tx {
        TuiCodeUiAdapter::new(code_ui_session, capabilities, control_tx)
    } else {
        ReadOnlyCodeUiAdapter::new(code_ui_session, capabilities)
    };
    // `LocalTui` keeps the terminal as the visible owner but still lets
    // browser/automation leases attach when their write surface is enabled.
    // `Fixed { Tui }` is reserved for sessions where neither writer should
    // ever be allowed to take control (read-only browser observe).
    let initial_controller = if automation_write_enabled || browser_write_enabled {
        CodeUiInitialController::LocalTui {
            owner_label: "Terminal UI".to_string(),
            reason: Some("The terminal UI controls this live session".to_string()),
        }
    } else {
        CodeUiInitialController::Fixed {
            kind: CodeUiControllerKind::Tui,
            owner_label: "Terminal UI".to_string(),
            reason: Some("The terminal UI controls this live session".to_string()),
        }
    };
    let mut runtime_options = CodeUiRuntimeOptions::new(
        browser_write_enabled,
        automation_write_enabled,
        initial_controller,
    );
    runtime_options.lease_duration = lease_duration_override;
    CodeUiRuntimeHandle::build_with_options(adapter, runtime_options).await
}

async fn load_code_ui_projection_bundle(
    working_dir: &Path,
    thread_id: Uuid,
) -> anyhow::Result<Option<ThreadBundle>> {
    let storage_root = resolve_storage_root(working_dir);
    let db_path = storage_root.join("libra.db");
    let db_path = db_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("database path is not valid UTF-8"))?;
    let db_conn = establish_connection(db_path).await?;
    let storage = Arc::new(LocalStorage::new(storage_root.join("objects")));
    let history = HistoryManager::new(storage.clone(), storage_root, Arc::new(db_conn.clone()));
    let rebuilder = ProjectionRebuilder::new(storage.as_ref(), &history);
    let resolver = ProjectionResolver::new(db_conn);
    resolver
        .load_or_rebuild_thread_bundle(thread_id, &rebuilder)
        .await
}

/// Core TUI lifecycle: wires up the terminal, background servers, agent
/// configuration, session persistence, and the interactive `App` event loop.
///
/// This function is generic over the completion model `M`, allowing all
/// providers to share the same TUI setup code. The flow is:
///
/// 1. Load git hooks from the working directory.
/// 2. Build the agent's `ToolLoopConfig` (preamble, temperature, sandbox policy).
/// 3. Initialize the terminal via `tui_init()` with a restore guard.
/// 4. Start the web server and MCP server as background tasks.
/// 5. Load slash commands and agent profiles from disk.
/// 6. Restore or create a new session.
/// 7. Run the `App` event loop until the user exits.
/// 8. Gracefully shut down all background servers.
///
/// # Side Effects
/// - Switches the terminal into TUI mode and restores it on exit.
/// - Starts background web and MCP listeners when their ports are available.
/// - Reads hook, slash-command, profile, session, and projection state from the
///   working directory.
/// - Persists session updates and may drive tool-mediated workspace writes.
///
/// # Errors
/// Returns [`CliError`] for terminal initialization failures, invalid resume
/// thread IDs, missing sessions, session/projection load failures, or fatal app
/// exits reported by the TUI event loop.
async fn run_tui_with_model<M>(
    model: M,
    params: TuiLaunchConfig,
    model_name: String,
    provider_name: String,
) -> CliResult<()>
where
    M: CompletionModel + Clone + 'static,
    M::Response: CompletionUsage,
{
    run_tui_with_model_inner(model, params, model_name, provider_name, None).await
}

async fn run_tui_with_managed_code_runtime(
    code_ui_runtime: Arc<CodeUiRuntimeHandle>,
    params: TuiLaunchConfig,
    model_name: String,
    provider_name: String,
) -> CliResult<()> {
    run_tui_with_model_inner(
        ManagedCodeRuntimeModel,
        params,
        model_name,
        provider_name,
        Some(code_ui_runtime),
    )
    .await
}

/// Formats the post-exit "inspect this thread graph" handoff line printed
/// when the TUI leaves and Libra could derive a canonical thread id.
///
/// The bare `libra graph <thread_id>` form discovers the repository from the
/// current directory. When the code session ran against a different repository
/// (`--repo`/`--cwd`, or simply launched from elsewhere), that discovery would
/// resolve the wrong repo, so the hint appends `--repo <path>` pointing at the
/// session's working directory — matching the remote-repo guidance in
/// `docs/commands/code.md`.
///
/// `current_dir` is the process working directory (`None` when it cannot be
/// resolved). Paths are canonicalized before comparison so `.`/relative/symlink
/// forms of the same directory do not produce a spurious `--repo` suffix; if
/// the current directory is unknown we fail safe by including the explicit
/// `--repo` path.
/// Quote a path for a copy-pasteable shell command when it contains
/// whitespace or shell-special characters; otherwise return it bare so the
/// common case stays readable. POSIX single-quote escaping (`'` -> `'\''`).
fn shell_quote_for_display(value: &str) -> String {
    let needs_quoting = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || "'\"\\$`&|;<>()*?[]{}#~!".contains(c));
    if needs_quoting {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        value.to_string()
    }
}

fn format_graph_handoff_hint(
    thread_id: &str,
    session_working_dir: &Path,
    current_dir: Option<&Path>,
) -> String {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let needs_repo_hint = match current_dir {
        Some(cwd) => canonical(cwd) != canonical(session_working_dir),
        None => true,
    };
    if needs_repo_hint {
        format!(
            "Inspect this thread graph with: libra graph {thread_id} --repo {}",
            shell_quote_for_display(&session_working_dir.display().to_string())
        )
    } else {
        format!("Inspect this thread graph with: libra graph {thread_id}")
    }
}

async fn run_tui_with_model_inner<M>(
    model: M,
    params: TuiLaunchConfig,
    model_name: String,
    provider_name: String,
    managed_code_ui_runtime: Option<Arc<CodeUiRuntimeHandle>>,
) -> CliResult<()>
where
    M: CompletionModel + Clone + 'static,
    M::Response: CompletionUsage,
{
    let registry = params.registry;
    let control_runtime = params.control_runtime;
    let browser_control = params.browser_control;
    let hook_runner = {
        let runner = HookRunner::load(registry.working_dir());
        if runner.has_hooks() {
            Some(std::sync::Arc::new(runner))
        } else {
            None
        }
    };

    let mut config = ToolLoopConfig {
        preamble: Some(params.preamble),
        temperature: params.temperature,
        thinking: params.thinking,
        reasoning_effort: params.reasoning_effort,
        stream: params.stream,
        hook_runner,
        allowed_tools: params.allowed_tools,
        runtime_context: Some(default_tui_runtime_context(
            registry.working_dir(),
            params.context,
            DefaultTuiApprovalConfig {
                policy: params.approval_policy,
                allow_all_commands: params.allow_all_commands,
                ttl: params.approval_ttl,
                cache_policy: params.approval_cache_policy,
            },
            params.network_access,
            params.exec_approval_tx.clone(),
        )),
        max_turns: None,
        preserve_reasoning_content: params.preserve_reasoning_content,
        ..Default::default()
    };

    // Initialize terminal.
    let terminal = match tui_init() {
        Ok(t) => t,
        Err(e) => return Err(CliError::io(format!("failed to initialize terminal: {e}"))),
    };

    // INVARIANT: every successful `tui_init` must install this guard before any
    // await point that can fail, otherwise a later error could leave the user's
    // terminal in raw/alternate-screen mode.
    let _guard = scopeguard::guard((), |_| {
        let _ = tui_restore();
    });

    let tui = Tui::new(terminal);

    // Set up session persistence
    let working_dir_str = registry.working_dir().to_string_lossy().to_string();
    // Capture the resolved session working directory before `registry` is
    // moved into `App::new`; the post-exit graph handoff hint needs it to
    // decide whether to surface a `--repo <path>` suffix for a non-cwd repo.
    let session_working_dir = registry.working_dir().to_path_buf();
    let storage_root = resolve_storage_root(registry.working_dir());
    let session_store = SessionStore::from_storage_path(&storage_root);
    let session = if let Some(thread_id) = params.resume_thread_id.as_deref() {
        // The resume identifier may be either a canonical UUID (planning-bound
        // thread) or a chat-flow session id from `generate_session_id`
        // (millisecond-hex / pid-hex / counter-hex). The store accepts either
        // shape — reject empty input here and let `load_for_thread_id` surface
        // a unified "no session found" error for any unknown identifier.
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
    // v0.17.791 session-bootstrap usage auto-prune: if the
    // operator configured `[usage] retention_days = N` in
    // `config.toml`, drop usage rows older than N days at session
    // start. Soft-failure (logs warn + continues) so a malformed
    // config or DB error doesn't block startup.
    crate::command::usage::auto_prune_at_session_start(&storage_root).await;

    if let Some(usage_recorder) = build_usage_recorder(&storage_root).await {
        config.usage_recorder = Some(usage_recorder);
        config.usage_context = Some(UsageContext {
            session_id: Some(session.id.clone()),
            thread_id: session_canonical_thread_id(&session),
            agent_run_id: None,
            run_id: None,
            provider: provider_name.clone(),
            model: model_name.clone(),
            request_kind: "completion".to_string(),
            intent: None,
            // OC-Phase 5 P5.2: single-agent legacy path. The
            // dispatcher (P5.3) sets this to the active profile name
            // when multi-agent is enabled.
            agent_name: None,
        });
    }

    let automation_write_enabled = control_runtime.is_write();
    let browser_write_enabled = browser_control == BrowserControlMode::Loopback;
    // The TUI control command channel is created whenever any writer
    // (automation or browser) is enabled, so the runtime adapter can route
    // submit/respond/cancel into the TUI app loop. Selecting the adapter
    // based on `code_control_tx.is_some()` would gate browser writes behind
    // `--control write`; gating on the explicit booleans avoids that.
    let (code_control_tx, code_control_rx) = if automation_write_enabled || browser_write_enabled {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<TuiControlCommand>();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let code_ui_runtime = if let Some(runtime) = managed_code_ui_runtime.clone() {
        if let Some(control_tx) = code_control_tx {
            let adapter = runtime.adapter();
            let code_ui_session = adapter.session();
            let capabilities = adapter.capabilities();
            let tui_adapter: Arc<dyn CodeUiProviderAdapter> =
                TuiCodeUiAdapter::new(code_ui_session, capabilities, control_tx);
            let mut runtime_options = CodeUiRuntimeOptions::new(
                browser_write_enabled,
                automation_write_enabled,
                CodeUiInitialController::LocalTui {
                    owner_label: "Terminal UI".to_string(),
                    reason: Some("The terminal UI controls this live managed session".to_string()),
                },
            );
            runtime_options.lease_duration = code_ui_test_lease_duration_override()?;
            CodeUiRuntimeHandle::build_with_options(tui_adapter, runtime_options).await
        } else {
            runtime
        }
    } else {
        let projection_bundle = session_canonical_thread_id(&session)
            .and_then(|thread_id| Uuid::parse_str(&thread_id).ok());
        let projection_bundle = match projection_bundle {
            Some(thread_id) => {
                match load_code_ui_projection_bundle(registry.working_dir(), thread_id).await {
                    Ok(bundle) => bundle,
                    Err(error) => {
                        tracing::warn!(%thread_id, error = %error, "failed to load projection-backed code ui snapshot; falling back to session state");
                        None
                    }
                }
            }
            None => None,
        };
        build_tui_code_ui_runtime(
            &working_dir_str,
            &session,
            &provider_name,
            &model_name,
            projection_bundle.as_ref(),
            code_control_tx,
            automation_write_enabled,
            browser_write_enabled,
            code_ui_test_lease_duration_override()?,
        )
        .await
    };
    let code_ui_session = code_ui_runtime.adapter().session();
    params
        .mcp_server
        .set_code_ui_session(code_ui_session.clone());
    let code_ui_runtime_for_app = code_ui_runtime.clone();

    let control_thread_id = session_canonical_thread_id(&session);
    let (mut web_handle, web_line) = match start_web_server(
        &params.host,
        params.port,
        registry.working_dir().to_path_buf(),
        WebServerOptions {
            code_ui: Some(code_ui_runtime),
            automation_control_token: control_runtime.token.clone(),
            audit_sink: None,
        },
    )
    .await
    {
        Ok(handle) => {
            let base_url = format!("http://{}", handle.addr);
            if let Err(error) = control_runtime.write_info_file(
                registry.working_dir(),
                base_url.clone(),
                None,
                control_thread_id.clone(),
            ) {
                handle.shutdown().await;
                if let Some(runtime) = managed_code_ui_runtime.as_ref() {
                    let _ = runtime.shutdown().await;
                }
                return Err(error);
            }
            let line = format!("Web: {base_url}");
            (Some(handle), line)
        }
        Err(err) if control_runtime.is_write() => {
            if let Some(runtime) = managed_code_ui_runtime.as_ref() {
                let _ = runtime.shutdown().await;
            }
            return Err(
                CliError::network(format!("failed to start web server: {err}"))
                    .with_detail("component", "web_server"),
            );
        }
        Err(err) => (
            None::<WebServerHandle>,
            format!("Web: failed to start ({err})"),
        ),
    };
    let control_base_url = web_handle
        .as_ref()
        .map(|handle| format!("http://{}", handle.addr));

    // Start MCP Server
    let (mcp_handle, mcp_line) =
        match start_mcp_server(&params.host, params.mcp_port, params.mcp_server.clone()).await {
            Ok(handle) => {
                let mcp_url = format!("http://{}", handle.addr);
                if let Some(base_url) = control_base_url.as_ref()
                    && let Err(error) = control_runtime.write_info_file(
                        registry.working_dir(),
                        base_url.clone(),
                        Some(mcp_url.clone()),
                        control_thread_id.clone(),
                    )
                {
                    if let Some(handle) = web_handle.take() {
                        handle.shutdown().await;
                    }
                    handle.shutdown().await;
                    if let Some(runtime) = managed_code_ui_runtime.as_ref() {
                        let _ = runtime.shutdown().await;
                    }
                    return Err(error);
                }
                let line = format!("MCP: {mcp_url}");
                (Some(handle), line)
            }
            Err(err) if control_runtime.is_write() => {
                if let Some(handle) = web_handle.take() {
                    handle.shutdown().await;
                }
                if let Some(runtime) = managed_code_ui_runtime.as_ref() {
                    let _ = runtime.shutdown().await;
                }
                return Err(
                    CliError::network(format!("failed to start MCP server: {err}"))
                        .with_detail("component", "mcp_server"),
                );
            }
            Err(err) => (None, format!("MCP: failed to start ({err})")),
        };

    let input_guidance = if managed_code_ui_runtime.is_some() {
        "Type your message and press Enter to work with the managed provider."
    } else {
        "Type a development request and press Enter to generate a reviewable plan before execution."
    };
    let welcome = format!("Welcome to Libra Code! {input_guidance}\n{web_line}\n{mcp_line}");

    // Load slash commands
    let commands = load_commands(registry.working_dir());
    let command_dispatcher = CommandDispatcher::new(commands);
    let skills = load_skills(registry.working_dir());
    let skill_dispatcher = SkillDispatcher::new(skills);

    // Load agent profiles
    let profiles = load_profiles(registry.working_dir());
    let agent_router = AgentProfileRouter::new(profiles);
    // OC-Phase 5 P5.1 session bootstrap (v0.17.775): read the
    // operator's `.libra/agents.toml` if present so
    // `code.sub_agents.enabled` / `code.multi_agent.enabled` /
    // `[code.budget]` / `[code.agents.*]` etc. actually take
    // effect. Missing file degrades to `AgentsConfig::default()`
    // (the previous hardcoded behavior) per `load_or_default`'s
    // contract. Parse errors are surfaced as a warning rather than
    // failing the session — a malformed config should not block an
    // operator from starting `libra code` to fix it.
    let agents_config_path = registry.working_dir().join(".libra").join("agents.toml");
    let agents_config = AgentsConfig::load_or_default(&agents_config_path).unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            path = %agents_config_path.display(),
            "failed to load agents.toml; falling back to AgentsConfig::default()",
        );
        AgentsConfig::default()
    });
    // v0.17.804 source_call_log persistence wire-up: build the
    // pool with the per-session SeaORM connection so every
    // SourcePool tool call lands a `source_call_log` row. Soft
    // fallback to `SourcePool::new()` (in-memory only) if the DB
    // path can't be resolved or the connection fails — same
    // posture as `build_usage_recorder` further down so session
    // bootstrap never blocks on a telemetry-layer issue.
    let source_pool = {
        let db_path = storage_root.join(DATABASE);
        let db_path_str = db_path.to_string_lossy();
        match establish_connection(&db_path_str).await {
            Ok(conn) => SourcePool::with_persistence(Arc::new(conn)),
            Err(err) => {
                tracing::warn!(
                    %err,
                    path = %db_path.display(),
                    "failed to open repo DB for SourcePool persistence; \
                     falling back to in-memory-only source call log",
                );
                SourcePool::new()
            }
        }
    };
    if let Err(error) = register_builtin_mcp_source_from_project_config(
        &source_pool,
        params.mcp_server.clone(),
        registry.working_dir(),
    ) {
        tracing::warn!("failed to register built-in MCP source: {error}");
    }
    config.source_pool = Some(source_pool.clone());
    config.source_session_id = Some(session.id.clone());

    // OC-Phase 3 P3.4 session bootstrap (v0.17.776): when the
    // operator's agents.toml flips `code.sub_agents.enabled =
    // true`, build the full `SubAgentToolLoopRuntime` so the
    // `task` tool actually routes through the dispatcher.
    //
    // Required parent context fields are sourced as:
    //   - dispatcher: DefaultSubAgentDispatcher::new(registry, cfg)
    //     .with_default_child_runner()
    //   - permission_service: a `DenyByDefaultPermissionAsker`
    //     fallback (interactive prompt wiring is a follow-up).
    //     `UserInitiated{bypass_permission_ask:true}` /task
    //     paths work; LlmInitiated paths that need escalation
    //     get rejected with an actionable feedback message.
    //   - parent_model_binding: ModelBinding from CLI flags.
    //   - parent_agent: minimal `AgentExecutionSpec` with the
    //     CLI-resolved model — enough for dispatcher gates
    //     (depth/concurrency/feature flag) which never reach
    //     into the parent_agent's tool/permission spec.
    //   - All other Arc'd state is sourced from values already
    //     constructed earlier in this function.
    //
    // Failure-to-build is logged and the runtime stays None —
    // `code.sub_agents.enabled = true` with a malformed agents
    // block degrades to the same "task tool not available" UX
    // an operator sees with the flag off.
    //
    // OC-Phase 4 P4.4 diagnostic (v0.17.783): if the operator
    // configured `[code.compaction]`, log the resolved model
    // binding so an operator can confirm the binding round-trip
    // works before the dispatcher-side integration lands. A
    // future commit consumes this binding in
    // `build_subagent_runtime_for_session` to route parent
    // frames through `run_compaction(...)` before feeding the
    // child via `ContextHandoff::to_handoff_messages`.
    if let Some(binding) = agents_config.compaction_model_binding() {
        tracing::info!(
            provider = %binding.provider_id,
            model = %binding.model_id,
            "compaction model binding resolved from [code.compaction]; \
             dispatcher integration is a v0.17.783+ follow-up",
        );
    }
    if agents_config.sub_agents.enabled {
        match build_subagent_runtime_for_session(
            &agents_config,
            registry.clone(),
            &session,
            &session_store,
            &storage_root,
            &model_name,
            &provider_name,
            &agent_router,
            config.hook_runner.clone(),
            // Hand the dispatcher the parent tool loop's resolved
            // runtime context (sandbox / approval / file-history)
            // so dispatched sub-agents inherit the parent's authority
            // rather than running unsandboxed (S2-INV-06).
            config.runtime_context.clone(),
        )
        .await
        {
            Ok(runtime) => {
                tracing::info!(
                    enabled = true,
                    max_depth = agents_config.multi_agent.max_subagent_depth,
                    // Log the EFFECTIVE concurrency (CEX-S2-12 caps it to
                    // 1), not the configured value, so the diagnostic
                    // matches what the dispatcher actually enforces.
                    max_concurrent = cex_s2_12_subagent_concurrency_cap(
                        agents_config.multi_agent.max_concurrent_subagents,
                    ),
                    "sub-agent dispatcher attached to tool_loop config",
                );
                config.subagent_runtime = Some(runtime);
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "failed to build SubAgentToolLoopRuntime; the `task` tool will surface \
                     'sub_agents.enabled = true required' until this is resolved",
                );
            }
        }
    }

    let managed_runtime_for_shutdown = managed_code_ui_runtime.clone();
    let auto_classify_first_user_message =
        params.auto_classify_first_user_message && managed_code_ui_runtime.is_none();

    // Create and run app
    let mut app = App::new(
        tui,
        model,
        registry,
        config,
        AppConfig {
            welcome_message: welcome,
            command_dispatcher,
            skill_dispatcher,
            agent_router,
            agents_config,
            session,
            session_store,
            user_input_rx: params.user_input_rx,
            exec_approval_rx: params.exec_approval_rx,
            model_name,
            provider_name,
            mcp_server: Some(params.mcp_server),
            code_ui_session: Some(code_ui_session),
            code_ui_runtime: Some(code_ui_runtime_for_app),
            code_control_rx,
            managed_code_ui_runtime,
            default_network_access: params.network_access,
            auto_classify_first_user_message,
            initial_goal: params.initial_goal.clone(),
            source_pool,
        },
    );

    let graph_thread_hint = match app.run().await {
        Ok(exit_info) => {
            if let ExitReason::Fatal(msg) = exit_info.reason {
                return Err(
                    CliError::fatal(msg).with_stable_code(StableErrorCode::InternalInvariant)
                );
            }
            exit_info.thread_id
        }
        Err(e) => return Err(CliError::internal(format!("TUI exited unexpectedly: {e}"))),
    };

    if let Some(handle) = web_handle {
        handle.shutdown().await;
    }
    if let Some(handle) = mcp_handle {
        handle.shutdown().await;
    }
    if let Some(runtime) = managed_runtime_for_shutdown {
        let _ = runtime.shutdown().await;
    }
    if let Some(thread_id) = graph_thread_hint {
        let current_dir = std::env::current_dir().ok();
        println!(
            "{}",
            format_graph_handoff_hint(&thread_id, &session_working_dir, current_dir.as_deref())
        );
    }

    Ok(())
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
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
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
) -> String {
    let intent = task_intent_for_context(context);
    let budget = ContextBudget::for_provider_model(
        context_budget_provider_name(provider),
        model.unwrap_or_else(|| default_context_budget_model(provider)),
    );
    let mut builder = SystemPromptBuilder::new(working_dir)
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

/// Constructs the default [`ToolRuntimeContext`] for TUI mode, configuring
/// the sandbox policy based on the operating context:
///
/// - **Dev mode (or no context)**: Workspace-write sandbox allowing modifications
///   within the working directory; network access follows the developer's
///   selected policy.
/// - **Review / Research mode**: Read-only sandbox; no writes or network access.
///
/// The approval policy and its communication channel are also wired in here.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DefaultTuiApprovalConfig {
    policy: AskForApproval,
    allow_all_commands: bool,
    ttl: Duration,
    cache_policy: ApprovalCachePolicy,
}

fn default_tui_runtime_context(
    working_dir: &std::path::Path,
    context: Option<CodeContext>,
    approval: DefaultTuiApprovalConfig,
    network_access: bool,
    exec_approval_tx: tokio::sync::mpsc::UnboundedSender<ExecApprovalRequest>,
) -> ToolRuntimeContext {
    let policy = match context {
        Some(CodeContext::Review | CodeContext::Research) => SandboxPolicy::ReadOnly,
        Some(CodeContext::Dev) | None => SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![working_dir.to_path_buf()],
            network_access: NetworkAccess::from_legacy_bool(network_access),
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        },
    };

    let mut approval_store = ApprovalStore::default();
    if approval.allow_all_commands {
        approval_store.approve_all_commands();
    }

    ToolRuntimeContext {
        sandbox: Some(ToolSandboxContext {
            policy,
            permissions: SandboxPermissions::UseDefault,
        }),
        sandbox_runtime: None,
        approval: Some(ToolApprovalContext {
            policy: approval.policy,
            request_tx: exec_approval_tx,
            store: Arc::new(tokio::sync::Mutex::new(approval_store)),
            scope_key_prefix: None,
            approval_ttl: approval.ttl,
            cache_policy: approval.cache_policy,
        }),
        file_history: None,
        max_output_bytes: None,
    }
}

#[derive(Debug, Deserialize)]
struct ApprovalProjectConfig {
    approval: Option<ApprovalSectionConfig>,
}

#[derive(Debug, Deserialize)]
struct ApprovalSectionConfig {
    ttl_seconds: Option<u64>,
    #[serde(default)]
    protected_branches: Option<Vec<String>>,
    #[serde(default)]
    allowed_network_domains: Option<Vec<String>>,
    #[serde(default)]
    no_cache_unknown_network: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ApprovalRuntimeConfig {
    ttl: Option<Duration>,
    cache_policy: ApprovalCachePolicy,
}

fn approval_config_from_project_config(working_dir: &Path) -> ApprovalRuntimeConfig {
    let path = working_dir.join(".libra").join("config.toml");
    let Some(contents) = fs::read_to_string(&path).ok() else {
        return ApprovalRuntimeConfig::default();
    };
    let Ok(config) = toml::from_str::<ApprovalProjectConfig>(&contents).map_err(|err| {
        tracing::warn!(
            target: "libra::command::code",
            path = %path.display(),
            error = %err,
            "failed to parse approval config"
        );
        err
    }) else {
        return ApprovalRuntimeConfig::default();
    };
    let Some(approval) = config.approval else {
        return ApprovalRuntimeConfig::default();
    };
    let ttl = approval.ttl_seconds.and_then(|ttl_seconds| {
        if ttl_seconds == 0 {
            tracing::warn!(
                target: "libra::command::code",
                path = %path.display(),
                "ignoring approval ttl_seconds=0"
            );
            None
        } else {
            Some(Duration::from_secs(ttl_seconds))
        }
    });

    let default_cache_policy = ApprovalCachePolicy::default();
    ApprovalRuntimeConfig {
        ttl,
        cache_policy: ApprovalCachePolicy {
            protected_branches: approval
                .protected_branches
                .unwrap_or(default_cache_policy.protected_branches),
            allowed_network_domains: approval.allowed_network_domains.unwrap_or_default(),
            no_cache_unknown_network: approval.no_cache_unknown_network,
            // OC-Phase 2 P2.5: the persistent ruleset is loaded lazily by
            // the runtime once it has a `DatabaseConnection`; the project-
            // config-derived policy starts with no projection attached.
            approved_ruleset: None,
        },
    }
}

/// Single source of truth for the approval-related CLI-args -> runtime
/// [`DefaultTuiApprovalConfig`] mapping (C7 criterion 2): `--approval-policy`
/// maps through `.into()`, `--approval-ttl` through `Duration::from_secs`
/// (CLI flag wins over the project `approval.ttl`, else `DEFAULT_APPROVAL_TTL`),
/// and `--approval-policy` also drives `allow_all_commands`. Both the TUI and
/// headless launch paths derive their approval config from here, so a dropped
/// or hardcoded flag is a single-point regression the unit test guards.
fn tui_approval_config_from_args(args: &CodeArgs, working_dir: &Path) -> DefaultTuiApprovalConfig {
    let approval_config = approval_config_from_project_config(working_dir);
    DefaultTuiApprovalConfig {
        policy: args.approval_policy.into(),
        allow_all_commands: args.approval_policy.allows_all_commands(),
        ttl: args
            .approval_ttl
            .map(Duration::from_secs)
            .or(approval_config.ttl)
            .unwrap_or(DEFAULT_APPROVAL_TTL),
        cache_policy: approval_config.cache_policy,
    }
}

#[cfg(test)]
fn approval_ttl_from_project_config(working_dir: &Path) -> Option<Duration> {
    approval_config_from_project_config(working_dir).ttl
}

#[cfg(test)]
fn approval_cache_policy_from_project_config(working_dir: &Path) -> ApprovalCachePolicy {
    approval_config_from_project_config(working_dir).cache_policy
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
    let storage_dir = resolve_storage_root(working_dir);
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

/// Resolves the `.libra/` storage root for the given working directory.
///
/// Supports linked worktrees by delegating to `try_get_storage_path`, which
/// follows `.libra` symlinks to the main repository's storage. Falls back to
/// `<working_dir>/.libra` if resolution fails.
pub(crate) fn resolve_storage_root(working_dir: &std::path::Path) -> std::path::PathBuf {
    try_get_storage_path(Some(working_dir.to_path_buf()))
        .unwrap_or_else(|_| working_dir.join(".libra"))
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
    // the follow-up replaces the auto-reject consumer with a real
    // TUI prompt widget that surfaces each ask interactively.
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
                 auto-rejecting until interactive TUI prompt widget lands",
            );
            // Send may fail if the dispatcher dropped its
            // oneshot receiver (e.g. cancelled mid-await). Ignore
            // the send error — the dispatcher already handles a
            // closed reply channel by surfacing Reject.
            let _ = ask.reply_tx.send(PermissionReply::Reject {
                feedback: Some(
                    "permission ask auto-rejected by the v0.17.788 channel consumer; \
                     pre-grant the permission via [code.agents.<name>.permission] in \
                     .libra/agents.toml or wait for the interactive TUI widget"
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

/// Validates CLI flag combinations across all three operating modes.
///
/// Enforces constraints such as:
/// - Web and MCP ports must differ (except in stdio mode).
/// - `--stdio` (MCP transport) rejects provider/model/api-base/temperature and
///   the provider-specific tuning flags — it has no provider surface.
/// - `--web`/`--web-only` relaxes provider/model/api-base/temperature and the
///   provider-specific tuning flags (they feed the headless web runtime) but
///   still rejects `--resume`, `--env-file`, `--network-access allow`,
///   `--context`, `--approval-policy`, and `--approval-ttl` (see
///   [`reject_non_tui_flags`]).
/// - Provider-specific flags are only accepted for their respective providers.
fn validate_mode_args(args: &CodeArgs, _output: &OutputConfig) -> Result<(), String> {
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

    if args.web_only {
        // web_only = true: relax provider/model/api-base/temperature and the
        // provider-specific tuning flags (they feed the headless web runtime and
        // still pass through the cross-provider match gate below).
        reject_non_tui_flags(args, "--web", true)?;
    }

    if args.stdio {
        if args.control == ControlMode::Write {
            return Err(
                "--control write is not supported with `libra code --stdio` because --stdio is the MCP stdio transport; use `libra code-control --stdio` for local TUI automation"
                    .to_string(),
            );
        }
        // web_only = false: --stdio is the MCP transport with no provider
        // surface, so it stays fully locked on provider/model/api-base and the
        // provider-specific flags.
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
    // the TUI-only reject that previously masked out-of-range values (codex C2
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

/// Rejects TUI-specific flags that are invalid in a non-TUI mode.
///
/// Two non-TUI modes reach this helper — `--web`/`--web-only` and `--stdio` —
/// and they receive DIFFERENT relaxations (plan.md Task C2). The `web_only`
/// argument selects which set applies; `--stdio` passes `web_only = false`.
///
/// * `--stdio` is the MCP stdio transport and has no provider / model / browser
///   surface, so it stays fully locked: `--provider != gemini`, `--model`,
///   `--api-base`, `--temperature`, and every provider-specific tuning flag are
///   rejected here.
/// * `--web`/`--web-only` drives the headless web runtime, which DOES consume
///   `--provider` (all seven providers plus the Codex branch), `--model`,
///   `--api-base`, `--temperature`, and the provider-specific tuning flags via
///   `build_any_completion_model_for_args` / the headless config factory. Under
///   web-only those are therefore NOT blanket-rejected here as "TUI-only"; they
///   flow through to the cross-provider match gate in `validate_mode_args`,
///   which still rejects a provider-specific flag that does not match the
///   selected provider and still rejects `--api-base` under `--provider=codex`.
///
/// Flags that stay rejected in BOTH non-TUI modes (design / safety / deferred
/// work): `--resume` (TUI-only by design — resume is accepted only on the TUI
/// path per `docs/development/tracing/code.md`; the session-layer headless
/// resume implementation is never wired to the CLI), `--env-file` (the
/// headless runtime still boots with `CodeEnvFile::default()`, so honoring a
/// user `--env-file` web-only needs additional plumbing — deferred),
/// `--network-access allow` (safety gate), plus `--context`,
/// `--approval-policy`, and `--approval-ttl`.
fn reject_non_tui_flags(args: &CodeArgs, mode: &str, web_only: bool) -> Result<(), String> {
    // Provider / model / api-base / temperature and the provider-specific tuning
    // flags feed the headless web runtime, so they are relaxed under web-only and
    // rejected only under stdio. Under web-only they still pass through the
    // cross-provider match gate and the Codex `--api-base` rejection in
    // `validate_mode_args` (invoked after this helper), so mismatched flags and
    // `--api-base` under Codex are still rejected there.
    if !web_only {
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
    }

    // Rejected in BOTH non-TUI modes.
    // NOTE (C2): web-only `--env-file` support is deferred. The headless runtime
    // currently boots with `CodeEnvFile::default()` (see
    // `build_non_codex_headless_runtime`), so honoring a user-supplied
    // `--env-file` web-only needs additional plumbing; keep it rejected until
    // that lands.
    reject_mode_flag(args.env_file.is_some(), "--env-file", mode)?;
    reject_mode_flag(args.context.is_some(), "--context", mode)?;
    // `--resume` is TUI-only by design (C5): although the session layer
    // (`load_or_create_headless_web_session_state`) carries a headless resume
    // implementation, resume is accepted only on the TUI path
    // (`docs/development/tracing/code.md` §"Session / graph"). This is a
    // deliberate contract, not deferred work — keep it rejected in every
    // non-TUI mode.
    reject_mode_flag(args.resume.is_some(), "--resume", mode)?;
    reject_mode_flag(
        args.approval_policy != CodeApprovalPolicy::OnRequest,
        "--approval-policy",
        mode,
    )?;
    reject_mode_flag(args.approval_ttl.is_some(), "--approval-ttl", mode)?;
    reject_mode_flag(
        args.network_access != CodeNetworkAccess::Deny,
        "--network-access",
        mode,
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    use axum::{Json, Router, routing::post};
    use serde_json::{Value, json};
    use tokio::{
        net::TcpListener,
        sync::{Mutex as AsyncMutex, mpsc::unbounded_channel},
    };

    use super::*;

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

    fn base_args() -> CodeArgs {
        CodeArgs {
            web_only: false,
            port: DEFAULT_WEB_PORT,
            host: DEFAULT_BIND_HOST.to_string(),
            cwd: None,
            repo: None,
            env_file: None,
            control: ControlMode::Observe,
            browser_control: None,
            control_token_file: None,
            control_info_file: None,
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
        let captured = Arc::new(AsyncMutex::new(Vec::new()));
        let app = Router::new().route(
            "/chat/completions",
            post({
                let captured = captured.clone();
                move |Json(body): Json<Value>| {
                    let captured = captured.clone();
                    async move {
                        captured.lock().await.push(body);
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
        (base_url, captured, handle)
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

    /// C5: `--resume` is TUI-only by design and stays rejected under
    /// `--web-only`. This is a deliberate contract (resume is accepted only on
    /// the TUI path, `docs/development/tracing/code.md`), not deferred work.
    /// `--model` used to be rejected here too, but C2 relaxed it web-only — see
    /// `accepts_model_api_base_and_temperature_in_web_only_mode`.
    #[test]
    fn rejects_tui_flags_in_web_mode() {
        let mut args = base_args();
        args.web_only = true;
        args.resume = Some("thread-id".to_string());
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--resume") && err.contains("--web") && err.contains("remove"),
            "web-only --resume rejection must name the flag, the mode, and an action; got: {err}"
        );
    }

    /// C5: `--resume` is also rejected under `--stdio` (the MCP transport has
    /// no session/resume surface). Pin the actionable message shape — name the
    /// flag, the mode, and a corrective action — so the TUI-only contract has a
    /// regression guard on both non-TUI modes.
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

    /// C5: the post-exit graph handoff prints a bare `libra graph <thread_id>`
    /// when the session ran against the current directory (graph discovers the
    /// repo from cwd), so no `--repo` suffix is needed.
    #[test]
    fn graph_handoff_hint_omits_repo_for_current_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let thread_id = "11111111-1111-4111-8111-111111111111";
        let hint = format_graph_handoff_hint(thread_id, dir.path(), Some(dir.path()));
        assert_eq!(
            hint,
            format!("Inspect this thread graph with: libra graph {thread_id}")
        );
        assert!(
            !hint.contains("--repo"),
            "same-dir hint must not add --repo"
        );
    }

    /// C5: when the code session ran against a repository other than the
    /// current directory (`--repo`/`--cwd`), the handoff appends
    /// `--repo <path>` so `libra graph` resolves the same repository — matching
    /// the remote-repo guidance in `docs/commands/code.md`.
    #[test]
    fn graph_handoff_hint_appends_repo_for_remote_repository() {
        let session_dir = tempfile::tempdir().expect("session tempdir");
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let thread_id = "11111111-1111-4111-8111-111111111111";
        let hint = format_graph_handoff_hint(thread_id, session_dir.path(), Some(cwd.path()));
        assert_eq!(
            hint,
            format!(
                "Inspect this thread graph with: libra graph {thread_id} --repo {}",
                session_dir.path().display()
            )
        );
    }

    /// C5 (codex review): the copy-pasteable `--repo` hint must survive paths
    /// with whitespace/shell-special characters — clean paths stay bare, dirty
    /// ones are POSIX single-quoted so the emitted command word-splits correctly.
    #[test]
    fn shell_quote_for_display_quotes_only_when_needed() {
        assert_eq!(
            shell_quote_for_display("/home/user/repo"),
            "/home/user/repo"
        );
        assert_eq!(
            shell_quote_for_display("/Volumes/Data/My Repo"),
            "'/Volumes/Data/My Repo'"
        );
        // Embedded single quote is escaped via the '\'' idiom.
        assert_eq!(
            shell_quote_for_display("/tmp/o'brien"),
            "'/tmp/o'\\''brien'"
        );
        // Shell metacharacters force quoting even without whitespace.
        assert_eq!(shell_quote_for_display("/tmp/a$b"), "'/tmp/a$b'");
        assert_eq!(shell_quote_for_display(""), "''");
    }

    /// C5 (codex review): a session dir with a space produces a quoted
    /// `--repo` argument in the handoff hint.
    #[test]
    fn graph_handoff_hint_quotes_repo_path_with_spaces() {
        let base = tempfile::tempdir().expect("base tempdir");
        let session_dir = base.path().join("My Repo");
        std::fs::create_dir_all(&session_dir).expect("create spaced dir");
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let thread_id = "22222222-2222-4222-8222-222222222222";
        let hint = format_graph_handoff_hint(thread_id, &session_dir, Some(cwd.path()));
        assert!(
            hint.ends_with(&format!(
                "--repo {}",
                shell_quote_for_display(&session_dir.display().to_string())
            )),
            "spaced repo path must be quoted in the hint: {hint}"
        );
        assert!(
            hint.contains('\''),
            "quoted path must contain a quote: {hint}"
        );
    }

    /// C5: if the process working directory can't be resolved, fail safe by
    /// emitting the explicit `--repo <path>` form rather than a bare hint that
    /// might discover the wrong repository.
    #[test]
    fn graph_handoff_hint_appends_repo_when_current_dir_unknown() {
        let session_dir = tempfile::tempdir().expect("session tempdir");
        let thread_id = "11111111-1111-4111-8111-111111111111";
        let hint = format_graph_handoff_hint(thread_id, session_dir.path(), None);
        assert!(
            hint.contains("--repo") && hint.contains(&session_dir.path().display().to_string()),
            "unknown cwd must fall back to explicit --repo; got: {hint}"
        );
    }

    #[test]
    fn rejects_web_flags_in_stdio_mode() {
        let mut args = base_args();
        args.stdio = true;
        args.host = "0.0.0.0".to_string();
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_err());
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
            args.web_only = true;
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
        args.web_only = true;
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
        args.web_only = true;
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
        args.web_only = true;
        args.provider = CodeProvider::Deepseek;
        args.deepseek_thinking = Some(DeepSeekThinkingArg::Enabled);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_matching_kimi_flag_in_web_only_mode() {
        let mut args = base_args();
        args.web_only = true;
        args.provider = CodeProvider::Kimi;
        args.kimi_thinking = Some(KimiThinkingArg::Enabled);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    /// C2 (P1, codex review): `--temperature` reaches the headless runtime after
    /// the web-only relaxation, so its 0.0–2.0 contract is enforced
    /// mode-independently. Out-of-range and non-finite values are rejected.
    #[test]
    fn rejects_out_of_range_temperature() {
        for (mode_web_only, bad) in [
            (true, 2.5_f64),
            (true, -0.1),
            (true, f64::NAN),
            (false, 3.0),
        ] {
            let mut args = base_args();
            args.web_only = mode_web_only;
            args.provider = CodeProvider::Ollama;
            args.temperature = Some(bad);
            let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
            assert!(
                err.contains("--temperature"),
                "temperature {bad} (web_only={mode_web_only}) must be rejected; got: {err}"
            );
        }
        // Boundary values are accepted.
        for good in [0.0_f64, 2.0, 1.0] {
            let mut args = base_args();
            args.web_only = true;
            args.provider = CodeProvider::Ollama;
            args.temperature = Some(good);
            assert!(
                validate_mode_args(&args, &OutputConfig::default()).is_ok(),
                "temperature {good} must be accepted"
            );
        }
    }

    /// C2 (R4): relaxing the web-only "TUI-only" blanket must NOT weaken the
    /// cross-provider match gate — a provider-specific flag that does not match
    /// the selected provider is still rejected under web-only.
    #[test]
    fn rejects_mismatched_provider_flag_in_web_only_mode() {
        let mut args = base_args();
        args.web_only = true;
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
        args.web_only = true;
        args.provider = CodeProvider::Codex;
        args.api_base = Some("http://127.0.0.1:8080".to_string());
        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(
            err.contains("--api-base") && err.contains("codex"),
            "web-only --api-base under Codex must still be rejected; got: {err}"
        );
    }

    /// C2 (R3): `--env-file` and `--network-access allow` stay rejected under
    /// web-only (env-file support deferred; network-access is a safety gate).
    #[test]
    fn rejects_deferred_and_safety_flags_in_web_only_mode() {
        let mut env_file_args = base_args();
        env_file_args.web_only = true;
        env_file_args.env_file = Some(PathBuf::from(".env.test"));
        let err = validate_mode_args(&env_file_args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--env-file") && err.contains("--web"));

        let mut net_args = base_args();
        net_args.web_only = true;
        net_args.network_access = CodeNetworkAccess::Allow;
        let err = validate_mode_args(&net_args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--network-access") && err.contains("--web"));
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
    fn accepts_default_tui_mode() {
        let args = base_args();
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_control_write_in_default_tui_mode() {
        let mut args = base_args();
        args.control = ControlMode::Write;

        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_control_write_in_default_web_mode() {
        let args = CodeArgs::try_parse_from(["libra", "--web", "--control", "write"]).unwrap();

        assert!(args.web_only);
        assert_eq!(args.control, ControlMode::Write);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn browser_control_resolution_matrix_pins_mode_provider_and_host_contract() {
        #[derive(Copy, Clone)]
        struct BrowserControlCase {
            name: &'static str,
            web_only: bool,
            provider: CodeProvider,
            explicit: Option<BrowserControlMode>,
            host: &'static str,
            expected: Result<BrowserControlMode, &'static str>,
        }

        let cases = [
            BrowserControlCase {
                name: "tui default stays off even on non-loopback host",
                web_only: false,
                provider: CodeProvider::Gemini,
                explicit: None,
                host: "0.0.0.0",
                expected: Ok(BrowserControlMode::Off),
            },
            BrowserControlCase {
                name: "tui explicit off allows non-loopback host",
                web_only: false,
                provider: CodeProvider::Gemini,
                explicit: Some(BrowserControlMode::Off),
                host: "0.0.0.0",
                expected: Ok(BrowserControlMode::Off),
            },
            BrowserControlCase {
                name: "tui explicit loopback allows loopback host",
                web_only: false,
                provider: CodeProvider::Gemini,
                explicit: Some(BrowserControlMode::Loopback),
                host: "127.0.0.1",
                expected: Ok(BrowserControlMode::Loopback),
            },
            BrowserControlCase {
                name: "tui explicit loopback rejects non-loopback host",
                web_only: false,
                provider: CodeProvider::Gemini,
                explicit: Some(BrowserControlMode::Loopback),
                host: "0.0.0.0",
                expected: Err("loopback"),
            },
            BrowserControlCase {
                name: "non-codex web-only default stays off on non-loopback host",
                web_only: true,
                provider: CodeProvider::Ollama,
                explicit: None,
                host: "0.0.0.0",
                expected: Ok(BrowserControlMode::Off),
            },
            BrowserControlCase {
                name: "non-codex web-only explicit loopback rejects non-loopback host",
                web_only: true,
                provider: CodeProvider::Ollama,
                explicit: Some(BrowserControlMode::Loopback),
                host: "0.0.0.0",
                expected: Err("loopback"),
            },
            BrowserControlCase {
                name: "codex web-only defaults to loopback on loopback host",
                web_only: true,
                provider: CodeProvider::Codex,
                explicit: None,
                host: "localhost",
                expected: Ok(BrowserControlMode::Loopback),
            },
            BrowserControlCase {
                name: "codex web-only default loopback rejects non-loopback host",
                web_only: true,
                provider: CodeProvider::Codex,
                explicit: None,
                host: "0.0.0.0",
                expected: Err("loopback"),
            },
            BrowserControlCase {
                name: "codex web-only explicit off allows non-loopback host",
                web_only: true,
                provider: CodeProvider::Codex,
                explicit: Some(BrowserControlMode::Off),
                host: "0.0.0.0",
                expected: Ok(BrowserControlMode::Off),
            },
            BrowserControlCase {
                name: "codex web-only explicit loopback allows ipv6 loopback host",
                web_only: true,
                provider: CodeProvider::Codex,
                explicit: Some(BrowserControlMode::Loopback),
                host: "::1",
                expected: Ok(BrowserControlMode::Loopback),
            },
        ];

        for case in cases {
            let mut args = base_args();
            args.web_only = case.web_only;
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
        assert!(err.contains("code-control --stdio"));
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
    fn accepts_env_file_cli_arg_in_tui_mode() {
        let args = CodeArgs::try_parse_from(["libra", "--env-file", ".env.test"]).unwrap();

        assert_eq!(args.env_file.as_deref(), Some(Path::new(".env.test")));
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn rejects_env_file_in_web_mode() {
        let mut args = base_args();
        args.web_only = true;
        args.env_file = Some(PathBuf::from(".env.test"));

        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--env-file"));
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
    fn provider_env_file_value_overrides_process_lookup() {
        let env_file =
            parse_code_env_file("DEEPSEEK_API_KEY=file-key", Path::new(".env.test")).unwrap();

        let value = provider_env_value_with_lookup(&env_file, "DEEPSEEK_API_KEY", |_| {
            Some("old-key".into())
        });

        assert_eq!(value.as_deref(), Some("file-key"));
    }

    #[test]
    fn accepts_network_access_cli_arg_in_tui_mode() {
        let args = CodeArgs::try_parse_from(["libra", "--network-access", "allow"]).unwrap();

        assert_eq!(args.network_access, CodeNetworkAccess::Allow);
        assert!(validate_mode_args(&args, &OutputConfig::default()).is_ok());
    }

    #[test]
    fn accepts_allow_all_approval_policy_in_tui_mode() {
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
    fn accepts_approval_ttl_cli_arg_in_tui_mode() {
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
        args.web_only = true;
        args.network_access = CodeNetworkAccess::Allow;

        let err = validate_mode_args(&args, &OutputConfig::default()).unwrap_err();
        assert!(err.contains("--network-access"));
    }

    #[test]
    fn accepts_anthropic_provider_in_tui_mode() {
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

    #[tokio::test]
    async fn tui_code_ui_runtime_prefers_projection_bundle_identity() {
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

        let runtime = build_tui_code_ui_runtime(
            "/tmp/workspace",
            &session,
            "ollama",
            "gemma4:31b",
            Some(&bundle),
            None,
            false,
            false,
            None,
        )
        .await;
        let snapshot = runtime.snapshot().await;

        assert_eq!(snapshot.session_id, thread_id.to_string());
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
        );

        assert!(prompt.contains("Code Review Mode"));
        assert!(prompt.contains("## Task Intent"));
        assert!(prompt.contains("intent=review"));
        assert!(prompt.contains("## Dynamic Workspace Context"));
        assert!(prompt.contains("source=libra status --short"));
        assert!(prompt.contains("## Context Budget Plan"));
    }

    #[test]
    fn default_tui_runtime_context_denies_network_in_dev_mode() {
        let (tx, _rx) = unbounded_channel();
        let runtime = default_tui_runtime_context(
            Path::new("/tmp/workspace"),
            Some(CodeContext::Dev),
            DefaultTuiApprovalConfig {
                policy: AskForApproval::OnRequest,
                allow_all_commands: false,
                ttl: DEFAULT_APPROVAL_TTL,
                cache_policy: ApprovalCachePolicy::default(),
            },
            false,
            tx,
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
    fn default_tui_runtime_context_allows_network_when_requested_in_dev_mode() {
        let (tx, _rx) = unbounded_channel();
        let runtime = default_tui_runtime_context(
            Path::new("/tmp/workspace"),
            Some(CodeContext::Dev),
            DefaultTuiApprovalConfig {
                policy: AskForApproval::OnRequest,
                allow_all_commands: false,
                ttl: DEFAULT_APPROVAL_TTL,
                cache_policy: ApprovalCachePolicy::default(),
            },
            true,
            tx,
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
    async fn default_tui_runtime_context_can_allow_all_commands() {
        let (tx, _rx) = unbounded_channel();
        let runtime = default_tui_runtime_context(
            Path::new("/tmp/workspace"),
            Some(CodeContext::Dev),
            DefaultTuiApprovalConfig {
                policy: AskForApproval::OnRequest,
                allow_all_commands: true,
                ttl: DEFAULT_APPROVAL_TTL,
                cache_policy: ApprovalCachePolicy::default(),
            },
            true,
            tx,
        );

        let approval = runtime
            .approval
            .expect("approval context should be present");
        assert!(approval.store.lock().await.allow_all_commands());
    }

    #[test]
    fn default_tui_runtime_context_is_read_only_for_review_and_research() {
        for context in [CodeContext::Review, CodeContext::Research] {
            let (tx, _rx) = unbounded_channel();
            let runtime = default_tui_runtime_context(
                Path::new("/tmp/workspace"),
                Some(context),
                DefaultTuiApprovalConfig {
                    policy: AskForApproval::OnRequest,
                    allow_all_commands: false,
                    ttl: DEFAULT_APPROVAL_TTL,
                    cache_policy: ApprovalCachePolicy::default(),
                },
                true,
                tx,
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
    fn default_tui_runtime_context_exposes_approval_policy_and_ttl() {
        // Exercise the PRODUCTION mapping (codex C7 review): the args ->
        // DefaultTuiApprovalConfig mapping is now the shared helper
        // `tui_approval_config_from_args`, which both the TUI and headless
        // launch paths call. Feeding it parsed CLI args and running the result
        // through `default_tui_runtime_context` catches a regression where a
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
        let runtime = default_tui_runtime_context(
            workspace.path(),
            Some(CodeContext::Dev),
            tui_approval_config_from_args(&args, workspace.path()),
            args.network_access.is_allowed(),
            tx,
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
        let default_cfg = tui_approval_config_from_args(&default_args, workspace.path());
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
    /// The TUI path inlines its registry construction inside
    /// `execute_tui` and is not testable in isolation; the unit-level
    /// guard at
    /// `internal::ai::tools::registry::tests::registry_does_not_expose_task_tool_in_flag_off_default`
    /// covers the fixture-level invariant for that path.
    #[test]
    fn build_headless_tool_registry_omits_task_tool_in_flag_off_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = build_headless_tool_registry(tmp.path(), tx);
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
    /// tools as TUI without bypassing sandbox, approval, or `--network-access
    /// deny`.
    #[test]
    fn build_headless_tool_registry_exposes_runtime_guarded_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = build_headless_tool_registry(tmp.path(), tx);
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

    /// Scenario: an agent binding whose `provider_id` does NOT match any
    /// `CodeProvider` variant must be rejected at
    /// `effective_code_provider_for_args` with a clear, actionable error.
    /// Silent fallback to `args.provider` would leave system prompt and
    /// context-budget computations pointed at the CLI provider while the
    /// model itself was built (or refused) for a different provider —
    /// a partial-misconfiguration trap. Pinning this gate prevents the
    /// regression Codex flagged on the OC-Phase 2 P2.4 review.
    #[test]
    fn effective_provider_rejects_unknown_binding_provider_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_agent_profile(
            tmp.path(),
            "alien",
            "---\n\
             name: alien\n\
             model: aleph-omega/some-model\n\
             ---\n\
             body",
        );
        let mut args = base_args();
        args.agent = Some("alien".to_string());

        let err = effective_code_provider_for_args(&args, tmp.path())
            .expect_err("unknown binding provider must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("alien"),
            "error must mention the agent name: {msg}"
        );
        assert!(
            msg.contains("aleph-omega"),
            "error must echo the offending provider id: {msg}"
        );
        assert!(
            msg.contains("anthropic"),
            "error must list the known provider ids: {msg}"
        );
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
            session_store,
            session_state,
            false,
        )
        .await
        .expect("headless Ollama should build through ProviderFactory")
        .expect("Ollama is the supported non-Codex headless provider");
        let snapshot = runtime.snapshot().await;

        assert_eq!(snapshot.provider.provider, "ollama");
        assert_eq!(snapshot.provider.mode.as_deref(), Some("web-headless"));
        assert_eq!(snapshot.provider.model.as_deref(), Some("llama3.2"));
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
            session_store,
            session_state,
            false,
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
            session_store,
            session_state,
            false,
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
