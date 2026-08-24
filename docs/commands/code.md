# `libra code`

Launch an interactive AI coding session. Every `libra code` launch apart from MCP `--stdio` and the `--control stdio` client shim launches the Web Code UI (the legacy TUI startup path was removed in the W5 breaking release, W5-06).

## Synopsis

```
libra code
libra code [-p <PORT>] [--host <HOST>]
libra code --stdio
libra code --provider <PROVIDER> [--model <MODEL>]
libra code --resume <THREAD_ID>
libra graph --json <THREAD_ID> [--repo <PATH>]
```

## Description

`libra code` starts an interactive coding session that pairs a human developer with an AI agent. The default mode launches the Web Code UI (embedded HTTP server + AgentRuntime) and prints the URL / control details; it stays in the foreground until `Ctrl-C` / SIGTERM. **Breaking change (W5-07):** the deprecated `--web` / `--web-only` aliases and the hidden `LIBRA_CODE_LEGACY_TUI` rollback env were removed in the W5 breaking release — `libra code` already defaults to the Web Code UI, so simply remove the flag; the binary now rejects the old flags with a usage error plus a migration hint. **Breaking change (W5-06):** the legacy TUI startup path and the bare `--provider codex --resume <thread_id>` TUI resume driver were removed in the W5 breaking release — bare `--provider codex --resume <thread_id>` now fails with a usage error plus a migration hint (managed Codex Web resume has not landed; Web `--provider codex` rejects `--resume`). `--stdio` is a **deprecated MCP-only legacy** entry: it exposes MCP tools/resources over standard input/output for clients like Claude Desktop, and is **not** live turn control. Prefer `libra code --control stdio` for local automation; a dedicated `libra mcp --stdio` is planned after W5 (DEFER-02).

The command supports eight AI provider backends (Gemini, OpenAI, Anthropic, DeepSeek, Kimi, Zhipu, Ollama, Codex) and three operating contexts (dev, review, research) that tune the agent's behavior for different workflows. Sessions can be persisted and resumed with Libra's canonical `--resume <thread_id>` flow. Passing `--goal "<objective>"` boots the session directly in goal mode, where a supervisor drives the tool loop toward the stated objective until a verifier accepts completion.

A sandboxed tool-execution layer enforces approval policies that control when the agent can run shell commands, apply patches, web search, or perform other potentially destructive operations. Headless Web sessions in the `dev` context default to workspace-write execution with network access denied (the legacy TUI resume driver was removed in W5-06). After the execution plan is ready, the Plan review dialog offers Execute Plan / Modify Plan / Cancel. Modify closes the current review and treats the next plain-text message as a durable revision instruction before opening a replacement review. Execute opens a separate mandatory network-policy prompt (`Network: Deny` / `Network: Allow` / `Back`). Deny abandons execution and Back returns to a fresh Plan review; both gates survive crash/resume. Network Allow admits confirmed plan execution onto the serialized AgentRuntime queue. Mutating tools still require approval, sandbox, and tool ACL; failures enter the W2-11 repair loop. The catalogued `PLAN_EXECUTION_NOT_AVAILABLE` 409 is retained for older clients and is no longer produced on Allow. Review and research contexts remain read-only and do not grant network access.

The live version graph is in Web Code UI; `libra graph --json` remains the agent path, and the interactive graph TUI entry was removed in the W5 breaking release (W5-08). The legacy TUI exit path that printed a follow-up `libra graph --json <thread_id>` command was removed together with the TUI in W5-06; run `libra graph --json <thread_id>` yourself (use `--machine` for the compact form). Use `libra graph --json <thread_id> --repo <path>` when inspecting a repository other than the current directory.

**Linked worktrees**: `libra code` (every mode) launches from a linked worktree through the W4-06 RequestScope resolver. Security-sensitive files (`sandbox.toml`, `hooks.json`, `config.toml` `[approval]`/`[mcp]`, `rules/`, `contexts/`) and extension/automation surfaces (`agents.toml`, `automations.toml`, `agents/`, `commands/`, `skills/`) keep repository defaults visible. Security overlays may only tighten; extension overlays win on the same name. Unreadable or malformed security config, or a damaged worktree scope, fails closed with a source-layer diagnostic (no file contents). Automation VCS dispatch runs in linked worktrees via the same resolver — see [automation.md](automation.md). Always approvals stored under the canonical `libra.repoid` stay visible across linked worktrees with worktree/session provenance (audit only). Session and one-time approvals stay bound to the issuing controller lease and are dropped on lease takeover, detach, or expiry; a new controller must reconfirm. The in-memory approval cache is keyed by `repo:{libra.repoid}` — never a process-global `None` scope.

## Options

| Flag | Short | Long | Default | Description |
|------|-------|------|---------|-------------|
| Port | `-p` | `--port` | `3000` | Web server listen port. |
| Host | | `--host` | `127.0.0.1` | Web server bind address. |
| Working directory | | `--cwd` | current dir | Working directory for the session. |
| Env file | | `--env-file <PATH>` | none | Load provider environment variables from a dotenv-style file; explicit file values take precedence over Vault and the process environment. |
| Control mode | | `--control <observe\|write\|stdio>` | `observe` | Local automation control mode. `observe` preserves existing loopback read behavior; `write` enables local token discovery and process-level automation control auth; `stdio` is a **client-only** JSON-RPC NDJSON shim that drives an existing write-control session (no Web/MCP launch). |
| Control token file | | `--control-token-file <PATH>` | `.libra/code/control-token` | Path for the per-process local automation token. In `write` mode, Unix/macOS files must be regular files with `0600` permissions. With `--control stdio`, overrides the worktree default token path (still independent of `--control-info-file`); overly permissive modes fail closed (`CONTROL_TOKEN_PERMS`). |
| Control info file | | `--control-info-file <PATH>` | `.libra/code/control.json` | Path for non-secret local endpoint discovery metadata. Written atomically at `0600` on Unix/macOS in launch modes. Never contains token material. With `--control stdio`, this is the **read** discovery path for `baseUrl` only (explicit `--control-url` overrides). Custom info paths do **not** relocate the default token — pass `--control-token-file` when the token is not under the worktree `code/` directory. |
| Control URL | | `--control-url <URL>` | (discovered) | Base URL of an existing Code UI control endpoint (e.g. `http://127.0.0.1:3000`). Only valid with `--control stdio`. When omitted, discovered from `--control-info-file`. Must be a literal loopback IP. |
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
| Network access | | `--network-access <allow\|deny>` | `deny` | Default network policy for shell/gate. Only the default `deny` is accepted: `--network-access allow` is rejected in every mode until the Plan network-policy gate owns per-execution sandbox network (approve network in Plan review instead). |
| MCP port | | `--mcp-port` | `6789` | MCP server listen port. |
| Stdio | | `--stdio` / `--mcp-stdio` | off | Deprecated MCP-only legacy: tools/resources over stdio (not turn control). Prefer `--control stdio` for automation; dedicated `libra mcp --stdio` planned after W5. |
| API base | | `--api-base` | provider default | Provider API base URL override. |
| Codex binary | | `--codex-bin` | `codex` | Codex executable path. |
| Codex port | | `--codex-port` | random | Override Codex app-server port. |
| Plan mode | | `--plan-mode [<true\|false>]` | off (on for Codex) | Require the agent to produce an approved plan before execution. The effective default is on for `--provider codex` and off for every other provider; Explicit `--plan-mode=true` (or bare `--plan-mode`) is only accepted with `--provider=codex` — it is rejected for other providers; pass `--plan-mode=false` to opt a Codex session out. |
| Browser control | | `--browser-control <off\|loopback>` | `loopback` | Posture for `/api/code/controller/attach` browser leases. Conflicts with `--stdio`; `loopback` requires a loopback `--host`. |
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
For normal runs, store provider keys in `vault.env.<NAME>`; Libra checks repo-local Vault, then global Vault, then the process environment. Use `--env-file .env.test` for live tests that need an explicit dotenv override. On the default Web launch, `--env-file`, `--context`, `--approval-policy`, and `--approval-ttl` apply to non-Codex providers (env-file values still override process env/Vault). Managed Web `--provider codex` still rejects `--env-file`, `--approval-ttl`, and `--resume` because those surfaces are not wired into the Codex app-server path; bare `libra code --provider codex --resume <thread_id>` is likewise rejected with a usage error plus a migration hint (the legacy TUI resume driver was removed in W5-06). MCP `--stdio` continues to reject the Web-only flags.

Ollama requests stream `/api/chat` responses by default and add a per-request `request_id` to debug logs. They also default to `think:false` so reasoning-capable local models do not spend several minutes generating hidden reasoning before tool calls. Use `--ollama-thinking high` for a single run, or set `OLLAMA_THINK=true`, `low`, `medium`, `high`, or `auto` as the environment default. `auto` omits the `think` field and lets Ollama decide. Use `--ollama-compact-tools` or `OLLAMA_COMPACT_TOOLS=true` when a remote/cloud Ollama endpoint accepts simple tools but returns 503 for Libra's full tool schema payload.

### Local Automation Control

`libra code --control observe` is the default and does not create local control files unless `--control-info-file` is explicitly supplied. Loopback clients can continue reading `/api/code/session` and `/api/code/events` without a token.

`libra code --control write` enables the local automation security envelope. Libra creates a fresh 32-byte token in `.libra/code/control-token`, atomically writes non-secret endpoint metadata to `.libra/code/control.json` (Unix/macOS mode `0600`) after the web server binds, and holds `.libra/code/control.lock` for the process lifetime. Default paths are per worktree local-gitdir, so two worktrees never share a token/info/lock; a cross-worktree scope mismatch fail-closes rather than reclaiming another worktree's sidecar. `control.json` includes `version`, `mode`, `pid`, `baseUrl`, optional `mcpUrl`, `workingDir`, optional `threadId`, `startedAt`, and version-2 writer scope (`repoId`/`worktreeId`/optional `workspaceId`/`leaseFence`); it never includes the token, token hash, token path, provider credentials, headers, or provider request/response bodies.

Write control is local-only. `--control write` is rejected with `--stdio`, and it requires `--host` to be loopback (`127.0.0.1`, `::1`, or `localhost`). A second write-control instance using the same default paths fails fast with `CONTROL_INSTANCE_CONFLICT`; use distinct `--control-token-file` and `--control-info-file` paths only when the caller intentionally manages multiple local instances.

Automation clients attach with `POST /api/code/controller/attach`, body `{ "clientId": "...", "kind": "automation" }`, header `X-Libra-Control-Token`, and then use the returned `X-Code-Controller-Token` for writes. Automation-held leases require both tokens for `/api/code/messages`, `/api/code/interactions/{id}`, `/api/code/controller/detach`, and `/api/code/control/cancel`. Code UI write request bodies are capped at 256KiB. A plan-repair Continue that raises an exhausted retry limit sends `{ "selectedOption": "continue", "maxAttempts": 3 }`; `maxAttempts` must exceed the current limit and not exceed 10. When the session advertises `capabilities.commandIdempotency` (headless web-only today), `POST /api/code/messages` accepts `{ "text": "...", "commandId": "..." }` for retry de-duplication (same id + same text is idempotent; same id + different text returns `COMMAND_PAYLOAD_CONFLICT`). The runtime namespaces each `commandId` under a SHA-256 fence of the active controller `clientId` before durable admission (the raw clientId is never written into the command log). `commandIdempotency` is advertised only when durable SessionStore command admission is configured.

`GET /api/code/diagnostics` returns a redacted observe-only status summary for local tools. Control attach, detach, submit, respond, and cancel operations emit `local-tui-control/v1` audit events through the runtime audit sink; this identifier is frozen for audit-consumer compatibility and does not denote an active terminal UI. For stdio automation clients, prefer the canonical `libra code --control stdio` JSON-RPC NDJSON client: it discovers the endpoint from `.libra/code/control.json` by default (override with `--control-url` / `--control-token-file` / `--control-info-file`). Discovery fails closed with stable codes (`CONTROL_INFO_MISSING`, `CONTROL_INFO_PERMS`, `CONTROL_TOKEN_MISSING`, `CONTROL_TOKEN_PERMS`, `CONTROL_SCOPE_CONFLICT`, `CONTROL_SERVER_MISSING`); attach lease/ownership conflicts surface as JSON-RPC `-32000` with Libra codes such as `CONTROLLER_CONFLICT`. The former `libra code-control` forwarding shim was **removed in the W5 breaking release (W5-01)**; `libra code --control stdio` is the only stdio automation client (see the [migration note](code-control.md)). Deprecated `libra code --stdio` remains the **MCP-only** tools/resources transport (stderr deprecation warning; not turn control) and must not be confused with `--control stdio`; a dedicated `libra mcp --stdio` is planned after W5.

The stdio client speaks newline-delimited JSON-RPC 2.0 on stdin/stdout and maps methods onto the loopback `/api/code/*` HTTP/SSE control surface:

| JSON-RPC method | HTTP equivalent |
|-----------------|-----------------|
| `session.get` | `GET /api/code/session` |
| `events.subscribe` | `GET /api/code/events` as JSON-RPC notifications |
| `diagnostics.get` | `GET /api/code/diagnostics` |
| `controller.attach` | `POST /api/code/controller/attach` |
| `controller.detach` | `POST /api/code/controller/detach` |
| `message.submit` | `POST /api/code/messages` |
| `task.dispatch` | `POST /api/code/task/dispatch` |
| `interaction.respond` | `POST /api/code/interactions/{id}` |
| `turn.cancel` | `POST /api/code/control/cancel` |
| `goal.start` | `POST /api/code/goal/start` |
| `goal.status` | `GET /api/code/goal/status` |
| `goal.cancel` | `POST /api/code/goal/cancel` |

Malformed JSON maps to JSON-RPC `-32700`. Unknown methods map to `-32601`. Invalid params map to `-32602`. HTTP 4xx/5xx errors map to `-32000` with `data.status` and `data.code`, preserving Libra errors such as `INVALID_CONTROL_TOKEN`, `INVALID_CONTROLLER_TOKEN`, `CONTROLLER_CONFLICT`, and `INTERACTION_NOT_ACTIVE`.

### Web Browser Control

`--browser-control <off|loopback>` controls whether the embedded UI's lease-based write surface is available. The default is `loopback` for the Web launch.

Selecting `loopback` is rejected when `--host` is not a loopback address, and the flag conflicts with `--stdio`. Use `--browser-control off` when binding a non-loopback `--host` for observe-only / remote-notice serving.

**Local trust model:** browser attach requires loopback bind + trusted same-origin `Origin`/`Referer` + rate limits (W3-05) **and** a per-session `X-Libra-Browser-Bootstrap` secret (printed on stdout / embedded as `?bt=` in the open URL). Forgeable Origin alone is not enough. Libra does **not** auto-open a `?bt=` URL (so the bootstrap secret never appears in opener argv on shared hosts); open the printed URL yourself. On shared machines, prefer `--browser-control off` (observe-only) or keep the session on a private host.

The browser server-side endpoints are tagged in the `code_router()` audit matrix (`src/internal/ai/web/mod.rs`):

- `GET /api/code/session`, `GET /api/code/thread-graph?threadId=`, `GET /api/code/events`, `GET /api/code/diagnostics`, `GET /api/code/threads`, `GET /api/code/usage`, `GET /api/code/skills`, `GET /api/code/goal/status` — loopback-only observe.
- `POST /api/code/controller/attach` — loopback. `kind: "automation"` requests additionally require `X-Libra-Control-Token`. The handler **issues** the lease's `controllerToken` (it does not expect the caller to send one).
- `POST /api/code/controller/detach`, `POST /api/code/messages`, `POST /api/code/interactions/{id}` — loopback + `X-Code-Controller-Token`; `Automation` leases additionally require `X-Libra-Control-Token`.
- `POST /api/code/control/cancel` — loopback + `X-Code-Controller-Token`. `Automation` leases also require `X-Libra-Control-Token`.
- `POST /api/code/task/dispatch` — loopback + `X-Code-Controller-Token`; user-initiated sub-agent dispatch requires an active controller write lease (browser or automation). Automation leases additionally require `X-Libra-Control-Token`.
- `POST /api/code/goal/start`, `POST /api/code/goal/cancel` — loopback + `X-Code-Controller-Token`; goal mutation requires the active controller lease.
- `POST /api/code/skills/activate`, `POST /api/code/session/resume` — loopback + `X-Code-Controller-Token` on the write router (256 KiB body limit); both require an active controller write lease. Automation leases additionally require `X-Libra-Control-Token`. Resume refuses busy and indeterminate snapshots, and currently fail-closes with `SESSION_RESUME_REQUIRES_RESTART` after proving the target thread is loadable (in-process AgentRuntime swap is not available yet). Skill activate fail-closes with `SKILL_ACTIVATION_UNSUPPORTED` after discoverability validation until a provider-consumed activation path exists.

Browser write requests share the same 256 KiB body limit and audit-sink wiring as automation control. The browser persists the lease only in memory; reloading the page drops the lease and the next write reattaches.

Browser writes (including `POST /controller/attach` with `kind: "browser"`) additionally require a trusted loopback `Origin` (or same-origin `Referer` fallback) that matches the Code UI bind address (exact `http://<bound-ip>:<port>`, plus `localhost` / `127.0.0.1` / `[::1]` aliases when bound to canonical loopback). Missing or cross-site Origin fails closed with `ORIGIN_REQUIRED`. Automation writes authenticate with `X-Libra-Control-Token` / controller lease and do **not** use Origin as a substitute. Per-session write rate limiting applies to both browser and automation producers (`LIBRA_CODE_SESSION_WRITE_RATE_LIMIT` / `LIBRA_CODE_SESSION_WRITE_RATE_WINDOW_SECS`, default 120 writes / 60s) and returns `429 RATE_LIMITED` until the window recovers.

The embedded SPA session-lifecycle panels list threads via `GET /api/code/threads`, cancel the active turn through `POST /api/code/control/cancel` (fail-closed when `controller.canWrite` is false), and post resume selection through `POST /api/code/session/resume` with `{ "threadId": "..." }`. Thread list is repository-storage-scoped (shared across linked worktrees), while resume is working-directory scoped; listed items omit `workingDir` until ThreadProjection persists a per-thread cwd.

The usage panel mirrors the W2-12 `RuntimeUsageTotals` read model (cumulative, current-turn delta, sub-agent attribution) and keeps `partial`/`unknown`/`error` visible instead of pretending zero spend. `GET /api/code/usage` reads durable totals and returns an error rather than fabricated zeroes. When durable sub-agent enumeration is unavailable, the response omits `subAgents` and sets `subAgentsStatus: "unavailable"` instead of an empty array.

The execution/repair panel projects `plans[]`, `toolCalls[]`, and `planExecutionRepair` from the live session snapshot. Continue/Cancel post through `POST /api/code/interactions/{id}` with `selectedOption` (`continue` / `cancel`); when projected `attempt >= max_attempts`, Continue also sends a raised `maxAttempts` (capped at 10) without reclassifying the failure on the client.

The SSE resilience panel surfaces reconnecting / resync-required / resynced status while keeping the last projected session snapshot and the last wire-supplied cursor seq (the browser never invents sequence numbers). Explicit snapshot resync routes through the shared store `refresh()` path and only reports success when that refresh applies (or is superseded by a newer live update). Production v2 transport backlog/resync (`event: resync` / `WIRE_V2_RESYNC_REQUIRED`) is delivered by W3-08; the built-in SPA cutover to consume it remains W3-09.

The workflow review panel projects pending `intent_review_choice` and `post_plan_choice` interactions (network policy is the same kind with `metadata.phase = "networkPolicy"`). Confirm/modify/cancel (and execute / network-allow / network-deny / back) post `selectedOption` through the leased interaction endpoint; turn cancel is fail-closed when the browser cannot write. After Plan Modify, the next accepted plain-text message is the revision note; Libra durably binds it to the rejected plan and opens a replacement review after Phase 1 completes. The panel does not keep a second workflow FSM — it waits for the next snapshot/SSE update. Network Allow consumes the network gate and admits confirmed plan execution onto the serialized runtime queue; mutating tools still pass through approval/sandbox/ACL, and repair remains W2-11-owned.

After IntentSpec Modify, revision mode accepts the next plain-text message or the strict control form `/intent modify <changes>`. Its private HMAC-authenticated state progresses through `Prepared`, `Active`, `Claiming`, and event-bound `Consuming`: the full consumer command binding is fsynced before Runtime admission and its exact event id/sequence is bound before the executor start gate opens. A receipt is committed only after durable effect proof: the fixed no-provider effect for exact raw `/intent cancel`, or exactly one successful `submit_intent_draft` plus the replacement review marker for Modify. The sidecar is removed only after that receipt. While revision authority is active, a fresh explicit direct command returns `409 SESSION_BUSY` before any intent or transcript mutation; an exact terminal or matching live `commandId` retry remains an idempotent acknowledgement. Padded cancel spellings are ordinary explicit-direct input. `/intent cancel` does not cancel a pending Plan revision; that state still requires its next non-empty plain-text Plan note.

Startup validates revision authority by constructing one shared, linear
`ValidatedIntentRevisionReceiptIndex` for the replay. Source-terminal,
receipt, consumer-status, replacement-marker, and retry-lineage lookups reuse
that one index; startup does not scan the full event stream again for every
receipt. The regression fixture uses exactly 5,000 events and 700 receipts and
requires no more than four indexed relationship visits per replay event.

Each Phase 1 review is bound to the canonical checkout identity and an exact,
ignore-aware content fingerprint. New v0.20.4 bindings also carry a
`metadata-v1:<sha256>` change token over paths, entry kinds, symlink targets,
and change-sensitive metadata. That token drives fast resume projection and
determinate pre-write validation/retry signals; it is never sufficient to
authorize Execute, which rechecks the content fingerprint. Each new binding is
captured over one stable interval (`metadata before -> exact content before ->
exact content after -> metadata after`); any exact or metadata mismatch rejects
the capture instead of pairing an old content authority with a newer drift
baseline, including on platforms with coarse file metadata. Older review contexts without
the additive token remain readable and fall back to content comparison using
the exact fingerprint. Every scan enforces a 30-second cooperative work budget,
a 1,000,000 traversed-entry budget (directories included), and a 128 MiB
cumulative encoded-path-name memory budget. Paths are streamed and counted
before the bounded manifest is sorted deterministically, so a very wide
directory cannot bypass the entry limit. A blocking filesystem operation or
directory iterator step can overrun the cooperative time budget; every such
operation, including the final EOF step, is checked when control returns and
fails closed instead of accepting an unverified workspace. Path-name
enumeration remains an ignore-aware walk; it supplies a bounded, sorted name
list but is not itself read authority. On Unix, authoritative entry access uses
pinned root/parent file descriptors with `openat`/`fstatat`/`readlinkat`,
no-follow/nonblocking opens, and identity, type, metadata, path, and reopen
checks. On Windows, pinned handles, `FILE_FLAG_OPEN_REPARSE_POINT`, final-path/
file-identity verification, and `FSCTL_GET_REPARSE_POINT` validate regular
files and reparse targets. Other platforms fail closed because secure
workspace fingerprint reads are unsupported. Symlink, FIFO, replacement,
parent/root swap, rename, and reparse races therefore fail closed instead of
hashing content outside the checkout.

Startup recovery requires one complete, contiguous committed workflow replay.
A sequence gap or bounded-window cut fails before Plan/Network authority is
chosen or any Phase 1 context is garbage-collected. The session append lock is
a persistent regular-file OS advisory lock: it is never age-reclaimed or
unlinked by a guard, and a symlink/special lock path fails closed.

Before reload or projection fold, a resumable headless session also acquires a
process-lifetime Phase 1 writer lease. This is a different lock from the
short-lived workflow append lock and a different ownership domain from the
browser/automation controller lease. It is bound to the exact SessionStore
path and session id, and all clones share a one-shot claim so only one
independently constructed persistence graph can consume it. Unix opens use
`O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC` and device/inode identity checks;
Windows opens with `FILE_FLAG_OPEN_REPARSE_POINT`, rejects reparse points, and
checks volume/file identity. The persistent lock path is never unlinked, while
process exit releases the advisory lock immediately.

Workspace or checkout change signals remain recoverable gate state on resume:
interaction metadata exposes `workspaceDrifted: true` and an actionable
`workspaceWarning`; Libra does not fence the session or start mutation. A
metadata-only signal is advisory: Execute performs the exact identity/content
recheck and may proceed when that authority still matches. Exact identity or
content drift, or a failed exact verification, returns
`409 PHASE1_WORKSPACE_CHANGED` and preserves the exact pending Plan gate.
Modify may regenerate against a new HEAD in the same Intent repository;
its next non-empty plain-text note recaptures the current checkout without
altering the user's files. A different repository identity requires Cancel and
a new request/IntentSpec review. Failed recapture returns the same 409 without
consuming the note. An empty or whitespace-only revision message returns
`400 PLAN_REVISION_NOTE_REQUIRED` and leaves revision authority pending.

When the server is bound to a non-loopback host (`--host 0.0.0.0` or a LAN address), non-loopback browsers receive a static remote access notice for HTML navigation instead of the SPA. The notice is zero JavaScript, includes only bind/remote/version/commit placeholders, and asset/API fallbacks return 404 so remote clients cannot probe session state. Snapshot, transcript, SSE, approval, and every `/api/code/*` read/write surface stay loopback-only (`LOOPBACK_REQUIRED`). Remote humans should SSH port-forward to loopback (`ssh -L 3000:127.0.0.1:3000 user@host`) rather than expecting a direct remote write UI; authenticated TLS reverse proxies are deferred (DEFER-04) and are not the default.

Default listen port is `3000`. If that address is already bound, startup fail-closes with an actionable `--port` hint and never auto-scans the next free port.

Only one writer holds the controller lease at a time: a second browser or automation attach fails with `CONTROLLER_CONFLICT` while another lease is active, and a lease takeover drops the previous controller's session approvals (see Approval Policies below).

For the default Web launch with non-Codex providers (`--provider ollama` is the canonical headless verification path), Libra builds a [`HeadlessCodeRuntime`](../../src/internal/ai/web/headless.rs) lifecycle host and mounts [`AgentRuntimeCodeUiAdapter`](../../src/internal/ai/web/agent_runtime_adapter.rs) as the production browser write-path owner. Browser submits enter the serialized `AgentRuntimeWorker`: plain (non-`/`) messages use the shared Phase 0 plan tool allowlist so direct chat cannot bypass the default mutating gate; slash/`/`-prefixed messages keep an explicit direct tool loop except for the revision controls `/intent modify <changes>` and `/intent cancel` described above. IntentSpec review, Phase 1 drafting/revision, Plan/network-policy gates, approval, and resume use runtime-owned interaction and event paths. Network Allow submits confirmed plan execution through `submit_confirmed_plan_execution` onto the serialized AgentRuntime queue. Mutating tools still pass through the shared hardening/approval/sandbox/ACL boundary, and the W2-11-owned repair loop parks after a classified failure. Headless mode advertises `messageInput`, `streamingText`, `toolCalls`, `planUpdates`, `patchsets`, `interactiveApprovals`, `structuredQuestions`, and `providerSessionResume`. Default Web `--resume <thread_id>` reloads the matching session for non-Codex providers in the same working directory, then applies the bounded durable Code UI projection suffix before starting the browser server. `--resume` remains unavailable with Web `--provider codex`, and bare `libra code --provider codex --resume <thread_id>` is rejected with a usage error plus a migration hint (the legacy TUI resume driver was removed in W5-06; managed Codex Web resume has not landed yet). `update_plan` projects into `plans[]`, and `apply_patch` metadata projects into `patchsets[]`. Cancellation is cooperative before a tool's mutation boundary. After a potentially mutating tool has begun, cancellation is accepted (`200`) and the runtime interaction enters `Cancelling`; the browser-visible `status` remains `executing_tool` until the tool reaches a determinate result. Libra never hard-aborts that side effect or relabels it as an ordinary cancelled turn, and a concurrent submit is rejected with `SESSION_BUSY` while cancellation is pending.

For Web `--provider codex`, managed app-server websocket notifications are normalized into the shared runtime `AgentEvent` envelope (same projection path as other providers). Unknown Codex methods take an explicit diagnosable `ProviderNotification` fallback rather than silent drop or panic. Ask-mode approvals park on the shared `AgentRuntime` interaction registry and forward browser `respond_interaction` decisions to the app-server; Codex still owns the in-app-server approval loop (see DEFER-07 in `docs/development/tracing/code.md`). Outward approval option ids match non-Codex (`approve` / `deny` / `abort`).

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
| `controller` | object | `{ kind, ownerLabel?, canWrite, leaseExpiresAt?, reason?, loopbackOnly }`; `kind` is `none`, `browser`, `automation`, `tui`, or `cli`. `tui` is decoded only from historic snapshots and every new lease rejects it with `INVALID_CONTROLLER_KIND`; it is never emitted for a new session. |
| `status` | string | `idle`, `thinking`, `executing_tool`, `awaiting_interaction`, `completed`, `error`, or `indeterminate_side_effect`. The final value means a mutating command may have taken effect and must be reconciled before any retry. |
| `transcript` | array | Entries with `id`, `kind`, optional `title` / `content` / `status`, `streaming`, `metadata`, `createdAt`, `updatedAt`. |
| `plans` / `tasks` / `toolCalls` / `patchsets` | arrays | Runtime projections used by Workflow, Summary, Diff, and Terminal panes. |
| `interactions` | array | Pending/resolved UI prompts. `kind` is `approval`, `sandbox_approval`, `request_user_input`, `intent_review_choice`, `post_plan_choice`, or `plan_execution_repair`. A pending plan-repair interaction offers `continue` and `cancel`; respond through the normal interaction endpoint. |
| `planExecutionRepair` | object, optional | Runtime-owned plan-execution repair state. It contains a snake_case `state`, bounded and runtime-redacted failure `evidence` (`output`, `diagnostics`, `attempt`, `max_attempts`), and an `interaction_id` while `awaiting_user`. `automatic_repair` records an in-progress retry. `awaiting_user` is projected only after the configured retries are exhausted: a Code UI Continue must send a higher `maxAttempts` (for example, `{ "selectedOption": "continue", "maxAttempts": 3 }`), otherwise it returns `PLAN_REPAIR_RETRY_LIMIT_REACHED`; alternatively, provide manual revision guidance. Cancel is terminal. `intent_spec_revision` and `manual_action` require a new user-directed workflow. |
| `threadGraph` | object, optional | Indexed Intent/Plan/Task/Run/PatchSet graph for the current `threadId` (W4-04). Omitted or cleared when storage is unresolved, the id is not a UUID, load fails, or live snapshot heads are not covered. Same camelCase shape as `GET /api/code/thread-graph`. |
| `updatedAt` | string | ISO 8601 update timestamp. |

`GET /api/code/thread-graph?threadId=<uuid>` returns the same `CodeUiThreadGraph` object used by `threadGraph` (loopback observe, fail-closed redaction). `threadId` is parsed with `Uuid::parse_str` (hyphenated RFC-4122 or 32 hex digits). Anything else is `THREAD_GRAPH_INVALID_ID`. Missing indexed projection is `404 THREAD_GRAPH_NOT_FOUND`; storage/load/redaction failures are `500 THREAD_GRAPH_STORAGE_UNAVAILABLE` / `THREAD_GRAPH_UNAVAILABLE` / `REDACTION_FAILED`.

| Field | Type | Contract |
|-------|------|----------|
| `threadId` | string | Canonical thread UUID. |
| `title` | string, optional | Thread title (redacted). |
| `selectedPlanId` / `activeTaskId` / `activeRunId` | string, optional | Live heads; may also appear as `selected` / `active` / `running` node tags. |
| `nodes` | array | `{ depth, kind, id, label, tags? }` for `intent` / `plan` / `task` / `run` / `patchset`. Capped at 256; live heads are preserved and remaining slots fill from newest lineage. |
| `truncated` | boolean, optional | Present when the indexed graph exceeded the 256-node cap. |
| `omittedNodeCount` | number, optional | Nodes dropped by the cap. |
| `totalNodeCount` | number, optional | Full indexed node count when truncated. |

`GET /api/code/events` streams session updates. Wire version is negotiated as follows
(W3-06 / plan-20260715):

| Selection | Mechanism |
|---|---|
| Explicit v1 | `?wire=1` or `?wire=v1` |
| Explicit v2 | `?wire=2` or `?wire=v2` |
| Accept hint | `Accept: text/event-stream;libra-wire=2` (query `wire=` wins if both are set) |
| Default (unspecified) | **v1** for clients that omit `wire` / `libra-wire`. The built-in SPA (W3-09) always requests `?wire=2`. |
| Illegal values | fail-closed `400 INVALID_WIRE_VERSION` |

**SSE v1** (default): `CodeUiEventEnvelope` records with `seq`, `type`, `at`, and
`data`. Event `type` is `session_updated`, `status_changed`, or
`controller_changed`; `session_updated` carries a full `CodeUiSessionSnapshot`.

**SSE wire v2**: `code_workflow` events with camelCase `cursor` (durable W1-06
workflow sequence), `eventId`, `kind`, `at`, and minimal `payload`. Reconnect with
`?wire=2&cursor=<lastCursor>` to replay without duplicates or gaps inside the
**transport** backlog window (W3-08 / GC-CODE-12): **1,024 events or 8 MiB**,
whichever is reached first (`MAX_CODE_UI_TRANSPORT_BACKLOG_*`). The Code UI
**projection** hot window is a separate budget with the same numeric caps
(`MAX_CODE_UI_PROJECTION_EVENTS` / `MAX_CODE_UI_PROJECTION_REPLAY_BYTES`);
do not add the two together. Single-event folds visit only the suffix, not
the full session history (W3-14; release p95 ≤ 5 ms on 10k-event sessions). When bootstrap or a lagged consumer would
exceed that budget, the server emits `event: resync` with
`WIRE_V2_RESYNC_REQUIRED` (`reason`, `lastCursor`, `durableTail`,
`action: fetch_snapshot`) and ends the stream — never silent-drops. Clients
fetch a session snapshot, then reconnect at `durableTail`. Wire v2 requires a
SessionStore-backed workflow hub. Today that hub is mounted for default Web
headless runs with session persistence (non-Codex
`HeadlessCodeRuntime`). Managed
`--provider codex` Web currently returns `503 WIRE_V2_REQUIRES_DURABLE_SESSION`
until that runtime exposes a hub.

The v2 envelope uses camelCase, while each `payload` retains the durable
workflow event's snake_case schema. New `plan_review_requested` payloads include
`context_id`, the immutable Phase 1 context binding; after Back, a fresh
interaction id reuses the source `context_id` instead of copying the context.
A replacement plan produced after Modify may also include optional
`revision_of`, whose value is the prior Plan review interaction it consumes.
When Back prepares a replacement Plan gate, the row also carries optional
`prepared_from_network`; that Plan gate becomes authoritative only after the
named Network gate has a durable `back` resolution. Historical rows omit these
fields: `context_id` decodes as `""` and falls back to the row's
`interaction_id`, while `revision_of` and `prepared_from_network` decode as
`None`. Clients must treat absent lineage as an initial or legacy Plan review
rather than inventing it. Readers that predate these fields ignore the additive
members and still recognize the row as `plan_review_requested`.

Before the runtime performs the formal execution/test Plan pair write, it emits
`phase1_formal_write_started` with `phase1_turn_id`,
`source_interaction_id`, and a non-secret `seed_digest`. Recovery may reattach
an exact seed-backed Pending Phase 1 command only while this marker is absent;
once it is present, a missing `plan_review_requested` result is an indeterminate
write boundary and must fail closed for reconciliation.

When a command accepts an interaction but continues running, its durable
checkpoint remains the existing `interaction_resolved` event kind. New rows
may include a `command` identity and `prior_interaction_resolutions`; the
legacy `interaction_id` / `resolution` fields remain the current checkpoint.
This additive shape lets older readers retain the workflow sequence and the
current non-secret audit label without treating the still-Pending command as
terminal.

Command terminal rows retain the same snake_case payload convention. A
`command_terminal_success_with_interaction_resolved` row keeps the current
gate in the legacy `interaction_id` / `resolution` fields and may include
`prior_interaction_resolutions` for earlier approvals or user-input answers
delivered by the same command. A `command_terminal_failure` row may similarly
include `interaction_resolutions`. Each history entry is the two-element array
`[interaction_id, non_secret_resolution_label]`; raw answers, approval payloads,
and provider output are never stored there.

A canonical IntentSpec `modify` terminal may also include only the public
binding `intent_revision: { "interaction_id": "...", "sidecar_digest":
"hmac-sha256:<64 lowercase hex>" }`. A non-empty Modify note is trimmed and
must be at most 16 KiB (16,384 UTF-8 bytes). Before committing that terminal,
Libra durably writes a private, session-local **Prepared** sidecar containing the raw
note and exact IntentSpec, plus a keyed HMAC-SHA256 over its schema, lineage,
and body. The crash-atomic terminal row and fsync retain the legacy primary
`interaction_id` / `resolution` and the digest binding, but never the raw note
or HMAC key. That exclusion applies to the workflow terminal and consumption
receipt and to their dedicated SSE v2 payloads. It does not redact ordinary
transcript entries, full session snapshots, or their projection deltas, which
retain user text under the existing user-content boundary.

The HMAC key is local to this session and stored beside the local sidecar. Its
digest proves exact durable lineage across crashes and keeps private content
out of the public workflow stream; it is **not** a defense against a same-user
local attacker who can rewrite both files. A missing key after any committed
HMAC binding, a non-canonical or mismatched digest, an ambiguous lineage, or a
bound terminal missing both its sidecar and an exact consumption receipt fails
closed for reconciliation.

After the terminal fsync, **Prepared** is promoted to authenticated **Active**
revision mode. On startup, an orphan Prepared sidecar while the original gate
is still open is discarded; a matching committed terminal promotes it, while
any mismatch fails closed. The next accepted revision turn first makes its
consumer command intent and browser-message snapshot durable, then fsyncs a
**Claiming** envelope with the complete command identity before Runtime
admission. Once the durable intent event id and sequence are known, Libra
promotes Claiming to event-bound **Consuming** before the executor start gate
opens.

Consumption is committed only after the requested effect has durable proof.
For exact raw `/intent cancel`, that proof is the fixed no-provider cancellation
effect, so Libra appends and fsyncs the receipt before unlinking the sidecar and
acknowledging cancellation. For a plain revision note or strict
`/intent modify <changes>`, the provider must make exactly one successful
`submit_intent_draft` call; Libra durably writes the replacement IntentSpec and
its `IntentReviewRequested` marker before appending the receipt and unlinking
the sidecar. Zero or multiple successful draft submissions fail closed and do
not consume the revision. Padded or otherwise non-exact spellings of
`/intent cancel` are ordinary explicit-direct input, not the privileged fixed
cancel operation. The Modify suffix is trimmed and limited to 16 KiB before
Claiming is written.

The committed consumption is appended as the additive
`intent_revision_consumption` receipt field on the workflow event stream,
binding the exact terminal lineage, the consumer command intent, and the
consumer intent event id/sequence. SSE v2 projects a committed receipt as the
dedicated `kind: "intent_revision_consumed"` with payload `{ "consumption":
... }`; it omits generic resolution fields and the raw note. A crash before durable effect
proof keeps Consuming for exact restart recovery. A crash after a replacement
review marker or cancellation receipt is recovered without rerunning the
provider, normalizes any stale transcript/tool projection, and preserves the
same replacement gate (or completed cancellation). A valid receipt permanently
closes the whole exact retry lineage even after that replacement gate is later
resolved; later Web commands cannot make the historical consumer ambiguous.
Conflicting marker/terminal/receipt ordering, including a replacement marker
written after an incompatible consumer terminal, fails closed and never
manufactures a receipt.

Prepared and Consuming deliberately carry an empty legacy `intentSpec`, so
readers that predate those envelopes reject them instead of activating an
uncommitted or ambiguously consumed revision. Active is legacy-readable only
after its terminal is committed; if an old binary consumes it without the new
receipt, a later new reader fails closed rather than treating sidecar absence
as proof of consumption. Historical terminal rows omit `intent_revision`,
which decodes as `None`; older workflow readers ignore this additive member and
still close the terminal from the legacy primary fields. New readers accept a
pre-binding legacy Active sidecar only with one exact, unambiguous terminal
lineage and add a digest before consumption; a missing non-empty legacy note
cannot be reconstructed. The resolution-history fields above remain additive
and default to an empty list.

Before `phase1_formal_write_started`, a failed Phase 1 command may publish
`retry_intent_review` in the same crash-atomic `command_terminal_failure` row
and fsync. That row is both the Failure terminal and the sole retry IntentSpec
review authority, so recovery cannot observe a failure without its replacement
gate. Cancel waits for any admitted terminal writer, durably resolves an
embedded retry interaction as `cancel`, and only then acknowledges cancellation;
replay therefore cannot reopen that gate.

### SSE v1 compatibility window (DEFER-08)

v1 snapshot SSE remains supported through at least one successful public patch
release after wire v2 becomes the default and the built-in frontend/automation
clients have migrated. Physical removal of v1 is **not** part of plan-20260715;
see DEFER-08 / ADR-CODE-08. Removal preconditions (checklist; all required):

1. Built-in frontend migrated to v2 (W3-09 evidence): the SPA
   opens `GET /api/code/events?wire=2` from `sse-resilience` (`wrapClientForSseResilience`),
   reconnects with `cursor` from the wire, and treats `event: resync` /
   `WIRE_V2_RESYNC_REQUIRED` as one explicit snapshot pull (W2-15 UI). Cursor/seq
   are never invented client-side.
2. Built-in automation clients migrated to v2.
3. Compat / matrix tests consume v2 by default.
4. Release notes name the last v1-supporting version and the upgrade path.
5. At least one successful public patch release after (1)–(4) while v1 still works.


`GET /api/code/threads` returns `{ items, nextOffset? }`. Each item has `id`, optional `title`, `archived`, optional `currentIntentId`, optional `workingDir`, `createdAt`, and `updatedAt`. `workingDir` is omitted until ThreadProjection persists a per-thread cwd (do not invent the server cwd for linked-worktree threads). `limit` defaults to 50 and clamps to 200; malformed `limit` or `offset` returns `INVALID_QUERY_PARAM`.

`GET /api/code/skills?provider=<slug>&skill=<name>` returns curated A0-07 `{ items: [{ name, provider }] }`. An unknown `provider` slug returns `INVALID_SKILL_PROVIDER` (same contract as activate); omit `provider` to list all curated providers. `POST /api/code/skills/activate` accepts `{ provider, name }`; after discoverability validation it currently returns `SKILL_ACTIVATION_UNSUPPORTED` until an in-process provider activation path exists.

Code UI API errors use `{ error: { code, message } }`:

| Code | HTTP | Meaning |
|------|------|---------|
| `LOOPBACK_REQUIRED` | 403 | Non-loopback client attempted an API route. |
| `PAYLOAD_TOO_LARGE` | 413 | Write request body exceeded 256 KiB. |
| `ORIGIN_REQUIRED` | 403 | Browser write/attach lacked a trusted loopback `Origin` (or same-origin `Referer`), or presented a cross-site Origin. |
| `MISSING_BROWSER_BOOTSTRAP` | 403 | Browser attach lacked `X-Libra-Browser-Bootstrap` for a session that minted a bootstrap secret. |
| `INVALID_BROWSER_BOOTSTRAP` | 403 | `X-Libra-Browser-Bootstrap` does not match this Libra Code session. |
| `RATE_LIMITED` | 429 | Per-session write budget exhausted; retry after the rate-limit window (see `Retry-After` / wait for window recovery). |
| `REDACTION_FAILED` | 500 | Session / diagnostics / SSE projection could not apply the secret redactor (empty rules or serialize failure). Fail closed: the response omits unredacted payload; restart `libra code` or retry after fixing redactor configuration. |
| `INVALID_WIRE_VERSION` | 400 | `GET /api/code/events` wire negotiation received an illegal `wire` / `libra-wire` value (only `1`/`v1` and `2`/`v2` are accepted). |
| `WIRE_V2_REQUIRES_DURABLE_SESSION` | 503 | SSE wire v2 requires a SessionStore-backed workflow hub (mounted today for default Web headless persistence; managed Codex Web does not yet expose one). |
| `WIRE_V2_CURSOR_AHEAD` | 409 | `?cursor=` is ahead of the durable workflow tail; drop the cursor and resync (an ahead cursor would permanently skip live events). |
| `WIRE_V2_RESYNC_REQUIRED` | SSE `resync` then close | Transport backlog exceeded (1,024 events / 8 MiB); fetch snapshot and reconnect with `cursor=<durableTail>`. |
| `WIRE_V2_REPLAY_FAILED` | 500 | Wire v2 could not replay durable workflow events after the requested cursor (gap or I/O; capacity exits use `WIRE_V2_RESYNC_REQUIRED`). |
| `CONTROL_DISABLED` | 403 | Automation control is not enabled for this process. |
| `MISSING_CONTROL_TOKEN` | 403 | Automation control token is absent. |
| `INVALID_CONTROL_TOKEN` | 403 | Automation control token is invalid. |
| `MISSING_CONTROLLER_TOKEN` | 403 | Lease token is absent for a write route. |
| `INVALID_CONTROLLER_TOKEN` | 403 | Lease token is invalid or stale for a write route. |
| `INVALID_CONTROLLER_KIND` | 400 | Controller attach requested an unsupported kind. |
| `CONTROLLER_CONFLICT` | 409 | Another live controller owns the lease, or the session is busy. |
| `INTERACTION_NOT_ACTIVE` | 409 | Respond targeted an interaction with no active runtime turn. |
| `PHASE1_WORKSPACE_CHANGED` | 409 | Execute's exact checkout identity/content no longer matches the reviewed Plan, or Libra cannot verify/recapture it. A metadata-only `workspaceWarning` may pass that exact recheck and does not itself produce this error. No mutation starts and stale Execute preserves the pending gate. Modify may regenerate for a new HEAD in the same repository; a different repository requires Cancel and a new request. Failed recapture does not consume the note. |
| `PLAN_EXECUTION_NOT_AVAILABLE` | 409 | Historical Web 409 while confirmed-plan execution was unwired. After W2-04, Network Allow admits execution onto the serialized runtime queue instead of producing this code. Older clients may still decode the catalogued 409. |
| `SESSION_BUSY` | 409 | Submit while a turn is already running, cancel with no turn in flight, or send a fresh explicit-direct command while IntentSpec revision authority is active. Exact terminal retries and matching live `commandId` retries are idempotent acknowledgements and append nothing. |
| `BROWSER_CONTROL_DISABLED` | 403 | Browser write control is disabled. |
| `AUTOMATION_CONTROLLER_REQUIRED` | 403 | An automation-only path was called with a non-automation lease. |
| `CODE_UI_UNAVAILABLE` | 404 | No active `libra code` session is attached to the web server. |
| `INVALID_QUERY_PARAM` | 400 | Query or interaction-response validation failed, including `/threads` pagination and an IntentSpec Modify note above the 16 KiB (16,384 UTF-8 byte) limit. Oversized plain or `/intent modify` input is rejected before Claiming, Runtime admission, or any workflow append. |
| `PLAN_REVISION_NOTE_REQUIRED` | 400 | After Plan Modify, the next plain-text message was empty or whitespace-only. Revision authority remains pending and unconsumed; send a non-empty change description or Cancel. |
| `INVALID_COMMAND_ID` | 400 | `commandId` was empty, too long, or contained whitespace/control characters. |
| `STORAGE_PATH_INVALID` | 500 | Storage-root resolution failed. |
| `STORAGE_ROOT_UNRESOLVED` | 500 | Repository storage root could not be resolved. |
| `STATUS_UNAVAILABLE` | 500 | Runtime status snapshot is unavailable. |
| `THREAD_LIST_FAILED` | 500 | Thread projection enumeration failed. |
| `DB_UNAVAILABLE` | 500 | Session database is offline. |
| `USAGE_UNAVAILABLE` | 500 | Durable runtime usage could not be queried. |
| `THREAD_GRAPH_INVALID_ID` | 400 | `GET /api/code/thread-graph` received a non-UUID `threadId`. |
| `THREAD_GRAPH_STORAGE_UNAVAILABLE` | 500 | Repository storage root could not be resolved for the thread graph. |
| `THREAD_GRAPH_NOT_FOUND` | 404 | No indexed thread projection exists for the requested `threadId`. |
| `THREAD_GRAPH_UNAVAILABLE` | 500 | Indexed thread graph could not be loaded (database, projection, or overlay failure). |
| `INVALID_SKILL_PROVIDER` | 400 | The requested skill provider is not an A0-07 agent slug. |
| `SKILL_NOT_DISCOVERABLE` | 400 | The requested skill is not curated for that provider. |
| `SKILL_ACTIVATION_UNSUPPORTED` | 422 | Skill is discoverable, but in-process activation is not available yet. |
| `SESSION_RESUME_BUSY` | 409 | A thinking or tool-running session cannot be replaced. |
| `SESSION_RESUME_NOT_FOUND` | 404 | No matching session exists under this working directory. |
| `SESSION_RESUME_REQUIRES_RESTART` | 422 | Target thread is loadable, but in-process AgentRuntime swap is not available; restart with `libra code --resume <threadId>`. |
| `SESSION_RESUME_LOAD_FAILED` | 500 | Target thread exists but session storage/checkpoint could not be loaded or folded. |
| `RECONCILIATION_REQUIRED` | 409 | A mutating turn needs manual reconciliation before another turn can run. |
| `COMMAND_PAYLOAD_CONFLICT` | 409 | The same `commandId` was reused with a different message payload. |
| `COMMAND_ALREADY_TERMINAL` | 409 | The same `commandId` already finished failed/cancelled/indeterminate; allocate a new `commandId` to retry. |
| `PLAN_REPAIR_RETRY_LIMIT_REACHED` | 409 | A plan-repair Continue request did not raise the exhausted automatic retry cap. Retry with a higher `maxAttempts` (for example, `{ "selectedOption": "continue", "maxAttempts": 3 }`), provide manual revision guidance, or cancel the repair. |
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
# Start a Web Code UI session with default Gemini provider
libra code

# Start with Anthropic Claude
libra code --provider anthropic --model claude-sonnet-4-20250514

# Bind the Web UI on all interfaces; remote browsers see a loopback-only notice
# (explicit --browser-control off: default Web is loopback and rejects non-loopback hosts)
libra code --port 8080 --host 0.0.0.0 --browser-control off

# Remote humans should SSH port-forward to the bound loopback port
# ssh -L 8080:127.0.0.1:8080 user@host
# then browse http://127.0.0.1:8080 locally

# Browser-driven session against a local Ollama (browser write lease is on by default)
libra code --provider ollama --port 4400

# Managed Codex on the default Web path (browser write lease is loopback by default)
libra code --provider codex

# Enable local automation write control (writes token + lease discovery files)
libra code --control write

# Drive an existing write-control session over JSON-RPC NDJSON (client-only).
# Defaults read `.libra/code/control.json` + sibling `control-token`.
libra code --control stdio

# Explicit endpoint overrides (still loopback-only)
libra code --control stdio \
  --control-url http://127.0.0.1:3000 \
  --control-token-file .libra/code/control-token

# Load provider keys from a dotenv-style file (overrides stale shell env vars)
libra code --env-file .env.test

# Deprecated MCP-only legacy (tools/resources; not turn control).
# Prefer --control stdio for automation; dedicated `libra mcp --stdio` after W5.
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

# Capture provider diagnostics while using a local Ollama model
LIBRA_LOG='libra::internal::ai=debug' \
LIBRA_LOG_FILE=/tmp/libra-code.log \
libra code --repo=/Volumes/Data/linked --provider ollama --model gemma4:31b

# Resume a canonical Libra thread with a non-Codex Web session
libra code --resume 11111111-1111-4111-8111-111111111111
libra code --provider ollama --resume 11111111-1111-4111-8111-111111111111

# Inspect the same thread's version graph
libra graph --json 11111111-1111-4111-8111-111111111111

# Inspect a thread graph from outside that repository
libra graph --json 11111111-1111-4111-8111-111111111111 --repo /Volumes/Data/linked

# Start in code review context with strict approval
libra code --context review --approval-policy untrusted

# Use Codex with plan-before-execute mode
libra code --provider codex --plan-mode
```

## Human Output

Output is delivered through the Web UI or MCP depending on the mode. Web mode prints URL / control details on stdout and stays resident until SIGINT/SIGTERM. In the generic provider workflow, a normal plain-text request starts the plan workflow automatically; explicit slash commands keep their command-specific behavior. Generic provider planning uses a two-step review: the LLM first drafts an IntentSpec for confirmation, then the confirmed IntentSpec is sent back to the LLM to generate a reviewable execution plan before any execution starts. Modify waits for the next plain-text revision note and then presents a replacement plan. Execute proceeds only to the mandatory network-policy gate. Deny and Back are available; Network Allow admits confirmed plan execution onto the serialized runtime queue. Mutating tools still require approval/sandbox/ACL, and classified failures enter the W2-11 repair loop. The web server serves an embedded Next.js application. The stdio mode communicates via JSON-RPC messages following the Model Context Protocol.

## Diagnostics

`libra code` supports tracing through `RUST_LOG` or `LIBRA_LOG`; when both are set, `LIBRA_LOG` takes precedence. Set `LIBRA_LOG_FILE=<path>` to write diagnostics to a plain log file. When `LIBRA_LOG_FILE` is set without an explicit log filter, Libra defaults to `libra=debug`.

For Ollama provider failures, useful diagnostics are:

```bash
mkdir -p /tmp/libra-logs
LIBRA_LOG='libra::internal::ai=debug' \
LIBRA_LOG_FILE=/tmp/libra-logs/libra-code-ollama.log \
libra code --repo=/Volumes/Data/linked --provider ollama --model gemma4:31b
```

If the session reports an Ollama 503, also capture the local server state:

```bash
ollama ps >> /tmp/libra-logs/libra-code-ollama.log
ollama list >> /tmp/libra-logs/libra-code-ollama.log
```

## Design Rationale

### Why a Web Code UI?

The Web Code UI is the primary (and only interactive) collaborative surface. The legacy TUI and its bare `--provider codex --resume` resume driver were removed in the W5 breaking release (W5-06); the deprecated `--web` / `--web-only` aliases and the `LIBRA_CODE_LEGACY_TUI` rollback env were removed earlier in the same release (W5-07).

### Why multiple AI provider support?

Different providers excel at different tasks and have different cost/latency profiles. Gemini is the default for its generous free tier and fast response times. Anthropic Claude excels at careful reasoning and code review. Local Ollama support enables fully offline development. By abstracting behind a `CompletionClient` trait, adding a new provider requires only implementing the trait without touching the session, tool, or web UI layers.

### Why MCP integration?

The Model Context Protocol (MCP) is an open standard for connecting AI clients to tool servers. Deprecated `libra code --stdio` still lets Libra act as an MCP tool/resource server for clients like Claude Desktop (tools/resources only — not live Code turn control). A dedicated `libra mcp --stdio` is planned after W5 (DEFER-02); until then this legacy entry prints a deprecation warning. Prefer `libra code --control stdio` for local automation against a write-control Web session. Libra exposes an allowlisted `run_libra_vcs` tool for version-control operations -- `status`, `diff`, `branch`, `log`, `show`, `show-ref`, `ls-files`, `add`, `commit`, and `switch` -- so external AI agents use Libra directly instead of invoking Git. `run_libra_vcs` only accepts those Libra subcommands; it is not a Git-compatible shell. For repository state inspection, prefer `status --json` or `status --porcelain v2 --untracked-files=all`, and use `ls-files` for tracked and untracked repository path inspection (for example `ls-files --others --exclude-standard` for ignore-aware untracked files). Libra-managed execution also rejects direct `git` shell commands.

### Why approval policies?

AI agents executing shell commands on a developer's machine present real safety risks. The five-tier approval system balances productivity with control:
- `never` is for fully locked-down environments where the agent can only read.
- `allow-all` is the opposite extreme: no prompts and every command runs, for trusted throwaway or sandboxed environments where friction outweighs risk.
- `on-failure` lets the agent try sandboxed execution and only asks when it fails.
- `on-request` (default) sandboxes everything and escalates when the agent or sandbox policy requires it.
- `untrusted` is the most conservative interactive mode, prompting for anything beyond known-safe reads.

Always approvals already stored in `approved_permission` are keyed by repository identity and remain visible to every worktree of that repository. Session/TTL memos live only in the in-memory cache for the current controller lease and are dropped on lease takeover, detach, or expiry — including when a browser or automation client first takes control from the previous controller.

### Why session persistence and resume?

Long coding sessions accumulate significant context: file edits, conversation history, tool outputs. Losing this context on an accidental terminal close is painful. Session persistence stores the full conversation and tool state, and `--resume <thread_id>` restores a canonical Libra thread.

The embedded Code UI exposes the same canonical identifier as `threadId` in its session snapshot. Older `session_id` fields remain present for compatibility, but new integrations should key resume, Web, MCP, and diagnostics flows by `threadId`.

For a persistent non-Codex Web session, the initial session write is a prerequisite for starting a turn: if it fails, Libra starts no turn and the browser can repair storage and retry. Approval and user-input responses are likewise checkpointed before their continuation is released; if that checkpoint fails, the original interaction remains pending, no approved action has started, and the same response can be retried after repairing storage. A persistence failure after a response or side-effect boundary has been consumed changes the live session to `indeterminate_side_effect` and blocks further submits or interaction replies; inspect the durable session data before restarting or reconciling it.

On `Ctrl-C` or `SIGTERM`, a non-Codex headless or web-only process closes browser command admission, then runs the shared process lifecycle shutdown owner (runtime/listeners/managed child/control) under one deadline. Read-only/model work is cooperatively cancelled; a started mutating tool is allowed to finish within that budget. If the deadline expires, `libra code` exits with an explicit shutdown failure and requires session inspection and reconciliation before restart. Supervisors should prefer `SIGTERM` (or `Ctrl-C` / `SIGINT`) over `SIGKILL` so ports, leases, and child processes are released cleanly.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Interactive AI session | `libra code` | Not available | Not available |
| Web mode | Default (only interactive mode; `--web`/`--web-only` aliases removed in W5-07, legacy TUI removed in W5-06) | Not available | Not available |
| MCP/stdio mode | `--stdio` | Not available | Not available |
| AI provider selection | `--provider` | Not available | Not available |
| Session resume | `--resume <thread_id>` (Web, non-Codex; Web Codex rejects `--resume`, bare codex+resume fails with a usage error) | Not available | Not available |
| Tool approval policy | `--approval-policy` | Not available | Not available |

Note: Neither Git nor jj have an equivalent to `libra code`. This command represents Libra's core differentiation as an AI-agent-native version control system. The closest analogs in the Git ecosystem are third-party tools like GitHub Copilot CLI or aider, which are separate applications rather than integrated VCS commands.

## Error Handling

| Scenario | Behavior | Exit |
|----------|----------|------|
| `--web` / `--web-only` specified | Removed in W5-07: clap unexpected-argument usage error plus a migration hint (`libra code` already defaults to the Web Code UI) | non-zero |
| Bare `--provider codex --resume <thread_id>` | Removed in W5-06: clap usage error plus a migration hint (legacy TUI resume driver removed; managed Codex Web resume has not landed) | non-zero |
| Missing API key for selected provider | Fatal error with provider name and expected env var | non-zero |
| Port already in use | Fatal error naming `host:port` and instructing an explicit `--port` (no auto-scan) | non-zero |
| `--network-access allow` | Usage error in every mode until the Plan network-policy gate owns per-execution sandbox network | non-zero |
| Thread ID not found on resume | Fatal error with canonical `thread_id` | non-zero |
| `--control write --stdio` | Usage error; MCP `--stdio` (tools/resources) and `--control stdio` automation are separate modes | non-zero |
| `--control write --host 0.0.0.0` or other non-loopback host | Usage error; write control is loopback-only | non-zero |
| Another live `--control write` owns the same control lock | `CONTROL_INSTANCE_CONFLICT` with existing PID/URL when available | non-zero |
| Control token file is a symlink, non-regular file, or not `0600` on Unix/macOS | Fatal setup error before the web server starts | non-zero |
