# `libra code`

启动带 TUI、Web 或 MCP 模式的交互式 AI 编码会话。

## 概要

```
libra code
libra code --web-only [-p <PORT>] [--host <HOST>]
libra code --stdio
libra code --provider <PROVIDER> [--model <MODEL>]
libra code --resume <THREAD_ID>
libra graph <THREAD_ID> [--repo <PATH>]
```

## 说明

`libra code` 启动一个交互式编码会话，让人类开发者与 AI agent 协作。默认模式会启动基于 ratatui/crossterm 的终端 UI（TUI），并带有后台 Web 服务器。Generic provider TUI 中的普通开发请求会先路由到内置 planning workflow：Libra 生成可审阅的 IntentSpec 和执行计划，然后在运行 mutating tools 之前等待 Execute Plan / Network / Modify Plan / Cancel。另有两种替代模式：`--web-only` 只运行 Web 服务器而不启动 TUI（适合浏览器访问或远程托管），`--stdio` 通过标准输入/输出运行 MCP server，用于与 Claude Desktop 等 AI clients 集成。

该命令支持八种 AI provider 后端（Gemini、OpenAI、Anthropic、DeepSeek、Kimi、Zhipu、Ollama、Codex），以及三种运行上下文（dev、review、research），用于针对不同工作流调节 agent 行为。会话可以通过 Libra 规范的 `--resume <thread_id>` 流程持久化和恢复。

沙箱化工具执行层会强制 approval policies，控制 agent 何时可以运行 shell 命令、应用补丁、Web 搜索或执行其他可能破坏性的操作。TUI dev 会话默认使用 workspace-write 执行且禁止网络访问。执行计划就绪后，Plan review 对话框包含 `Network: Deny` / `Network: Allow` toggle；选中的值会成为执行 `IntentSpec` 的网络策略，用于 shell、gate 和 `web_search`。Review 和 research 上下文保持只读，且不授予网络访问。

当 TUI 退出且 Libra 能推导出规范 thread ID 时，`libra code` 会打印后续 `libra graph <thread_id>` 命令，以便在独立 TUI 中检查该线程的 Intent/Plan/Task/Run/PatchSet 版本图。检查非当前目录仓库时，使用 `libra graph <thread_id> --repo <path>`。

## 选项

| 标志 | 短参数 | 长参数 | 默认值 | 说明 |
|------|--------|--------|--------|------|
| Web only | | `--web-only` / `--web` | off | 只运行 Web 服务器，不运行 TUI。与 `--stdio` 冲突。 |
| Port | `-p` | `--port` | `3000` | Web 服务器监听端口。 |
| Host | | `--host` | `127.0.0.1` | Web 服务器 bind 地址。 |
| Working directory | | `--cwd` | 当前目录 | 会话工作目录。 |
| Env file | | `--env-file <PATH>` | 无 | 从 dotenv 风格文件加载 provider 环境变量；显式文件值优先于 Vault 和进程环境。 |
| Control mode | | `--control <observe\|write>` | `observe` | 本地自动化控制模式。`observe` 保留现有 loopback 读行为；`write` 启用本地 token discovery 和进程级自动化控制认证。 |
| Control token file | | `--control-token-file <PATH>` | `.libra/code/control-token` | 每进程本地自动化 token 路径。在 `write` 模式下，Unix/macOS 文件必须是权限 `0600` 的普通文件。 |
| Control info file | | `--control-info-file <PATH>` | `.libra/code/control.json` | 非 secret 本地 endpoint discovery 元数据路径。该文件永不包含 token 材料。 |
| Provider | | `--provider` | `gemini` | AI provider 后端（见下方 Provider Backends）。 |
| Model | | `--model` | provider 默认值 | Provider 专用 model ID。 |
| Temperature | | `--temperature` | provider 默认值 | 生成采样 temperature。 |
| Ollama thinking | | `--ollama-thinking` / `--thinking` | `OLLAMA_THINK`，然后 `off` | Ollama thinking 模式：`auto`、`off`、`on`、`low`、`medium` 或 `high`。 |
| Ollama compact tools | | `--ollama-compact-tools` | `OLLAMA_COMPACT_TOOLS`，然后 off | 为拒绝复杂 JSON schemas 的远程/云 Ollama endpoint 发送紧凑 tool schemas。 |
| DeepSeek thinking | | `--deepseek-thinking <enabled\|disabled>` | 省略 | 使用 `--provider deepseek` 时发送 DeepSeek 的 `thinking` 对象。 |
| DeepSeek reasoning effort | | `--deepseek-reasoning-effort <low\|medium\|high\|max>` | 省略 | 使用 `--provider deepseek` 时发送 DeepSeek 的 `reasoning_effort` 值；`xhigh` 作为 `max` 的别名被接受。 |
| DeepSeek stream | | `--deepseek-stream <true\|false>` / `--stream <true\|false>` | `false` | 使用 `--provider deepseek` 时发送 DeepSeek 的 `stream` boolean。 |
| Kimi thinking | | `--kimi-thinking <enabled\|disabled>` | model 默认值 | 使用 `--provider kimi` 时发送 Kimi 的 `thinking` 对象。 |
| Context | | `--context` | 无 | 运行上下文：`dev`（别名 `development`）、`review`（别名 `code-review`）、`research`（别名 `explore`）。 |
| Resume | | `--resume <THREAD_ID>` | 无 | 按 thread ID 恢复规范 Libra 线程。 |
| Approval policy | | `--approval-policy` | `on-request` | 工具审批策略（见下方 Approval Policies）。 |
| Network access | | `--network-access <allow\|deny>` | `deny` | shell 和 gate 执行的默认 TUI 网络策略；仍可在 Plan review 时切换。 |
| MCP port | | `--mcp-port` | `6789` | MCP server 监听端口。 |
| Stdio | | `--stdio` / `--mcp-stdio` | off | 通过 stdio 运行 MCP。与 `--web-only` 冲突。 |
| API base | | `--api-base` | provider 默认值 | Provider API base URL 覆盖。 |
| Codex binary | | `--codex-bin` | `codex` | Codex 可执行文件路径。 |
| Codex port | | `--codex-port` | 随机 | 覆盖 Codex app-server 端口。 |
| Plan mode | | `--plan-mode` | off | 要求 agent 在执行前生成计划（Codex 模式）。 |
| Browser control | | `--browser-control <off\|loopback>` | provider-aware（见 Web Browser Control） | `/api/code/controller/attach` 浏览器租约姿态。与 `--stdio` 冲突；`loopback` 要求 loopback `--host`。 |

### Provider Backends

| 值 | 说明 | API Key Env | Base URL 覆盖 |
|----|------|-------------|---------------|
| `gemini` | Google Gemini（默认：gemini-2.5-flash） | `GEMINI_API_KEY` | `--api-base` |
| `openai` | OpenAI（默认：gpt-4o-mini） | `OPENAI_API_KEY` | `--api-base`、`OPENAI_BASE_URL` |
| `anthropic` | Anthropic（默认：claude-3.5-sonnet） | `ANTHROPIC_API_KEY` | `--api-base`、`ANTHROPIC_BASE_URL` |
| `deepseek` | DeepSeek | `DEEPSEEK_API_KEY` | `--api-base` |
| `kimi` | Moonshot AI Kimi（默认：kimi-k2.6） | `MOONSHOT_API_KEY` | `--api-base`、`MOONSHOT_BASE_URL`、`--kimi-thinking` |
| `zhipu` | Zhipu GLM（默认：glm-5） | `ZHIPU_API_KEY` | `--api-base`、`ZHIPU_BASE_URL` |
| `ollama` | Ollama（本地模型和直接 Cloud API） | 直接 Cloud API 使用 `OLLAMA_API_KEY` | `OLLAMA_BASE_URL`、`OLLAMA_THINK`、`OLLAMA_COMPACT_TOOLS`、`--api-base`、`--ollama-thinking` 或 `--ollama-compact-tools` |
| `codex` | Codex app-server | -- | `--codex-bin` / `--codex-port` |

关于 Codex app-server 连接、model forwarding、credentials ownership 和持久化对象存储细节，见 [Codex data storage integration](codex-data-storage.md)。

DeepSeek 请求可以通过 `--deepseek-thinking enabled --deepseek-reasoning-effort high --deepseek-stream true` 选择加入 provider 专用字段；这些标志会对非 DeepSeek provider 拒绝。
Kimi 请求默认使用所选 model 的 thinking 行为；对于需要更低延迟或官方 Web 搜索兼容性的 K2.6/K2.5 run，使用 `--kimi-thinking disabled`。当 provider 返回 Kimi `reasoning_content` 时，Libra 会在 tool-call turns 中保留它。
常规运行时，将 provider keys 存在 `vault.env.<NAME>` 中；Libra 先检查 repo-local Vault，再检查 global Vault，最后检查进程环境。对需要显式 dotenv 覆盖的 live tests，使用 `--env-file .env.test`。

Ollama 请求默认流式读取 `/api/chat` 响应，并向 debug logs 添加每请求 `request_id`。它们也默认使用 `think:false`，避免具备 reasoning 能力的本地模型在 tool calls 前花数分钟生成隐藏 reasoning。单次运行使用 `--ollama-thinking high`，或将 `OLLAMA_THINK=true`、`low`、`medium`、`high` 或 `auto` 设为环境默认值。`auto` 会省略 `think` 字段并让 Ollama 决定。当远程/云 Ollama endpoint 接受简单 tools 但对 Libra 完整 tool schema payload 返回 503 时，使用 `--ollama-compact-tools` 或 `OLLAMA_COMPACT_TOOLS=true`。

### 本地自动化控制

`libra code --control observe` 是默认值，除非显式提供 `--control-info-file`，否则不会创建本地控制文件。Loopback clients 可以继续无 token 读取 `/api/code/session` 和 `/api/code/events`。

`libra code --control write` 启用本地自动化安全信封。Libra 会在 `.libra/code/control-token` 中创建新的 32-byte token，在 Web 服务器绑定后将非 secret endpoint 元数据写入 `.libra/code/control.json`，并在进程生命周期内持有 `.libra/code/control.lock`。`control.json` 包含 `version`、`mode`、`pid`、`baseUrl`、可选 `mcpUrl`、`workingDir`、可选 `threadId` 和 `startedAt`；它永不包含 token、token hash、token path、provider credentials、headers 或 provider request/response bodies。

Write control 仅限本地。`--control write` 与 `--stdio` 组合会被拒绝，并要求 `--host` 是 loopback（`127.0.0.1`、`::1` 或 `localhost`）。使用相同默认路径启动第二个 write-control 实例会以 `CONTROL_INSTANCE_CONFLICT` 快速失败；只有调用方有意管理多个本地实例时，才使用不同的 `--control-token-file` 和 `--control-info-file` 路径。

Automation clients 使用 `POST /api/code/controller/attach` 连接，请求体 `{ "clientId": "...", "kind": "automation" }`，header `X-Libra-Control-Token`，然后使用返回的 `X-Code-Controller-Token` 写入。Automation-held leases 对 `/api/code/messages`、`/api/code/interactions/{id}`、`/api/code/controller/detach` 和 `/api/code/control/cancel` 同时要求两个 tokens。本地 TUI 可以用 `/control reclaim` 重新取得控制权，这会使 automation lease 失效。Code UI 写请求体上限为 256KiB。

`GET /api/code/diagnostics` 返回为本地工具准备的脱敏 observe-only 状态摘要。Control attach、detach、submit、respond 和 cancel 操作会通过 runtime audit sink 发出 `local-tui-control/v1` audit events。Stdio automation clients 使用 [`libra code-control --stdio`](code-control.md)；`libra code --stdio` 仍是 MCP stdio server，不控制 live TUI。

### Web Browser Control

`--browser-control <off|loopback>` 控制嵌入式 UI 的基于租约的写表面是否可用。默认值感知模式：

| 入口 | 默认 `--browser-control` |
|------|--------------------------|
| TUI 会话（`libra code` 且不带 `--web-only`） | `off` |
| `libra code --web-only --provider codex` | `loopback` |
| 使用任意其他 provider 的 `libra code --web-only` | `off` |

当 `--host` 不是 loopback 地址时，选择 `loopback` 会被拒绝；该标志也与 `--stdio` 冲突。浏览器服务端 endpoints 在 `code_router()` audit matrix（`src/internal/ai/web/mod.rs`）中标记：

- `/session`、`/events`、`/diagnostics`、`/threads`、`/repo`、`/repo/status` — 仅 loopback observe。
- `/controller/attach` — loopback。`kind: "automation"` 请求还要求 `X-Libra-Control-Token`。handler **签发** lease 的 `controllerToken`（不期待调用方发送它）。
- `/controller/detach`、`/messages`、`/interactions/{id}` — loopback + `X-Code-Controller-Token`；`Automation` leases 还要求 `X-Libra-Control-Token`。
- `/control/cancel` — loopback + `X-Code-Controller-Token`。`Automation` leases 也要求 `X-Libra-Control-Token`；这是与 TUI `Esc` cancel 路径的唯一区别。

浏览器写请求共享与自动化控制相同的 256 KiB body limit 和 audit-sink wiring。浏览器只在内存中持久化 lease；重新加载页面会丢弃 lease，下一次写入会重新 attach。

当服务器绑定到非 loopback host 时，非 loopback 浏览器的 HTML navigation 会收到静态 remote access notice，而不是 SPA。该 notice 零 JavaScript，只包含 bind/remote/version/commit 占位符；asset/API fallback 返回 404，使远程 clients 无法探测 session state。

请求 `--browser-control loopback` 且浏览器持有 active lease 时，TUI 初始 controller 是 `LocalTui`（可见 owner，可 reclaim），而不是 `Fixed { Tui }`（永久阻塞）。如果 TUI 也想驱动写入，必须同时提供 `--control write` 和 `--browser-control loopback`；两个 writer 通过同一个 `TuiControlCommand` channel 串行化。

对于 `--web-only` 非 Codex providers（`--provider ollama` 是规范 Phase 3 验证路径），Libra 构建 [`HeadlessCodeRuntime`](../../../src/internal/ai/web/headless.rs)，直接运行 agent 的 tool loop，使浏览器可以驱动真实会话，无需终端。Headless 模式公布 `messageInput`、`streamingText`、`toolCalls`、`planUpdates`、`patchsets`、`interactiveApprovals`、`structuredQuestions` 和 `providerSessionResume`；`--resume <thread_id>` 会为相同工作目录恢复持久化 transcript/history。`update_plan` 投影到 `plans[]`，`apply_patch` metadata 投影到 `patchsets[]`。

### Code UI Wire Contract

Code UI JSON contract 使用 camelCase 字段名和 snake_case 枚举值。Rust 事实来源是 `src/internal/ai/web/code_ui.rs`；浏览器镜像是 `web/src/lib/code-ui/types.ts`；`tests/ai_code_ui_wire_test.rs` 固定 wire shape。

`GET /api/code/session` 返回 `CodeUiSessionSnapshot`：

| 字段 | 类型 | 契约 |
|------|------|------|
| `sessionId` | string | 为兼容性保留的 runtime session identifier。 |
| `threadId` | string, optional | 规范的持久化 Libra thread ID；存在时，resume、graph、Web、MCP 和 diagnostics 流程应优先使用它。 |
| `workingDir` | string | 会话工作目录。 |
| `provider` | object | `{ provider, model?, mode?, managed }`。 |
| `capabilities` | object | 八个 booleans：`messageInput`、`streamingText`、`planUpdates`、`toolCalls`、`patchsets`、`interactiveApprovals`、`structuredQuestions`、`providerSessionResume`。 |
| `controller` | object | `{ kind, ownerLabel?, canWrite, leaseExpiresAt?, reason?, loopbackOnly }`；`kind` 是 `none`、`browser`、`automation`、`tui` 或 `cli`。 |
| `status` | string | `idle`、`thinking`、`executing_tool`、`awaiting_interaction`、`completed` 或 `error`。 |
| `transcript` | array | 带 `id`、`kind`、可选 `title` / `content` / `status`、`streaming`、`metadata`、`createdAt`、`updatedAt` 的条目。 |
| `plans` / `tasks` / `toolCalls` / `patchsets` | arrays | Workflow、Summary、Diff 和 Terminal panes 使用的 runtime projections。 |
| `interactions` | array | 待处理/已解决的 UI prompts。`kind` 是 `approval`、`sandbox_approval`、`request_user_input`、`intent_review_choice` 或 `post_plan_choice`。 |
| `updatedAt` | string | ISO 8601 更新时间戳。 |

`GET /api/code/events` 流式传输 `CodeUiEventEnvelope` 记录，包含 `seq`、`type`、`at` 和 `data`。事件 `type` 是 `session_updated`、`status_changed` 或 `controller_changed`；`session_updated` 携带完整 `CodeUiSessionSnapshot`。

`GET /api/code/threads` 返回 `{ items, nextOffset? }`。每个 item 有 `id`、可选 `title`、`archived`、可选 `currentIntentId`、`createdAt` 和 `updatedAt`。`limit` 默认 50 并 clamp 到 200；格式错误的 `limit` 或 `offset` 返回 `INVALID_QUERY_PARAM`。

Code UI API 错误使用 `{ error: { code, message } }`：

| Code | HTTP | 含义 |
|------|------|------|
| `LOOPBACK_REQUIRED` | 403 | 非 loopback client 试图访问 API route。 |
| `PAYLOAD_TOO_LARGE` | 413 | 写请求体超过 256 KiB。 |
| `CONTROL_DISABLED` | 403 | 当前进程未启用 automation control。 |
| `MISSING_CONTROL_TOKEN` / `INVALID_CONTROL_TOKEN` | 403 | Automation control token 缺失或无效。 |
| `MISSING_CONTROLLER_TOKEN` / `INVALID_CONTROLLER_TOKEN` | 403 | Lease token 对写路由缺失或无效。 |
| `INVALID_CONTROLLER_KIND` | 400 | Controller attach 请求了不支持的 kind。 |
| `CONTROLLER_CONFLICT` | 409 | 另一个 live controller 拥有 lease，或会话正忙。 |
| `BROWSER_CONTROL_DISABLED` | 403 | 浏览器写控制已禁用。 |
| `AUTOMATION_CONTROLLER_REQUIRED` | 403 | 用非 automation lease 调用了 automation-only 路径。 |
| `CODE_UI_UNAVAILABLE` | 404 | 没有 active `libra code` session 附加到 Web 服务器。 |
| `INVALID_QUERY_PARAM` | 400 | 查询解析失败，目前用于 `/threads` 分页。 |
| `STORAGE_PATH_INVALID` / `STATUS_UNAVAILABLE` / `THREAD_LIST_FAILED` / `DB_UNAVAILABLE` / `INTERNAL_ERROR` | 500 | 服务端 storage、status、projection、database 或 fallback internal failure。 |
| `UNSUPPORTED_OPERATION` | 422 | Runtime 拒绝尚不支持的请求操作。 |

### Web Search

`web_search` 工具要求会话网络策略允许 outbound access。如果 `BRAVE_SEARCH_API_KEY` 可从 `vault.env.BRAVE_SEARCH_API_KEY` 或进程环境获得，Libra 会先尝试 Brave Search API，并返回结果标题、URL 和 snippets。如果 Brave 未配置或请求失败，Libra 会回退到零配置 DuckDuckGo HTML endpoint。

### Approval Policies

| 值 | 别名 | 说明 |
|----|------|------|
| `never` | -- | 不提示；危险命令直接拒绝。 |
| `on-failure` | `on-failure` | 仅在沙箱拒绝后重试时提示。 |
| `on-request` | `on-request` | 默认在沙箱内运行；当升级或策略需要时提示（默认）。 |
| `untrusted` | `unless-trusted`、`untrusted` | 对非 trusted 操作提示；已知安全读取自动允许。 |

### Context Modes

| 值 | 别名 | 说明 |
|----|------|------|
| `dev` | `development` | 常规开发工作流。 |
| `review` | `code-review` | 聚焦代码审查。 |
| `research` | `explore` | 探索式研究和分析。 |

## 常用命令

```bash
# 使用默认 Gemini provider 启动 TUI 会话
libra code

# 使用 Anthropic Claude 启动
libra code --provider anthropic --model claude-sonnet-4-20250514

# 只启动 Web 并绑定所有接口；远程浏览器会看到 loopback-only notice
libra code --web-only --port 8080 --host 0.0.0.0

# 浏览器驱动的本地 Ollama 会话
libra code --web-only --provider ollama --port 4400

# 允许浏览器通过 loopback 写控制（Codex web-only 默认是 loopback）
libra code --web-only --provider codex --browser-control loopback

# 启用本地自动化写控制（写入 token + lease discovery 文件）
libra code --control write

# 从 dotenv 风格文件加载 provider keys（覆盖陈旧 shell env vars）
libra code --env-file .env.test

# 为 Claude Desktop 集成通过 stdio 运行 MCP
libra code --stdio

# 使用启用 reasoning 的 DeepSeek
libra code --provider deepseek --model deepseek-v4-pro --deepseek-thinking enabled --deepseek-reasoning-effort high --deepseek-stream true
libra code --env-file .env.test --provider deepseek --model deepseek-v4-pro --deepseek-thinking enabled --deepseek-reasoning-effort high --deepseek-stream true

# 使用 Kimi（Moonshot AI）和 K2.6 默认值；为了降低延迟可关闭 thinking
libra code --provider kimi
libra code --provider kimi --model kimi-k2-thinking --kimi-thinking enabled
libra code --provider kimi --model kimi-k2.6 --kimi-thinking disabled

# 使用本地 Ollama 模型；普通请求会先生成可审阅计划
libra code --provider ollama --model llama3 --api-base http://127.0.0.1:11434/v1

# 为远程/云 Ollama endpoint 使用紧凑 tool schemas
libra code --provider ollama --model minimax-m2.7:cloud --api-base http://192.168.0.5:11434/v1 --ollama-compact-tools

# 为一次 Ollama 运行启用 high thinking
libra code --provider ollama --model qwen3.6 --ollama-thinking high

# 使用本地 Ollama 模型时捕获 provider/TUI diagnostics
LIBRA_LOG='libra::internal::ai=debug,libra::internal::tui=debug' \
LIBRA_LOG_FILE=/tmp/libra-code.log \
libra code --repo=/Volumes/Data/linked --provider ollama --model gemma4:31b

# 恢复规范 Libra 线程
libra code --resume 11111111-1111-4111-8111-111111111111

# 恢复浏览器驱动的非 Codex headless 会话
libra code --web-only --provider ollama --resume 11111111-1111-4111-8111-111111111111 --browser-control loopback

# 检查同一线程的版本图
libra graph 11111111-1111-4111-8111-111111111111

# 从该仓库外部检查线程图
libra graph 11111111-1111-4111-8111-111111111111 --repo /Volumes/Data/linked

# 以 code review 上下文和严格 approval 启动
libra code --context review --approval-policy untrusted

# 使用 Codex 的先规划后执行模式
libra code --provider codex --plan-mode
```

## 人工输出

输出会根据模式通过 TUI、Web 界面或 MCP 协议交付。默认 TUI 模式没有面向行的 stdout。在 generic provider TUI 中，普通纯文本请求会自动启动 plan workflow；显式 slash commands 保持其命令专用行为。Generic provider planning 使用两步审阅：LLM 首先起草 IntentSpec 供确认，然后确认后的 IntentSpec 会被送回 LLM，用于在任何执行开始前生成可审阅执行计划。如果已确认计划执行失败，或 orchestrator 在到达最终决策前中止，Libra 会将失败证据反馈给 planner，要求其添加或调整修复步骤，并在自动修复阈值内自动运行修订计划。达到阈值后，TUI 会等待开发者用 `/plan continue` 继续自动修复，或提供显式计划修复指导。Web 服务器提供嵌入式 Next.js 应用。Stdio 模式通过遵循 Model Context Protocol 的 JSON-RPC 消息通信。

## Diagnostics

`libra code` 支持通过 `RUST_LOG` 或 `LIBRA_LOG` tracing；两者都设置时，`LIBRA_LOG` 优先。对于 TUI 会话，推荐使用 `LIBRA_LOG_FILE=<path>`，这样 diagnostics 会写入普通日志文件，而不是 alternate-screen terminal。当设置 `LIBRA_LOG_FILE` 但没有显式 log filter 时，Libra 默认使用 `libra=debug`。

对 Ollama provider 失败，有用的 diagnostics 是：

```bash
mkdir -p /tmp/libra-logs
LIBRA_LOG='libra::internal::ai=debug,libra::internal::tui=debug' \
LIBRA_LOG_FILE=/tmp/libra-logs/libra-code-ollama.log \
libra code --repo=/Volumes/Data/linked --provider ollama --model gemma4:31b
```

如果 TUI 报告 Ollama 503，也捕获本地 server 状态：

```bash
ollama ps >> /tmp/libra-logs/libra-code-ollama.log
ollama list >> /tmp/libra-logs/libra-code-ollama.log
```

## 设计动机

### 为什么采用 TUI + Web server 混合？

TUI 为已经在终端中工作的开发者提供低延迟、键盘驱动界面。后台 Web 服务器同时运行，使同一会话可以从浏览器访问，这有助于与队友共享上下文、查看丰富 diff，或从移动设备访问会话。`--web-only` 标志会完全去掉 TUI，用于没有可用终端的 headless 或远程服务器部署。

### 为什么支持多个 AI provider？

不同 provider 擅长不同任务，并具有不同成本/延迟画像。Gemini 因慷慨的免费层和快速响应而作为默认值。Anthropic Claude 擅长谨慎 reasoning 和代码审查。本地 Ollama 支持完全离线开发。通过抽象在 `CompletionClient` trait 后面，添加新 provider 只需要实现该 trait，无需触碰 session、tool 或 TUI 层。

### 为什么集成 MCP？

Model Context Protocol（MCP）是连接 AI clients 与 tool servers 的开放标准。通过支持 `--stdio` 模式，Libra 可以作为任意 MCP 兼容 client（例如 Claude Desktop）的 tool server。Libra 暴露 allowlisted `run_libra_vcs` tool，用于 status、diff、add、commit、branch、log、show 和 switch 等版本控制操作，因此外部 AI agents 直接使用 Libra，而不是调用 Git。`run_libra_vcs` 只接受这些 Libra 子命令；它不是 Git 兼容 shell，agents 应使用 workspace file tools 进行原始文件发现，而不是 `ls-files` 等 Git-only 命令。检查仓库状态时，优先使用 `status --json` 或 `status --porcelain v2 --untracked-files=all`。Libra-managed execution 也会拒绝直接的 `git` shell 命令。

### 为什么需要 approval policies？

AI agents 在开发者机器上执行 shell 命令存在真实安全风险。四层 approval 系统在效率和控制之间取得平衡：
- `never` 用于完全锁定环境，agent 只能读取。
- `on-failure` 允许 agent 尝试沙箱执行，只有失败时才询问。
- `on-request`（默认）把所有操作放进沙箱，并在 agent 或沙箱策略需要时升级。
- `untrusted` 是最保守的交互模式，对已知安全读取之外的任何操作都提示。

### 为什么持久化和恢复会话？

长编码会话会积累大量上下文：文件编辑、对话历史、工具输出。意外关闭终端后丢失这些上下文很痛苦。Session persistence 会存储完整 conversation 和 tool state，而 `--resume <thread_id>` 会恢复规范 Libra 线程。

嵌入式 Code UI 在其 session snapshot 中以 `threadId` 暴露相同规范标识。较旧的 `session_id` 字段仍保留以维持兼容，但新集成应使用 `threadId` 作为 resume、Web、MCP 和 diagnostics 流程的 key。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|------|-------|-----|----|
| 交互式 AI 会话 | `libra code` | 不可用 | 不可用 |
| TUI 模式 | 默认 | 不可用 | 不可用 |
| Web 模式 | `--web-only` | 不可用 | 不可用 |
| MCP/stdio 模式 | `--stdio` | 不可用 | 不可用 |
| AI provider 选择 | `--provider` | 不可用 | 不可用 |
| 会话恢复 | `--resume <thread_id>` | 不可用 | 不可用 |
| 工具 approval policy | `--approval-policy` | 不可用 | 不可用 |

注意：Git 和 jj 都没有 `libra code` 的等价物。该命令体现了 Libra 作为 AI-agent-native 版本控制系统的核心差异。Git 生态中最接近的类似物是 GitHub Copilot CLI 或 aider 等第三方工具，它们是独立应用，而不是集成 VCS 命令。

## 错误处理

| 场景 | 行为 | 退出 |
|------|------|------|
| 同时指定 `--web-only` 和 `--stdio` | Clap 参数冲突错误 | non-zero |
| 选中 provider 缺少 API key | 带 provider 名称和期望 env var 的 fatal error | non-zero |
| 端口已被占用 | 带端口号的 fatal error | non-zero |
| TUI 模式下没有可用终端 | 回退或报告错误 | non-zero |
| 恢复时找不到 Thread ID | 带规范 `thread_id` 的 fatal error | non-zero |
| `--control write --stdio` | 用法错误；MCP stdio 和本地 TUI automation stdio 是不同模式 | non-zero |
| `--control write --host 0.0.0.0` 或其他非 loopback host | 用法错误；write control 仅限 loopback | non-zero |
| 另一个 live `--control write` 拥有相同 control lock | 可用时带已有 PID/URL 的 `CONTROL_INSTANCE_CONFLICT` | non-zero |
| Control token file 是 symlink、非普通文件，或在 Unix/macOS 上不是 `0600` | Web 服务器启动前 fatal setup error | non-zero |
