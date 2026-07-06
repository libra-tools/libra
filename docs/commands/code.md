# `libra code`

Launch an interactive AI coding session with TUI, web, or MCP modes.

## Synopsis

```
libra code
libra code --web-only [-p <PORT>] [--host <HOST>]
libra code --stdio
libra code --provider <PROVIDER> [--model <MODEL>]
libra code --resume <THREAD_ID>
libra graph <THREAD_ID> [--repo <PATH>]
```

## Description

`libra code` starts an interactive coding session that pairs a human developer with an AI agent. The default mode launches a terminal UI (TUI) built on ratatui/crossterm with a background web server. Plain developer requests in the generic provider TUI are routed through the built-in planning workflow first: Libra generates a reviewable IntentSpec and execution plan, then waits for Execute Plan / Network / Modify Plan / Cancel before running mutating tools. Two alternative modes are available: `--web-only` runs the web server without the TUI (useful for browser access or remote hosting), and `--stdio` runs an MCP server over standard input/output for integration with AI clients like Claude Desktop.

The command supports eight AI provider backends (Gemini, OpenAI, Anthropic, DeepSeek, Kimi, Zhipu, Ollama, Codex) and three operating contexts (dev, review, research) that tune the agent's behavior for different workflows. Sessions can be persisted and resumed with Libra's canonical `--resume <thread_id>` flow. Passing `--goal "<objective>"` boots the session directly in goal mode, where a supervisor drives the tool loop toward the stated objective until a verifier accepts completion.

A sandboxed tool-execution layer enforces approval policies that control when the agent can run shell commands, apply patches, web search, or perform other potentially destructive operations. TUI dev sessions default to workspace-write execution with network access denied. After the execution plan is ready, the Plan review dialog includes a `Network: Deny` / `Network: Allow` toggle; the selected value becomes the execution `IntentSpec` network policy for shell, gate, and `web_search` use. Review and research contexts remain read-only and do not grant network access.

When the TUI exits and Libra can derive the canonical thread ID, `libra code` prints a follow-up `libra graph <thread_id>` command so the thread's Intent/Plan/Task/Run/PatchSet version graph can be inspected in a separate TUI. Use `libra graph <thread_id> --repo <path>` when inspecting a repository other than the current directory.

## Options

| Flag | Short | Long | Default | Description |
|------|-------|------|---------|-------------|
| Web only | | `--web-only` / `--web` | off | Run the web server without TUI. Conflicts with `--stdio`. |
| Port | `-p` | `--port` | `3000` | Web server listen port. |
| Host | | `--host` | `127.0.0.1` | Web server bind address. |
| Working directory | | `--cwd` | current dir | Working directory for the session. |
| Env file | | `--env-file <PATH>` | none | Load provider environment variables from a dotenv-style file; explicit file values take precedence over Vault and the process environment. |
| Control mode | | `--control <observe\|write>` | `observe` | Local automation control mode. `observe` preserves existing loopback read behavior; `write` enables local token discovery and process-level automation control auth. |
| Control token file | | `--control-token-file <PATH>` | `.libra/code/control-token` | Path for the per-process local automation token. In `write` mode, Unix/macOS files must be regular files with `0600` permissions. |
| Control info file | | `--control-info-file <PATH>` | `.libra/code/control.json` | Path for non-secret local endpoint discovery metadata. The file never contains token material. |
| Provider | | `--provider` | `gemini` | AI provider backend (see Provider Backends below). |
| Model | | `--model` | provider default | Provider-specific model ID. |
| Agent profile | | `--agent <NAME>` | none | Select an agent profile by name. When the profile carries a structured `model: provider/model[@variant]` binding, that binding wins atomically -- provider, model ID, and variant all come from the profile, and a separately supplied `--model` is ignored to avoid hybrid pairs; profiles without a structured binding fall back to the CLI defaults. Profiles resolve through the three-tier hierarchy (project `.libra/agents/`, user `~/.config/libra/agents/`, embedded). Unknown or non-primary-eligible profiles are rejected. |
| Temperature | | `--temperature` | provider default | Sampling temperature for generation. |
| Ollama thinking | | `--ollama-thinking` / `--thinking` | `OLLAMA_THINK`, then `off` | Ollama thinking mode: `auto`, `off`, `on`, `low`, `medium`, or `high`. |
| Ollama compact tools | | `--ollama-compact-tools` | `OLLAMA_COMPACT_TOOLS`, then off | Sends compact tool schemas for remote/cloud Ollama endpoints that reject complex JSON schemas. |
| DeepSeek thinking | | `--deepseek-thinking <enabled\|disabled>` | omitted | Sends DeepSeek's `thinking` object when using `--provider deepseek`. |
| DeepSeek reasoning effort | | `--deepseek-reasoning-effort <low\|medium\|high\|max>` | omitted | Sends DeepSeek's `reasoning_effort` value when using `--provider deepseek`; `xhigh` is accepted as an alias for `max`. |
| DeepSeek stream | | `--deepseek-stream <true\|false>` / `--stream <true\|false>` | `false` | Sends DeepSeek's `stream` boolean when using `--provider deepseek`. |
| Kimi thinking | | `--kimi-thinking <enabled\|disabled>` | model default | Sends Kimi's `thinking` object when using `--provider kimi`. |
| Kimi stream | | `--kimi-stream <true\|false>` | `true` (Kimi) | Sends Kimi's `stream` boolean when using `--provider kimi`; defaults to streaming. Rejected for non-Kimi providers. |
| Context | | `--context` | none | Operating context: `dev` (alias `development`), `review` (alias `code-review`), `research` (alias `explore`). |
| Resume | | `--resume <THREAD_ID>` | none | Resume a canonical Libra thread by thread ID. |
| Approval policy | | `--approval-policy` | `on-request` | Tool approval policy (see Approval Policies below). |
| Approval TTL | | `--approval-ttl <SECS>` | `300` | Seconds that a granted approval stays reusable for matching commands before the agent is prompted again. Overrides the project config `[approval] ttl_seconds` in `.libra/config.toml`; relevant to the prompting policies. |
| Network access | | `--network-access <allow\|deny>` | `deny` | Default TUI network policy for shell and gate execution; can still be toggled at Plan review. |
| MCP port | | `--mcp-port` | `6789` | MCP server listen port. |
| Stdio | | `--stdio` / `--mcp-stdio` | off | Run MCP over stdio. Conflicts with `--web-only`. |
| API base | | `--api-base` | provider default | Provider API base URL override. |
| Codex binary | | `--codex-bin` | `codex` | Codex executable path. |
| Codex port | | `--codex-port` | random | Override Codex app-server port. |
| Plan mode | | `--plan-mode [<true\|false>]` | off (on for Codex) | Require the agent to produce an approved plan before execution. The effective default is on for `--provider codex` and off for every other provider; Explicit `--plan-mode=true` (or bare `--plan-mode`) is only accepted with `--provider=codex` — it is rejected for other providers; pass `--plan-mode=false` to opt a Codex session out. |
| Browser control | | `--browser-control <off\|loopback>` | provider-aware (see Web Browser Control) | Posture for `/api/code/controller/attach` browser leases. Conflicts with `--stdio`; `loopback` requires a loopback `--host`. |
| Goal | | `--goal <OBJECTIVE>` | none | Boot the session in goal mode with the supplied objective, equivalent to running `/goal start <objective>` as the session opens; the supervisor drives the tool loop until completion is claimed and the verifier accepts. The objective is validated at parse time (non-empty after trim, at most 16 KiB). |

### Provider Backends

| Value | Description | API Key Env | Base URL Override |
|-------|-------------|-------------|-------------------|
| `gemini` | Google Gemini (default: gemini-2.5-flash) | `GEMINI_API_KEY` | `--api-base` |
| `openai` | OpenAI (default: gpt-4o-mini) | `OPENAI_API_KEY` | `--api-base`, `OPENAI_BASE_URL` |
| `anthropic` | Anthropic (default: claude-3.5-sonnet) | `ANTHROPIC_API_KEY` | `--api-base`, `ANTHROPIC_BASE_URL` |
| `deepseek` | DeepSeek | `DEEPSEEK_API_KEY` | `--api-base` |
| `kimi` | Moonshot AI Kimi (default: kimi-k2.6) | `MOONSHOT_API_KEY` | `--api-base`, `MOONSHOT_BASE_URL`, `--kimi-thinking` |
| `zhipu` | Zhipu GLM (default: glm-5) | `ZHIPU_API_KEY` | `--api-base`, `ZHIPU_BASE_URL` |
| `ollama` | Ollama (local models and direct Cloud API) | `OLLAMA_API_KEY` for direct Cloud API | `OLLAMA_BASE_URL`, `OLLAMA_THINK`, `OLLAMA_COMPACT_TOOLS`, `--api-base`, `--ollama-thinking`, or `--ollama-compact-tools` |
| `codex` | Codex app-server | -- | `--codex-bin` / `--codex-port` |

For Codex app-server linkage, model forwarding, credentials ownership, and persisted object storage details, see [Codex data storage integration](codex-data-storage.md).

DeepSeek requests can opt into provider-specific fields with `--deepseek-thinking enabled --deepseek-reasoning-effort high --deepseek-stream true`; these flags are rejected for non-DeepSeek providers.
Kimi requests default to the selected model's thinking behavior; use `--kimi-thinking disabled` for K2.6/K2.5 runs where lower latency or official web-search compatibility matters. Libra preserves Kimi `reasoning_content` across tool-call turns when the provider returns it.
For normal runs, store provider keys in `vault.env.<NAME>`; Libra checks repo-local Vault, then global Vault, then the process environment. Use `--env-file .env.test` for live tests that need an explicit dotenv override.

Ollama requests stream `/api/chat` responses by default and add a per-request `request_id` to debug logs. They also default to `think:false` so reasoning-capable local models do not spend several minutes generating hidden reasoning before tool calls. Use `--ollama-thinking high` for a single run, or set `OLLAMA_THINK=true`, `low`, `medium`, `high`, or `auto` as the environment default. `auto` omits the `think` field and lets Ollama decide. Use `--ollama-compact-tools` or `OLLAMA_COMPACT_TOOLS=true` when a remote/cloud Ollama endpoint accepts simple tools but returns 503 for Libra's full tool schema payload.

### Local Automation Control

`libra code --control observe` is the default and does not create local control files unless `--control-info-file` is explicitly supplied. Loopback clients can continue reading `/api/code/session` and `/api/code/events` without a token.

`libra code --control write` enables the local automation security envelope. Libra creates a fresh 32-byte token in `.libra/code/control-token`, writes non-secret endpoint metadata to `.libra/code/control.json` after the web server binds, and holds `.libra/code/control.lock` for the process lifetime. `control.json` includes `version`, `mode`, `pid`, `baseUrl`, optional `mcpUrl`, `workingDir`, optional `threadId`, and `startedAt`; it never includes the token, token hash, token path, provider credentials, headers, or provider request/response bodies.

Write control is local-only. `--control write` is rejected with `--stdio`, and it requires `--host` to be loopback (`127.0.0.1`, `::1`, or `localhost`). A second write-control instance using the same default paths fails fast with `CONTROL_INSTANCE_CONFLICT`; use distinct `--control-token-file` and `--control-info-file` paths only when the caller intentionally manages multiple local instances.

Automation clients attach with `POST /api/code/controller/attach`, body `{ "clientId": "...", "kind": "automation" }`, header `X-Libra-Control-Token`, and then use the returned `X-Code-Controller-Token` for writes. Automation-held leases require both tokens for `/api/code/messages`, `/api/code/interactions/{id}`, `/api/code/controller/detach`, and `/api/code/control/cancel`. The local TUI can reclaim control with `/control reclaim`, which invalidates the automation lease. Code UI write request bodies are capped at 256KiB.

`GET /api/code/diagnostics` returns a redacted observe-only status summary for local tools. Control attach, detach, submit, respond, and cancel operations emit `local-tui-control/v1` audit events through the runtime audit sink. For stdio automation clients, use [`libra code-control --stdio`](code-control.md); `libra code --stdio` remains the MCP stdio server and does not control a live TUI.

### Web Browser Control

`--browser-control <off|loopback>` controls whether the embedded UI's lease-based write surface is available. The default is mode-aware:

| Entry point | Default `--browser-control` |
|-------------|-----------------------------|
| TUI session (`libra code` without `--web-only`) | `off` |
| `libra code --web-only --provider codex` | `loopback` |
| `libra code --web-only` with any other provider | `off` |

Selecting `loopback` is rejected when `--host` is not a loopback address, and the flag conflicts with `--stdio`. The browser server-side endpoints are tagged in the `code_router()` audit matrix (`src/internal/ai/web/mod.rs`):

- `GET /api/code/session`, `GET /api/code/events`, `GET /api/code/diagnostics`, `GET /api/code/threads`, `GET /api/code/goal/status` — loopback-only observe.
- `POST /api/code/controller/attach` — loopback. `kind: "automation"` requests additionally require `X-Libra-Control-Token`. The handler **issues** the lease's `controllerToken` (it does not expect the caller to send one).
- `POST /api/code/controller/detach`, `POST /api/code/messages`, `POST /api/code/interactions/{id}` — loopback + `X-Code-Controller-Token`; `Automation` leases additionally require `X-Libra-Control-Token`.
- `POST /api/code/control/cancel` — loopback + `X-Code-Controller-Token`. `Automation` leases also require `X-Libra-Control-Token`; this is the only difference from the TUI `Esc` cancel path.
- `POST /api/code/task/dispatch` — loopback + `X-Code-Controller-Token`; user-initiated sub-agent dispatch requires an automation lease.
- `POST /api/code/goal/start`, `POST /api/code/goal/cancel` — loopback + `X-Code-Controller-Token`; goal mutation requires the active controller lease.

Browser write requests share the same 256 KiB body limit and audit-sink wiring as automation control. The browser persists the lease only in memory; reloading the page drops the lease and the next write reattaches.

When the server is bound to a non-loopback host, non-loopback browsers receive a static remote access notice for HTML navigation instead of the SPA. The notice is zero JavaScript, includes only bind/remote/version/commit placeholders, and asset/API fallbacks return 404 so remote clients cannot probe session state.

When `--browser-control loopback` is requested and the browser holds the active lease, the TUI initial controller is `LocalTui` (visible owner, can be reclaimed) instead of `Fixed { Tui }` (permanently blocking). If the TUI also wants to drive writes, `--control write` must be supplied alongside `--browser-control loopback`; the two writers serialize through the same `TuiControlCommand` channel.

For `--web-only` non-Codex providers (`--provider ollama` is the canonical Phase 3 verification path), Libra builds a [`HeadlessCodeRuntime`](../../src/internal/ai/web/headless.rs) that runs the agent's tool loop directly so the browser can drive a real session -- no terminal required. Headless mode advertises `messageInput`, `streamingText`, `toolCalls`, `planUpdates`, `patchsets`, `interactiveApprovals`, `structuredQuestions`, and `providerSessionResume`. `--resume <thread_id>` is TUI-only by design: the headless runtime carries a session-layer resume implementation, but the `--resume` CLI flag is accepted only on the interactive TUI path and is rejected in `--web-only` (and `--stdio`) mode. Persisted headless resume is reachable only through the TUI — this is a deliberate contract, not deferred work. `update_plan` projects into `plans[]`, and `apply_patch` metadata projects into `patchsets[]`.

### Code UI Wire Contract

The Code UI JSON contract uses camelCase field names and snake_case enum values. The Rust source of truth is `src/internal/ai/web/code_ui.rs`; the browser mirror is `web/src/lib/code-ui/types.ts`; `tests/ai_code_ui_wire_test.rs` pins the wire shape.

`GET /api/code/session` returns a `CodeUiSessionSnapshot`:

| Field | Type | Contract |
|-------|------|----------|
| `sessionId` | string | Runtime session identifier retained for compatibility. |
| `threadId` | string, optional | Canonical persisted Libra thread ID; prefer this for resume, graph, Web, MCP, and diagnostics flows when present. |
| `workingDir` | string | Session working directory. |
| `provider` | object | `{ provider, model?, mode?, managed }`. |
| `capabilities` | object | Eight booleans: `messageInput`, `streamingText`, `planUpdates`, `toolCalls`, `patchsets`, `interactiveApprovals`, `structuredQuestions`, `providerSessionResume`. |
| `controller` | object | `{ kind, ownerLabel?, canWrite, leaseExpiresAt?, reason?, loopbackOnly }`; `kind` is `none`, `browser`, `automation`, `tui`, or `cli`. |
| `status` | string | `idle`, `thinking`, `executing_tool`, `awaiting_interaction`, `completed`, or `error`. |
| `transcript` | array | Entries with `id`, `kind`, optional `title` / `content` / `status`, `streaming`, `metadata`, `createdAt`, `updatedAt`. |
| `plans` / `tasks` / `toolCalls` / `patchsets` | arrays | Runtime projections used by Workflow, Summary, Diff, and Terminal panes. |
| `interactions` | array | Pending/resolved UI prompts. `kind` is `approval`, `sandbox_approval`, `request_user_input`, `intent_review_choice`, or `post_plan_choice`. |
| `updatedAt` | string | ISO 8601 update timestamp. |

`GET /api/code/events` streams `CodeUiEventEnvelope` records with `seq`, `type`, `at`, and `data`. Event `type` is `session_updated`, `status_changed`, or `controller_changed`; `session_updated` carries a full `CodeUiSessionSnapshot`.

`GET /api/code/threads` returns `{ items, nextOffset? }`. Each item has `id`, optional `title`, `archived`, optional `currentIntentId`, `createdAt`, and `updatedAt`. `limit` defaults to 50 and clamps to 200; malformed `limit` or `offset` returns `INVALID_QUERY_PARAM`.

Code UI API errors use `{ error: { code, message } }`:

| Code | HTTP | Meaning |
|------|------|---------|
| `LOOPBACK_REQUIRED` | 403 | Non-loopback client attempted an API route. |
| `PAYLOAD_TOO_LARGE` | 413 | Write request body exceeded 256 KiB. |
| `CONTROL_DISABLED` | 403 | Automation control is not enabled for this process. |
| `MISSING_CONTROL_TOKEN` | 403 | Automation control token is absent. |
| `INVALID_CONTROL_TOKEN` | 403 | Automation control token is invalid. |
| `MISSING_CONTROLLER_TOKEN` | 403 | Lease token is absent for a write route. |
| `INVALID_CONTROLLER_TOKEN` | 403 | Lease token is invalid or stale for a write route. |
| `INVALID_CONTROLLER_KIND` | 400 | Controller attach requested an unsupported kind. |
| `CONTROLLER_CONFLICT` | 409 | Another live controller owns the lease, or the session is busy. |
| `BROWSER_CONTROL_DISABLED` | 403 | Browser write control is disabled. |
| `AUTOMATION_CONTROLLER_REQUIRED` | 403 | An automation-only path was called with a non-automation lease. |
| `CODE_UI_UNAVAILABLE` | 404 | No active `libra code` session is attached to the web server. |
| `INVALID_QUERY_PARAM` | 400 | Query parsing failed, currently for `/threads` pagination. |
| `STORAGE_PATH_INVALID` | 500 | Storage-root resolution failed. |
| `STATUS_UNAVAILABLE` | 500 | Runtime status snapshot is unavailable. |
| `THREAD_LIST_FAILED` | 500 | Thread projection enumeration failed. |
| `DB_UNAVAILABLE` | 500 | Session database is offline. |
| `INTERNAL_ERROR` | 500 | Fallback internal failure. |
| `UNSUPPORTED_OPERATION` | 422 | Runtime rejected a requested operation that is not yet supported. |

### Web Search

The `web_search` tool requires the session network policy to allow outbound access. If `BRAVE_SEARCH_API_KEY` is available from `vault.env.BRAVE_SEARCH_API_KEY` or the process environment, Libra tries the Brave Search API first and returns result titles, URLs, and snippets. If Brave is not configured or the request fails, Libra falls back to the zero-configuration DuckDuckGo HTML endpoint.

### Approval Policies

| Value | Aliases | Description |
|-------|---------|-------------|
| `never` | -- | No prompts; dangerous commands are rejected outright. |
| `allow-all` | `allow_all`, `always`, `accept` | No prompts; every command is allowed for this session (`allows_all_commands`). |
| `on-failure` | `on-failure` | Prompt only when retrying after a sandbox denial. |
| `on-request` | `on-request` | Run inside sandbox by default; prompt when escalation or policy requires it (default). |
| `untrusted` | `unless-trusted`, `untrusted` | Prompt for non-trusted operations; auto-allow known-safe reads. |

### Context Modes

| Value | Aliases | Description |
|-------|---------|-------------|
| `dev` | `development` | General development workflow. |
| `review` | `code-review` | Code review focus. |
| `research` | `explore` | Exploratory research and analysis. |

## Common Commands

```bash
# Start a TUI session with default Gemini provider
libra code

# Start with Anthropic Claude
libra code --provider anthropic --model claude-sonnet-4-20250514

# Bind web-only on all interfaces; remote browsers see a loopback-only notice
libra code --web-only --port 8080 --host 0.0.0.0

# Browser-driven session against a local Ollama
libra code --web-only --provider ollama --port 4400

# Allow browser write control over loopback (Codex web-only is loopback by default)
libra code --web-only --provider codex --browser-control loopback

# Enable local automation write control (writes token + lease discovery files)
libra code --control write

# Load provider keys from a dotenv-style file (overrides stale shell env vars)
libra code --env-file .env.test

# Run MCP over stdio for Claude Desktop integration
libra code --stdio

# Use DeepSeek with reasoning enabled
libra code --provider deepseek --model deepseek-v4-pro --deepseek-thinking enabled --deepseek-reasoning-effort high --deepseek-stream true
libra code --env-file .env.test --provider deepseek --model deepseek-v4-pro --deepseek-thinking enabled --deepseek-reasoning-effort high --deepseek-stream true

# Use Kimi (Moonshot AI) with the K2.6 default; opt out of thinking for lower latency
libra code --provider kimi
libra code --provider kimi --model kimi-k2-thinking --kimi-thinking enabled
libra code --provider kimi --model kimi-k2.6 --kimi-thinking disabled

# Use a local Ollama model; plain requests generate a reviewable plan first
libra code --provider ollama --model llama3 --api-base http://127.0.0.1:11434/v1

# Use compact tool schemas for a remote/cloud Ollama endpoint
libra code --provider ollama --model minimax-m2.7:cloud --api-base http://192.168.0.5:11434/v1 --ollama-compact-tools

# Enable high thinking for one Ollama run
libra code --provider ollama --model qwen3.6 --ollama-thinking high

# Capture provider/TUI diagnostics while using a local Ollama model
LIBRA_LOG='libra::internal::ai=debug,libra::internal::tui=debug' \
LIBRA_LOG_FILE=/tmp/libra-code.log \
libra code --repo=/Volumes/Data/linked --provider ollama --model gemma4:31b

# Resume a canonical Libra thread (TUI-only: --resume is not accepted with
# --web-only or --stdio, by design)
libra code --resume 11111111-1111-4111-8111-111111111111

# Inspect the same thread's version graph
libra graph 11111111-1111-4111-8111-111111111111

# Inspect a thread graph from outside that repository
libra graph 11111111-1111-4111-8111-111111111111 --repo /Volumes/Data/linked

# Start in code review context with strict approval
libra code --context review --approval-policy untrusted

# Use Codex with plan-before-execute mode
libra code --provider codex --plan-mode
```

## Human Output

Output is delivered through the TUI, web interface, or MCP protocol depending on the mode. There is no line-oriented stdout in the default TUI mode. In the generic provider TUI, a normal plain-text request starts the plan workflow automatically; explicit slash commands keep their command-specific behavior. Generic provider planning uses a two-step review: the LLM first drafts an IntentSpec for confirmation, then the confirmed IntentSpec is sent back to the LLM to generate a reviewable execution plan before any execution starts. If a confirmed plan executes and fails, or the orchestrator aborts before reaching a final decision, Libra feeds the failure evidence back into the planner, asks it to add or adjust repair steps, and automatically runs the revised plan up to the automatic repair threshold. After the threshold is reached, the TUI waits for the developer to either continue automatic repair with `/plan continue` or provide explicit plan repair guidance. The web server serves an embedded Next.js application. The stdio mode communicates via JSON-RPC messages following the Model Context Protocol.

## Diagnostics

`libra code` supports tracing through `RUST_LOG` or `LIBRA_LOG`; when both are set, `LIBRA_LOG` takes precedence. For TUI sessions, prefer `LIBRA_LOG_FILE=<path>` so diagnostics are written to a plain log file instead of the alternate-screen terminal. When `LIBRA_LOG_FILE` is set without an explicit log filter, Libra defaults to `libra=debug`.

For Ollama provider failures, useful diagnostics are:

```bash
mkdir -p /tmp/libra-logs
LIBRA_LOG='libra::internal::ai=debug,libra::internal::tui=debug' \
LIBRA_LOG_FILE=/tmp/libra-logs/libra-code-ollama.log \
libra code --repo=/Volumes/Data/linked --provider ollama --model gemma4:31b
```

If the TUI reports an Ollama 503, also capture the local server state:

```bash
ollama ps >> /tmp/libra-logs/libra-code-ollama.log
ollama list >> /tmp/libra-logs/libra-code-ollama.log
```

## Design Rationale

### Why a TUI + web server hybrid?

The TUI provides a low-latency, keyboard-driven interface for developers already working in a terminal. The background web server runs simultaneously so that the same session can be accessed from a browser -- useful for sharing context with teammates, viewing rich diffs, or accessing the session from a mobile device. The `--web-only` flag drops the TUI entirely for headless or remote server deployments where no terminal is available.

### Why multiple AI provider support?

Different providers excel at different tasks and have different cost/latency profiles. Gemini is the default for its generous free tier and fast response times. Anthropic Claude excels at careful reasoning and code review. Local Ollama support enables fully offline development. By abstracting behind a `CompletionClient` trait, adding a new provider requires only implementing the trait without touching the session, tool, or TUI layers.

### Why MCP integration?

The Model Context Protocol (MCP) is an open standard for connecting AI clients to tool servers. By supporting `--stdio` mode, Libra can act as a tool server for any MCP-compatible client (e.g., Claude Desktop). Libra exposes an allowlisted `run_libra_vcs` tool for version-control operations -- `status`, `diff`, `branch`, `log`, `show`, `show-ref`, `ls-files`, `add`, `commit`, and `switch` -- so external AI agents use Libra directly instead of invoking Git. `run_libra_vcs` only accepts those Libra subcommands; it is not a Git-compatible shell. For repository state inspection, prefer `status --json` or `status --porcelain v2 --untracked-files=all`, and use `ls-files` for tracked and untracked repository path inspection (for example `ls-files --others --exclude-standard` for ignore-aware untracked files). Libra-managed execution also rejects direct `git` shell commands.

### Why approval policies?

AI agents executing shell commands on a developer's machine present real safety risks. The five-tier approval system balances productivity with control:
- `never` is for fully locked-down environments where the agent can only read.
- `allow-all` is the opposite extreme: no prompts and every command runs, for trusted throwaway or sandboxed environments where friction outweighs risk.
- `on-failure` lets the agent try sandboxed execution and only asks when it fails.
- `on-request` (default) sandboxes everything and escalates when the agent or sandbox policy requires it.
- `untrusted` is the most conservative interactive mode, prompting for anything beyond known-safe reads.

### Why session persistence and resume?

Long coding sessions accumulate significant context: file edits, conversation history, tool outputs. Losing this context on an accidental terminal close is painful. Session persistence stores the full conversation and tool state, and `--resume <thread_id>` restores a canonical Libra thread.

The embedded Code UI exposes the same canonical identifier as `threadId` in its session snapshot. Older `session_id` fields remain present for compatibility, but new integrations should key resume, Web, MCP, and diagnostics flows by `threadId`.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Interactive AI session | `libra code` | Not available | Not available |
| TUI mode | Default | Not available | Not available |
| Web mode | `--web-only` | Not available | Not available |
| MCP/stdio mode | `--stdio` | Not available | Not available |
| AI provider selection | `--provider` | Not available | Not available |
| Session resume | `--resume <thread_id>` | Not available | Not available |
| Tool approval policy | `--approval-policy` | Not available | Not available |

Note: Neither Git nor jj have an equivalent to `libra code`. This command represents Libra's core differentiation as an AI-agent-native version control system. The closest analogs in the Git ecosystem are third-party tools like GitHub Copilot CLI or aider, which are separate applications rather than integrated VCS commands.

## Error Handling

| Scenario | Behavior | Exit |
|----------|----------|------|
| `--web-only` and `--stdio` both specified | Clap argument conflict error | non-zero |
| Missing API key for selected provider | Fatal error with provider name and expected env var | non-zero |
| Port already in use | Fatal error with port number | non-zero |
| No terminal available in TUI mode | Falls back or reports error | non-zero |
| Thread ID not found on resume | Fatal error with canonical `thread_id` | non-zero |
| `--control write --stdio` | Usage error; MCP stdio and local TUI automation stdio are separate modes | non-zero |
| `--control write --host 0.0.0.0` or other non-loopback host | Usage error; write control is loopback-only | non-zero |
| Another live `--control write` owns the same control lock | `CONTROL_INSTANCE_CONFLICT` with existing PID/URL when available | non-zero |
| Control token file is a symlink, non-regular file, or not `0600` on Unix/macOS | Fatal setup error before the web server starts | non-zero |
