# `libra code`

启动交互式 AI 编码会话。除 MCP `--stdio` 与 `--control stdio` 客户端 shim 外，所有 `libra code` 启动均进入 Web Code UI（遗留 TUI 启动路径已在 W5 breaking 发布中删除，W5-06）。

## 概要

```
libra code
libra code [-p <PORT>] [--host <HOST>]
libra code --stdio
libra code --provider <PROVIDER> [--model <MODEL>]
libra code --resume <THREAD_ID>
libra graph --json <THREAD_ID> [--repo <PATH>]
```

## 说明

`libra code` 启动一个交互式编码会话，让人类开发者与 AI agent 协作。默认模式启动 Web Code UI（嵌入式 HTTP + AgentRuntime），打印 URL / control 信息并前台常驻，直到 `Ctrl-C` / SIGTERM。**Breaking change（W5-07）：** 弃用的 `--web` / `--web-only` 别名与隐藏的 `LIBRA_CODE_LEGACY_TUI` 回滚环境变量已在 W5 breaking 发布中删除——`libra code` 默认即 Web Code UI，直接去掉该 flag 即可；二进制会以 usage error 加迁移提示拒绝旧 flag。**Breaking change（W5-06）：** 遗留 TUI 启动路径与裸 `--provider codex --resume <thread_id>` 的 TUI resume driver 已在 W5 breaking 发布中删除——裸 `--provider codex --resume <thread_id>` 现在以 usage error 加迁移提示 fail-closed（managed Codex Web resume 尚未落地；Web `--provider codex` 拒绝 `--resume`）。`--stdio` 是 **弃用的 MCP-only legacy** 入口：仅通过标准输入/输出暴露 MCP tools/resources（例如 Claude Desktop），**不是** live turn control。本地自动化请优先使用 `libra code --control stdio`；独立的 `libra mcp --stdio` 计划在 W5 之后（DEFER-02）。

该命令支持八种 AI provider 后端（Gemini、OpenAI、Anthropic、DeepSeek、Kimi、Zhipu、Ollama、Codex），以及三种运行上下文（dev、review、research），用于针对不同工作流调节 agent 行为。会话可以通过 Libra 规范的 `--resume <thread_id>` 流程持久化和恢复。传入 `--goal "<objective>"` 会直接以 goal 模式启动会话，由 supervisor 驱动 tool loop 朝既定 objective 前进，直到 verifier 接受完成。

沙箱化工具执行层会强制 approval policies，控制 agent 何时可以运行 shell 命令、应用补丁、Web 搜索或执行其他可能破坏性的操作。Headless Web 会话在 `dev` 上下文中默认使用 workspace-write 执行且禁止网络访问（遗留 TUI resume driver 已在 W5-06 删除）。执行计划就绪后，Plan review 对话框提供 Execute Plan / Modify Plan / Cancel。Modify 会关闭当前 review，并把下一条纯文本消息作为 durable revision instruction，随后打开替换 review。Execute 会打开独立的强制 network-policy 提示（`Network: Deny` / `Network: Allow` / `Back`）；Deny 放弃执行，Back 返回新一代 Plan review，两个 gate 都可在崩溃后恢复。Network Allow 会把 confirmed plan execution 送入串行的 AgentRuntime 队列。Mutating tools 仍须通过 approval、sandbox 与 tool ACL；失败进入 W2-11 repair loop。目录中保留的 `PLAN_EXECUTION_NOT_AVAILABLE` 409 供旧客户端解码，Allow 不再产生该码。Review 和 research 上下文保持只读，且不授予网络访问。

实时版本图在 Web Code UI 中查看；`libra graph --json` 仍是 agent 路径，交互式 graph TUI 入口已在 W5 breaking 发布中删除（W5-08）。遗留 TUI 退出时打印后续 `libra graph --json <thread_id>` 命令的行为已随 TUI 在 W5-06 一并删除；请自行运行 `libra graph --json <thread_id>`（紧凑形式可用 `--machine`）。检查非当前目录仓库时，使用 `libra graph --json <thread_id> --repo <path>`。

**Linked worktree**：`libra code`（所有模式）可在 linked worktree 中启动，并走 W4-06 RequestScope resolver。安全相关文件（`sandbox.toml`、`hooks.json`、`config.toml` 的 `[approval]`/`[mcp]`、`rules/`、`contexts/`）以及 extension/automation 表面（`agents.toml`、`automations.toml`、`agents/`、`commands/`、`skills/`）保留仓库默认层。安全 overlay 只能收紧；extension overlay 同名覆盖。不可读或无法解析的安全配置，以及损坏的 worktree scope，均 fail-closed，诊断只报 source layer、不回显文件内容。linked worktree 中 automation 的 VCS dispatch 同样经 resolver 运行——见 [automation.md](automation.md)。已按规范 `libra.repoid` 存储的 Always 审批在同一仓库的 linked worktree 中可见，并保留 worktree/session provenance（仅审计）。Session / 一次性审批绑定发起该次确认的 controller lease；lease takeover / detach / expiry 后不得复用，新 controller 必须重新确认。内存 ApprovalStore cache 以 `repo:{libra.repoid}` 为 key，不再使用进程级 `None` 全局 scope。

## 选项

| 标志 | 短参数 | 长参数 | 默认值 | 说明 |
|------|--------|--------|--------|------|
| Port | `-p` | `--port` | `3000` | Web 服务器监听端口。 |
| Host | | `--host` | `127.0.0.1` | Web 服务器 bind 地址。 |
| Working directory | | `--cwd` | 当前目录 | 会话工作目录。 |
| Env file | | `--env-file <PATH>` | 无 | 从 dotenv 风格文件加载 provider 环境变量；显式文件值优先于 Vault 和进程环境。 |
| Control mode | | `--control <observe\|write\|stdio>` | `observe` | 本地自动化控制模式。`observe` 保留现有 loopback 读行为；`write` 启用本地 token discovery 和进程级自动化控制认证；`stdio` 是 **client-only** JSON-RPC NDJSON shim，驱动已有 write-control 会话（不启动 Web/MCP）。 |
| Control token file | | `--control-token-file <PATH>` | `.libra/code/control-token` | 每进程本地自动化 token 路径。在 `write` 模式下，Unix/macOS 文件必须是权限 `0600` 的普通文件。与 `--control stdio` 一起使用时可覆盖 worktree 默认 token 路径（与 `--control-info-file` 独立）；权限过宽 fail-closed（`CONTROL_TOKEN_PERMS`）。 |
| Control info file | | `--control-info-file <PATH>` | `.libra/code/control.json` | 非 secret 本地 endpoint discovery 元数据路径。launch 模式在 Unix/macOS 上以原子写 + `0600` 落盘。该文件永不包含 token 材料。与 `--control stdio` 一起使用时仅为 `baseUrl` 的 **读取** discovery 路径（显式 `--control-url` 可覆盖）。自定义 info 路径**不会**改写默认 token 位置——若 token 不在 worktree `code/` 目录下，请同时传 `--control-token-file`。 |
| Control URL | | `--control-url <URL>` |（discovery） | 已有 Code UI control endpoint 的 base URL（例如 `http://127.0.0.1:3000`）。仅与 `--control stdio` 合法。省略时从 `--control-info-file` discovery。必须是字面 loopback IP。 |
| Provider | | `--provider` | `gemini` | AI provider 后端（见下方 Provider Backends）。 |
| Model | | `--model` | provider 默认值 | Provider 专用 model ID。 |
| Agent profile | | `--agent <NAME>` | 无 | 按名称选择 agent profile。当 profile 携带结构化 `model: provider/model[@variant]` 绑定时，该绑定原子生效——provider、model ID 和 variant 全部来自 profile，单独提供的 `--model` 会被忽略以避免混搭组合；无结构化绑定的 profile 回退到 CLI 默认值。Profiles 通过三层层级解析（项目 `.libra/agents/`、用户 `~/.config/libra/agents/`、内置）。未知或非 primary-eligible profile 会被拒绝。 |
| Temperature | | `--temperature` | provider 默认值 | 生成采样 temperature。 |
| Ollama thinking | | `--ollama-thinking` / `--thinking` | `OLLAMA_THINK`，然后 `off` | Ollama thinking 模式：`auto`、`off`、`on`、`low`、`medium` 或 `high`。 |
| Ollama compact tools | | `--ollama-compact-tools` | `OLLAMA_COMPACT_TOOLS`，然后 off | 为拒绝复杂 JSON schemas 的远程/云 Ollama endpoint 发送紧凑 tool schemas。 |
| DeepSeek thinking | | `--deepseek-thinking <enabled\|disabled>` | 省略 | 使用 `--provider deepseek` 时发送 DeepSeek 的 `thinking` 对象。 |
| DeepSeek reasoning effort | | `--deepseek-reasoning-effort <low\|medium\|high\|max>` | 省略 | 使用 `--provider deepseek` 时发送 DeepSeek 的 `reasoning_effort` 值；`xhigh` 作为 `max` 的别名被接受。 |
| DeepSeek stream | | `--deepseek-stream <true\|false>` / `--stream <true\|false>` | `false` | 使用 `--provider deepseek` 时发送 DeepSeek 的 `stream` boolean。 |
| Kimi thinking | | `--kimi-thinking <enabled\|disabled>` | model 默认值 | 使用 `--provider kimi` 时发送 Kimi 的 `thinking` 对象。 |
| Kimi stream | | `--kimi-stream <true\|false>` | `true`（Kimi） | 使用 `--provider kimi` 时发送 Kimi 的 `stream` boolean；默认流式。对非 Kimi provider 拒绝。 |
| Context | | `--context` | 无 | 运行上下文：`dev`（别名 `development`）、`review`（别名 `code-review`）、`research`（别名 `explore`）。 |
| Resume | | `--resume <THREAD_ID>` | 无 | 按 thread ID 恢复规范 Libra 线程。 |
| Approval policy | | `--approval-policy` | `on-request` | 工具审批策略（见下方 Approval Policies）。 |
| Approval TTL | | `--approval-ttl <SECS>` | `300` | 已授予的审批在再次提示前对匹配命令保持可复用的秒数。覆盖 `.libra/config.toml` 中的项目配置 `[approval] ttl_seconds`；与会提示的策略相关。 |
| Network access | | `--network-access <allow\|deny>` | `deny` | shell/gate 默认网络策略。仅接受默认的 `deny`：`--network-access allow` 在所有模式下均被拒绝，直到 Plan network-policy gate 接管每次执行的 sandbox 网络（请在 Plan review 中批准网络）。 |
| MCP port | | `--mcp-port` | `6789` | MCP server 监听端口。 |
| Stdio | | `--stdio` / `--mcp-stdio` | off | 弃用的 MCP-only legacy：通过 stdio 暴露 tools/resources（非 turn control）。自动化请用 `--control stdio`；独立 `libra mcp --stdio` 计划在 W5 之后。 |
| API base | | `--api-base` | provider 默认值 | Provider API base URL 覆盖。 |
| Codex binary | | `--codex-bin` | `codex` | Codex 可执行文件路径。 |
| Codex port | | `--codex-port` | 随机 | 覆盖 Codex app-server 端口。 |
| Plan mode | | `--plan-mode [<true\|false>]` | off（Codex 为 on） | 要求 agent 在执行前生成经批准的计划。有效默认值对 `--provider codex` 为 on，对其他所有 provider 为 off；显式 `--plan-mode=true`（或裸 `--plan-mode`）只在 `--provider=codex` 下被接受——对其他 provider 会被拒绝；传 `--plan-mode=false` 让 Codex 会话关闭计划模式（opt out of plan mode，非退出会话）。 |
| Browser control | | `--browser-control <off\|loopback>` | `loopback` | `/api/code/controller/attach` 浏览器租约姿态。与 `--stdio` 冲突；`loopback` 要求 loopback `--host`。 |
| Goal | | `--goal <OBJECTIVE>` | 无 | 以 goal 模式启动会话并带上给定 objective，等价于会话打开时运行 `/goal start <objective>`；supervisor 驱动 tool loop 直到声明完成且 verifier 接受。objective 在解析时校验（trim 后非空，至多 16 KiB）。 |

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
常规运行时，将 provider keys 存在 `vault.env.<NAME>` 中；Libra 先检查 repo-local Vault，再检查 global Vault，最后检查进程环境。对需要显式 dotenv 覆盖的 live tests，使用 `--env-file .env.test`。在默认 Web 启动下，非 Codex provider 支持 `--env-file`、`--context`、`--approval-policy`、`--approval-ttl`（env-file 值仍优先于进程环境/Vault）。Managed Web `--provider codex` 仍拒绝 `--env-file`、`--approval-ttl` 与 `--resume`（未接入 Codex app-server 路径）；裸 `libra code --provider codex --resume <thread_id>` 同样以 usage error 加迁移提示被拒绝（遗留 TUI resume driver 已在 W5-06 删除）；MCP `--stdio` 继续拒绝这些 Web-only flag。

Ollama 请求默认流式读取 `/api/chat` 响应，并向 debug logs 添加每请求 `request_id`。它们也默认使用 `think:false`，避免具备 reasoning 能力的本地模型在 tool calls 前花数分钟生成隐藏 reasoning。单次运行使用 `--ollama-thinking high`，或将 `OLLAMA_THINK=true`、`low`、`medium`、`high` 或 `auto` 设为环境默认值。`auto` 会省略 `think` 字段并让 Ollama 决定。当远程/云 Ollama endpoint 接受简单 tools 但对 Libra 完整 tool schema payload 返回 503 时，使用 `--ollama-compact-tools` 或 `OLLAMA_COMPACT_TOOLS=true`。

### 本地自动化控制

`libra code --control observe` 是默认值，除非显式提供 `--control-info-file`，否则不会创建本地控制文件。Loopback clients 可以继续无 token 读取 `/api/code/session` 和 `/api/code/events`。

`libra code --control write` 启用本地自动化安全信封。Libra 会在 `.libra/code/control-token` 中创建新的 32-byte token，在 Web 服务器绑定后以原子写（Unix/macOS 模式 `0600`）将非 secret endpoint 元数据写入 `.libra/code/control.json`，并在进程生命周期内持有 `.libra/code/control.lock`。默认路径按 worktree 本地 gitdir 隔离，两个 worktree 不会共享 token/info/lock；跨 worktree 的 scope mismatch 会 fail-closed，而不是回收另一 worktree 的 sidecar。`control.json` 包含 `version`、`mode`、`pid`、`baseUrl`、可选 `mcpUrl`、`workingDir`、可选 `threadId`、`startedAt`，以及 version-2 writer scope（`repoId`/`worktreeId`/可选 `workspaceId`/`leaseFence`）；它永不包含 token、token hash、token path、provider credentials、headers 或 provider request/response bodies。

Write control 仅限本地。`--control write` 与 `--stdio` 组合会被拒绝，并要求 `--host` 是 loopback（`127.0.0.1`、`::1` 或 `localhost`）。使用相同默认路径启动第二个 write-control 实例会以 `CONTROL_INSTANCE_CONFLICT` 快速失败；只有调用方有意管理多个本地实例时，才使用不同的 `--control-token-file` 和 `--control-info-file` 路径。

Automation clients 使用 `POST /api/code/controller/attach` 连接，请求体 `{ "clientId": "...", "kind": "automation" }`，header `X-Libra-Control-Token`，然后使用返回的 `X-Code-Controller-Token` 写入。Automation-held leases 对 `/api/code/messages`、`/api/code/interactions/{id}`、`/api/code/controller/detach` 和 `/api/code/control/cancel` 同时要求两个 tokens。Code UI 写请求体上限为 256KiB。当会话广告 `capabilities.commandIdempotency`（当前仅 headless web-only）时，`POST /api/code/messages` 接受 `{ "text": "...", "commandId": "..." }` 以支持重试去重（相同 id + 相同 text 幂等；相同 id + 不同 text 返回 `COMMAND_PAYLOAD_CONFLICT`）。运行时会用活动 controller `clientId` 的 SHA-256 fence 命名空间化 `commandId`（原始 clientId 不会写入 durable command log）。`commandIdempotency` 仅在配置了 durable SessionStore command admission 时广告。其他 runtime 若收到 `commandId` 会显式拒绝，而不会静默忽略。

`GET /api/code/diagnostics` 返回为本地工具准备的脱敏 observe-only 状态摘要。Control attach、detach、submit、respond 和 cancel 操作会通过 runtime audit sink 发出 `local-tui-control/v1` audit events；此标识为兼容既有 audit consumers 而冻结，并不代表仍存在 terminal UI。Stdio automation clients 请优先使用 canonical `libra code --control stdio` JSON-RPC NDJSON client：默认从 `.libra/code/control.json` discovery（可用 `--control-url` / `--control-token-file` / `--control-info-file` 覆盖）。Discovery fail-closed 使用稳定码（`CONTROL_INFO_MISSING`、`CONTROL_INFO_PERMS`、`CONTROL_TOKEN_MISSING`、`CONTROL_TOKEN_PERMS`、`CONTROL_SCOPE_CONFLICT`、`CONTROL_SERVER_MISSING`）；attach lease/ownership 冲突以 JSON-RPC `-32000` + Libra 码（如 `CONTROLLER_CONFLICT`）返回。原 `libra code-control` 转发 shim 已在 **W5 breaking 发布（W5-01）中删除**；`libra code --control stdio` 是唯一的 stdio automation client（见[迁移说明](code-control.md)）。弃用的 `libra code --stdio` 仍是 **MCP-only** tools/resources 传输（stderr 弃用警告；非 turn control），不得与 `--control stdio` 混同；独立的 `libra mcp --stdio` 计划在 W5 之后。

Stdio client 在 stdin/stdout 上使用换行分隔的 JSON-RPC 2.0，并把方法映射到 loopback `/api/code/*` HTTP/SSE 控制接口：

| JSON-RPC 方法 | HTTP 等价接口 |
|-----------------|-----------------|
| `session.get` | `GET /api/code/session` |
| `events.subscribe` | 作为 JSON-RPC 通知的 `GET /api/code/events` |
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

格式错误的 JSON 映射为 JSON-RPC `-32700`。未知方法映射为 `-32601`。无效参数映射为 `-32602`。HTTP 4xx/5xx 错误映射为带有 `data.status` 和 `data.code` 的 `-32000`，并保留 `INVALID_CONTROL_TOKEN`、`INVALID_CONTROLLER_TOKEN`、`CONTROLLER_CONFLICT` 和 `INTERACTION_NOT_ACTIVE` 等 Libra 错误。

### Web Browser Control

`--browser-control <off|loopback>` 控制嵌入式 UI 的基于租约的写表面是否可用。Web 启动的默认值为 `loopback`。

当 `--host` 不是 loopback 地址时，选择 `loopback` 会被拒绝；该标志也与 `--stdio` 冲突。绑定非 loopback `--host` 做 observe-only / remote-notice 服务时须显式 `--browser-control off`。

**本地信任模型：** 浏览器 attach 要求 loopback 绑定 + 可信同源 `Origin`/`Referer` + 速率限制（W3-05），**以及**每会话的 `X-Libra-Browser-Bootstrap` secret（打印在 stdout / 以 `?bt=` 嵌入打开 URL）。仅伪造 Origin 不足。含 `?bt=` 的 URL **不会**被自动打开（避免 bootstrap secret 出现在 opener 命令行）；请手动打开终端打印的 URL。共享机器请优先 `--browser-control off`（只读）或隔离主机。

浏览器服务端 endpoints 在 `code_router()` audit matrix（`src/internal/ai/web/mod.rs`）中标记：

- `GET /api/code/session`、`GET /api/code/thread-graph?threadId=`、`GET /api/code/events`、`GET /api/code/diagnostics`、`GET /api/code/threads`、`GET /api/code/usage`、`GET /api/code/skills`、`GET /api/code/goal/status` — 仅 loopback observe。
- `POST /api/code/controller/attach` — loopback。`kind: "automation"` 请求还要求 `X-Libra-Control-Token`。handler **签发** lease 的 `controllerToken`（不期待调用方发送它）。
- `POST /api/code/controller/detach`、`POST /api/code/messages`、`POST /api/code/interactions/{id}` — loopback + `X-Code-Controller-Token`；`Automation` leases 还要求 `X-Libra-Control-Token`。
- `POST /api/code/control/cancel` — loopback + `X-Code-Controller-Token`。`Automation` leases 也要求 `X-Libra-Control-Token`。
- `POST /api/code/task/dispatch` — loopback + `X-Code-Controller-Token`；用户发起的 sub-agent dispatch 需要 active controller write lease（browser 或 automation）。Automation lease 额外要求 `X-Libra-Control-Token`。
- `POST /api/code/goal/start`、`POST /api/code/goal/cancel` — loopback + `X-Code-Controller-Token`；goal mutation 需要 active controller lease。
- `POST /api/code/skills/activate`、`POST /api/code/session/resume` — loopback + `X-Code-Controller-Token`（位于 write router，256 KiB body limit）；两者均需要 controller write lease；Automation lease 另需 `X-Libra-Control-Token`。resume 会拒绝 busy 或 `indeterminate_side_effect` snapshot；在证明目标 thread 可加载后当前 fail-closed 为 `SESSION_RESUME_REQUIRES_RESTART`（尚无 in-process AgentRuntime swap）。skill activate 在 discoverability 校验后 fail-closed 为 `SKILL_ACTIVATION_UNSUPPORTED`，直到存在 provider 消费路径。

浏览器写请求共享与自动化控制相同的 256 KiB body limit 和 audit-sink wiring。浏览器只在内存中持久化 lease；重新加载页面会丢弃 lease，下一次写入会重新 attach。

浏览器写入（含 `kind: "browser"` 的 `POST /controller/attach`）还要求可信的 loopback `Origin`（或同源 `Referer` 回退），且必须匹配 Code UI bind 地址（精确的 `http://<bound-ip>:<port>`；绑定到经典 loopback 时额外接受 `localhost` / `127.0.0.1` / `[::1]` 别名）。缺失或跨站 Origin 以 `ORIGIN_REQUIRED` fail-closed。Automation 写入走 `X-Libra-Control-Token` / controller lease，**不得**用 Origin 代替身份校验。浏览器与 automation 生产者共用按 session 的写速率限制（`LIBRA_CODE_SESSION_WRITE_RATE_LIMIT` / `LIBRA_CODE_SESSION_WRITE_RATE_WINDOW_SECS`，默认 120 次 / 60s），超限返回 `429 RATE_LIMITED`，窗口恢复后可继续。

嵌入式 SPA 的 session-lifecycle 面板通过 `GET /api/code/threads` 列出 threads，经 `POST /api/code/control/cancel` 取消当前 turn（当 `controller.canWrite` 为 false 时 fail-closed），并经 `POST /api/code/session/resume` 以 `{ "threadId": "..." }` 发起 resume。Thread 列表按仓库存储根共享（跨 linked worktree）；列表项在 ThreadProjection 持久化 per-thread cwd 之前省略 `workingDir`。

Usage 面板镜像 W2-12 `RuntimeUsageTotals` read model（累计、本 turn 增量、sub-agent 归因），并保持 `partial`/`unknown`/`error` 可见，而不是伪装成零花费。`GET /api/code/usage` 从 durable totals 读取，失败时返回错误而不伪造零值。当 durable sub-agent 枚举不可用时，响应省略 `subAgents` 并设置 `subAgentsStatus: "unavailable"`，而不是返回空数组。

Execution/repair 面板从 live session snapshot 投影 `plans[]`、`toolCalls[]` 与 `planExecutionRepair`。Continue/Cancel 经 `POST /api/code/interactions/{id}` 发送 `selectedOption`（`continue` / `cancel`）；当投影的 `attempt >= max_attempts` 时，Continue 还会带上提高后的 `maxAttempts`（上限 10），且不在浏览器侧重新分类失败。

SSE resilience 面板展示 reconnecting / resync-required / resynced 状态，同时保留最后一次投影的 session snapshot 与 wire 提供的 cursor seq（浏览器从不自造序号）。显式 snapshot resync 走共享 store 的 `refresh()`，仅在 refresh 成功应用（或被更新的 live 更新 superseded）时报告成功；production v2 backlog/resync 事件在后续 wire 卡（W3-06/W3-08）落地，内置 SPA 切换归 W3-09。

Workflow review 面板投影 pending 的 `intent_review_choice` 与 `post_plan_choice`（network policy 同 kind，用 `metadata.phase = "networkPolicy"` 区分）。Confirm/modify/cancel（以及 execute / network-allow / network-deny / back）经 leased interaction endpoint 发送 `selectedOption`；当浏览器不能 write 时 turn cancel fail-closed。选择 Plan Modify 后，下一条被接纳的纯文本消息就是 revision note；Libra 将其 durable 绑定到被拒绝的计划，并在 Phase 1 完成后打开替换 review。面板不保存第二套 workflow FSM，等待下一次 snapshot/SSE 更新。Network Allow 会消费 network gate，并把 confirmed plan execution 送入串行 runtime 队列；mutating tools 仍走 approval/sandbox/ACL，repair 仍由 W2-11 持有。

IntentSpec Modify 后，revision mode 可接收下一条纯文本消息，也可使用严格控制形式 `/intent modify <changes>`。其私有 HMAC 认证状态依次经过 `Prepared`、`Active`、`Claiming` 和绑定 event 的 `Consuming`：完整 consumer command binding 在 Runtime admission 前 fsync，精确 event id/sequence 在 executor start gate 打开前绑定。只有 durable effect proof 成立后才会提交收据：原始文本精确等于 `/intent cancel` 时是固定的无 provider effect，Modify 时则是恰好一次成功的 `submit_intent_draft` 加替换 review marker。收据之后才能删除 sidecar。该 authority 活跃时，新的 explicit direct command 在写入 intent 或 transcript 前返回 `409 SESSION_BUSY`；精确 terminal 重试或匹配的 live `commandId` 重试仍是幂等确认。带空白的 cancel 写法是普通 explicit-direct 输入。`/intent cancel` 不会取消 pending Plan revision；后者仍需要下一条非空纯文本 Plan note。

启动时只构造一次共享、线性的 `ValidatedIntentRevisionReceiptIndex` 来验证
revision authority。source terminal、receipt、consumer status、replacement marker 与 retry
lineage 都复用这个索引，不会为每张收据重新扫描完整 event stream。回归 fixture 精确包含
5,000 个 events 与 700 张 receipts，并要求 indexed relationship visit 不超过 replay event
数量的四倍。

每个 Phase 1 review 都绑定 canonical checkout identity 与精确、ignore-aware 的
content fingerprint。v0.20.4 新写入的 binding 还携带
`metadata-v1:<sha256>` change token，覆盖路径、entry kind、symlink target 与
change-sensitive metadata；它用于快速 resume 投影和可判定的 pre-write
校验/重试信号，绝不能单独授权 Execute，Execute 仍会重算 content fingerprint。每个新
binding 都在同一稳定区间内按 `metadata before -> exact content before -> exact content
after -> metadata after` 捕获；任一 exact 或 metadata 不匹配都会拒绝捕获，不会把旧
content authority 与较新的 drift baseline 配对，在文件 metadata 较粗的平台上也一样。
没有该 additive token 的旧 review context 继续可读，并回退到精确 content
fingerprint 比较。每次扫描都执行 30 秒 cooperative work budget、1,000,000 个遍历 entry
（包括目录）的工作预算，以及累计 128 MiB 编码路径名的内存预算。路径会先流式计数并受限，
再对有界 manifest 做确定性排序，因此超宽目录不能绕过 entry 上限。单次阻塞式文件系统操作
或目录 iterator step 可能超过 cooperative 时间预算；包括最终 EOF 在内，每次重新取得控制
都会复查预算并 fail-closed，不会接受未验证的 workspace。路径名枚举仍由 ignore-aware
walker 完成，只提供有界、排序后的 name list，本身不是读取 authority。Unix 的权威 entry
访问使用 pinned root/parent file descriptor 与 `openat`/`fstatat`/`readlinkat`，以
no-follow/nonblocking 打开并复核 identity、type、metadata、path 与 reopen 结果。Windows
使用 pinned handle、`FILE_FLAG_OPEN_REPARSE_POINT`、final-path/file-identity 校验与
`FSCTL_GET_REPARSE_POINT` 验证 regular file 和 reparse target。其他平台因为不支持安全
workspace fingerprint read 而 fail-closed。因此 symlink、FIFO、replacement、parent/root
swap、rename 或 reparse race 都不能让 scanner hash checkout 外部的内容。

启动恢复要求一份完整、连续且已 committed 的 workflow replay。发现 sequence gap 或
bounded-window cut 时，会在选择 Plan/Network authority 或 GC 任一 Phase 1 context 前
fail-closed。Session append lock 是持久 regular-file 上的 OS advisory lock：不会按年龄
回收，guard 也不会 unlink；lock path 若为 symlink 或 special entry 会 fail-closed。

可恢复 headless session 还会在 reload 或 projection fold **之前**取得进程生命周期的
Phase 1 writer lease。它与短时 workflow append lock、browser/automation controller lease
是三个不同的锁与 ownership domain。writer lease 绑定精确 SessionStore path 与 session id；
所有 clone 共享一次性 claim，因此只能构造一个独立 persistence graph。Unix 以
`O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC` 打开并校验 device/inode；Windows 以
`FILE_FLAG_OPEN_REPARSE_POINT` 打开、拒绝 reparse point，并校验 volume/file identity。
持久 lock path 从不 unlink，进程退出则由 OS 立即释放 advisory lock。

resume 时，workspace 或 checkout 变化信号保持为可恢复 gate 状态：interaction metadata
暴露 `workspaceDrifted: true` 与可操作的 `workspaceWarning`；Libra 不会 fence session，
也不会启动 mutation。仅 metadata 变化时该信号只是提示：Execute 会执行精确
identity/content 复核，并可在 authority 仍匹配时继续。精确 identity/content 已漂移或
精确验证失败时，Execute 返回 `409 PHASE1_WORKSPACE_CHANGED` 并保留原 pending Plan
gate。对于同一 Intent repository 内的新 HEAD，Modify 后的下一条非空纯文本 note
会重新捕获当前 checkout、生成替换 Plan，并保留用户已有文件；不同 repository identity
必须 Cancel 后发起新 request，重新审阅 IntentSpec。重新捕获失败时返回同一 409，note
不会被消费。空或纯空白 revision message 返回
`400 PLAN_REVISION_NOTE_REQUIRED`，revision authority 继续 pending。

当服务器绑定到非 loopback host（`--host 0.0.0.0` 或局域网地址）时，非 loopback 浏览器的 HTML navigation 会收到静态 remote access notice，而不是 SPA。该 notice 零 JavaScript，只包含 bind/remote/version/commit 占位符；asset/API fallback 返回 404，使远程 clients 无法探测 session state。Snapshot、transcript、SSE、approval 以及所有 `/api/code/*` 读写 surface 仍保持 loopback-only（`LOOPBACK_REQUIRED`）。远程人工应通过 SSH port-forward 访问 loopback（`ssh -L 3000:127.0.0.1:3000 user@host`），不要期望远端浏览器直接可写；经认证的 TLS 反代属 DEFER-04，不是本计划默认面。

默认监听端口为 `3000`。若该地址已被占用，启动会 fail-closed 并提示显式 `--port`，**不会**自动扫描下一个空闲端口。

同一时间只有一个 writer 持有 controller lease：当已有 lease 活跃时，第二个 browser 或 automation attach 会以 `CONTROLLER_CONFLICT` 失败；lease takeover 会丢弃前一个 controller 的 session 审批（见下文 Approval Policies）。

对于默认 Web 启动的非 Codex providers（`--provider ollama` 是规范 headless 验证路径），Libra 构建 [`HeadlessCodeRuntime`](../../../src/internal/ai/web/headless.rs) 生命周期宿主，并将 [`AgentRuntimeCodeUiAdapter`](../../../src/internal/ai/web/agent_runtime_adapter.rs) 挂载为生产浏览器写路径 owner。浏览器 submit 进入串行的 `AgentRuntimeWorker`：普通（非 `/` 前缀）消息走共享的 Phase 0 plan 工具白名单，使 direct chat 无法绕过默认 mutating gate；以 `/` 开头的消息保留显式 direct tool loop，但上文的 revision controls `/intent modify <changes>` 和 `/intent cancel` 例外。IntentSpec review、Phase 1 draft/revision、Plan/network-policy gates、approval 与 resume 使用 runtime-owned interaction 和 event 路径。Network Allow 经 `submit_confirmed_plan_execution` 把 confirmed plan execution 送入串行 AgentRuntime 队列。Mutating tools 仍走共享 hardening/approval/sandbox/ACL 边界，分类失败后由 W2-11 repair loop 接管。Headless 模式公布 `messageInput`、`streamingText`、`toolCalls`、`planUpdates`、`patchsets`、`interactiveApprovals`、`structuredQuestions` 和 `providerSessionResume`。默认 Web `--resume <thread_id>` 会为非 Codex provider 在同一工作目录加载匹配会话，并在启动浏览器服务器前应用有界的 durable Code UI projection suffix。Web `--provider codex` 仍拒绝 `--resume`；裸 `libra code --provider codex --resume <thread_id>` 以 usage error 加迁移提示被拒绝（遗留 TUI resume driver 已在 W5-06 删除；managed Codex Web resume 尚未落地）。`update_plan` 投影到 `plans[]`，`apply_patch` metadata 投影到 `patchsets[]`。取消在工具的 mutation boundary 之前采用 cooperative 方式；一旦可能变异的工具已经开始，取消会被接受（`200`），runtime interaction 进入 `Cancelling`；浏览器可见的 `status` 在工具取得可判定结果前仍为 `executing_tool`。Libra 不会 hard-abort 该副作用，也不会把它重标为普通 `cancelled` turn；取消等待期间的并发 submit 会以 `SESSION_BUSY` 被拒绝。

对于 Web `--provider codex`，managed app-server 的 websocket 通知归一到共享 runtime `AgentEvent` 信封（与其他 provider 同一 projection 路径）。未知 Codex method 走显式可诊断的 `ProviderNotification` fallback，不 silent drop、也不 panic。Ask-mode approvals 挂在共享 `AgentRuntime` interaction 注册表上，并把浏览器 `respond_interaction` 决策转发到 app-server；Codex 仍拥有 app-server 内的 approval 回环（见 `docs/development/tracing/code.md` 的 DEFER-07）。对外 approval option id 与非 Codex 一致（`approve` / `deny` / `abort`）。

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
| `controller` | object | `{ kind, ownerLabel?, canWrite, leaseExpiresAt?, reason?, loopbackOnly }`；`kind` 是 `none`、`browser`、`automation`、`tui` 或 `cli`。`tui` 仅从历史 snapshot 解码；每个新的 lease 都会以 `INVALID_CONTROLLER_KIND` 拒绝它，并且新会话绝不会输出该值。 |
| `status` | string | `idle`、`thinking`、`executing_tool`、`awaiting_interaction`、`completed`、`error` 或 `indeterminate_side_effect`。最后一种表示变异命令可能已生效；再次尝试前必须先完成 reconciliation。 |
| `transcript` | array | 带 `id`、`kind`、可选 `title` / `content` / `status`、`streaming`、`metadata`、`createdAt`、`updatedAt` 的条目。 |
| `plans` / `tasks` / `toolCalls` / `patchsets` | arrays | Workflow、Summary、Diff 和 Terminal panes 使用的 runtime projections。 |
| `interactions` | array | 待处理/已解决的 UI prompts。`kind` 是 `approval`、`sandbox_approval`、`request_user_input`、`intent_review_choice`、`post_plan_choice` 或 `plan_execution_repair`。待处理的 plan-repair interaction 提供 `continue` 和 `cancel`，通过正常 interaction endpoint 回应。 |
| `planExecutionRepair` | object, optional | Runtime-owned plan-execution repair 状态。它含 snake_case 的 `state`、有界且经 runtime 脱敏的 failure `evidence`（`output`、`diagnostics`、`attempt`、`max_attempts`），并在 `awaiting_user` 时含 `interaction_id`。`automatic_repair` 表示进行中的重试；`awaiting_user` 只会在配置的重试次数耗尽后出现：Code UI Continue 必须提供更高的 `maxAttempts`（例如当前上限为 2 时发送 `{ "selectedOption": "continue", "maxAttempts": 3 }`），否则返回 `PLAN_REPAIR_RETRY_LIMIT_REACHED`；也可以提供手动修订指导。Cancel 为终态。`intent_spec_revision` 和 `manual_action` 需要新的用户定向 workflow。 |
| `threadGraph` | object, optional | 当前 `threadId` 的 indexed Intent/Plan/Task/Run/PatchSet 图（W4-04）。storage 未解析、id 非 UUID、加载失败或 live heads 未被覆盖时省略/清空。camelCase 形状与 `GET /api/code/thread-graph` 相同。 |
| `updatedAt` | string | ISO 8601 更新时间戳。 |

`GET /api/code/thread-graph?threadId=<uuid>` 返回与 `threadGraph` 相同的 `CodeUiThreadGraph`（loopback observe，fail-closed redaction）。`threadId` 经 `Uuid::parse_str` 解析（带连字符的 RFC-4122 或 32 位 hex）；其它输入为 `THREAD_GRAPH_INVALID_ID`。缺少 indexed projection 为 `404 THREAD_GRAPH_NOT_FOUND`；storage/load/redaction 失败为 `500 THREAD_GRAPH_STORAGE_UNAVAILABLE` / `THREAD_GRAPH_UNAVAILABLE` / `REDACTION_FAILED`。

| 字段 | 类型 | 契约 |
|------|------|------|
| `threadId` | string | Canonical thread UUID。 |
| `title` | string, optional | Thread 标题（已脱敏）。 |
| `selectedPlanId` / `activeTaskId` / `activeRunId` | string, optional | Live heads；也可出现在节点 `selected` / `active` / `running` tags。 |
| `nodes` | array | `{ depth, kind, id, label, tags? }`，kind 为 `intent` / `plan` / `task` / `run` / `patchset`。上限 256；保留 live heads，剩余名额从最新 lineage 填充。 |
| `truncated` | boolean, optional | indexed 图超过 256 节点时出现。 |
| `omittedNodeCount` | number, optional | 被 cap 丢弃的节点数。 |
| `totalNodeCount` | number, optional | 截断时的完整 indexed 节点数。 |

`GET /api/code/events` 流式传输会话更新。Wire 版本协商如下（W3-06 / plan-20260715）：

| 选择 | 机制 |
|---|---|
| 显式 v1 | `?wire=1` 或 `?wire=v1` |
| 显式 v2 | `?wire=2` 或 `?wire=v2` |
| Accept 提示 | `Accept: text/event-stream;libra-wire=2`（若同时给出 query `wire=`，以 query 为准） |
| 未指定默认 | 省略 `wire` / `libra-wire` 的客户端仍为 **v1**。内置 SPA（W3-09）始终请求 `?wire=2`。 |
| 非法值 | fail-closed `400 INVALID_WIRE_VERSION` |

**SSE v1**（默认）：`CodeUiEventEnvelope` 记录，含 `seq`、`type`、`at`、`data`。事件 `type` 为 `session_updated`、`status_changed` 或 `controller_changed`；`session_updated` 携带完整 `CodeUiSessionSnapshot`。

**SSE wire v2**：`code_workflow` 事件，camelCase 字段 `cursor`（W1-06 持久 workflow sequence）、`eventId`、`kind`、`at` 与最小 `payload`。用 `?wire=2&cursor=<lastCursor>` 断线重连，在 **transport** backlog 窗口内无重复、无丢事件（W3-08 / GC-CODE-12）：**1,024 条或 8 MiB**，先达者为准（`MAX_CODE_UI_TRANSPORT_BACKLOG_*`）。Code UI **projection** 热窗口是同数值、独立命名的预算（`MAX_CODE_UI_PROJECTION_EVENTS` / `MAX_CODE_UI_PROJECTION_REPLAY_BYTES`），两者不可相加。单事件 fold 只访问 suffix，不回放整段 session 历史（W3-14；10k events 下 release p95 ≤ 5 ms）。bootstrap 或慢消费者 catch-up 将超过该预算时，服务器发送 `event: resync`（`WIRE_V2_RESYNC_REQUIRED`，含 `reason` / `lastCursor` / `durableTail` / `action: fetch_snapshot`）并结束流，**不 silent drop**。客户端应拉取 session snapshot，再以 `durableTail` 重连。Wire v2 需要 SessionStore-backed workflow hub。当前该 hub 挂在带 session persistence 的默认 Web headless（非 Codex `HeadlessCodeRuntime`）；managed `--provider codex` Web 在暴露 hub 之前会返回 `503 WIRE_V2_REQUIRES_DURABLE_SESSION`。

v2 envelope 使用 camelCase，但各 `payload` 保留 durable workflow event 的
snake_case schema。新的 `plan_review_requested` payload 包含 `context_id`，用于绑定
immutable Phase 1 context；Back 会创建新的 interaction id，但复用来源 `context_id`，
而不复制 context。Modify 后产生的替换计划还可以携带可选 `revision_of`，其值是被消费
的上一代 Plan review interaction。Back 准备替换 Plan gate 时，该行还会携带可选
`prepared_from_network`；只有被引用的 Network gate 已持久化 `back` resolution 后，
该 Plan gate 才成为权威。历史行省略这些字段：`context_id` 解码为 `""` 并回退到该行的
`interaction_id`，`revision_of` 与 `prepared_from_network` 解码为 `None`。客户端必须把
缺省 lineage 解释为初始或 legacy Plan review，不能自行推断。早于这些字段的 reader 会
忽略 additive members，仍把该行识别为 `plan_review_requested`。

runtime 在正式写入 execution/test Plan pair 前会发送
`phase1_formal_write_started`，payload 包含 `phase1_turn_id`、
`source_interaction_id` 与不含秘密正文的 `seed_digest`。恢复仅可在此 marker 尚不存在时，
重挂与 seed 精确匹配的 Pending Phase 1 command；marker 已存在却没有后续
`plan_review_requested` 时属于不确定写入边界，必须 fail closed 并要求 reconciliation。

当 command 接受一条 interaction 后还会继续运行时，durable checkpoint 继续复用既有的
`interaction_resolved` event kind。新行可以携带 `command` identity 与
`prior_interaction_resolutions`；兼容字段 `interaction_id` / `resolution` 仍表示本次
checkpoint。该 additive 形状使旧 reader 仍能推进 workflow sequence 并保留当前非敏感审计标签，
同时不会把仍为 Pending 的 command 误判为 terminal。

命令终态行同样沿用 snake_case payload。`command_terminal_success_with_interaction_resolved`
把当前 gate 保留在兼容字段 `interaction_id` / `resolution` 中，并可用
`prior_interaction_resolutions` 记录同一 command 先前已经交付的 approval 或 user-input；
`command_terminal_failure` 可同样携带 `interaction_resolutions`。每条历史记录都是二元素数组
`[interaction_id, non_secret_resolution_label]`，其中不会保存原始回答、approval payload 或
provider 输出。

规范的 IntentSpec `modify` 终态只可额外携带公开绑定
`intent_revision: { "interaction_id": "...", "sidecar_digest":
"hmac-sha256:<64 个小写十六进制字符>" }`。非空 Modify note 会先去除首尾空白，其 UTF-8
表示不得超过 16 KiB（16,384 bytes）。提交 terminal 前，Libra 会先 durable 写入私有、
session-local 的 **Prepared** sidecar；其中保存原始 note、准确 IntentSpec，以及覆盖 schema、lineage
和正文的 keyed HMAC-SHA256。crash-atomic terminal row 与同一次 fsync 只保留兼容主字段
`interaction_id` / `resolution` 和 digest 绑定，绝不写入原始 note 或 HMAC key。这个排除
边界适用于 workflow terminal、消费收据及其专用 SSE v2 payload；它不会清除普通 transcript
entry、完整 session snapshot 或对应 projection delta 中的用户内容，用户文本仍按既有边界保留。

HMAC key 仅属于当前 session，并与本地 sidecar 一同存放。digest 用于跨崩溃证明准确的
durable lineage，并避免私有正文进入公开 workflow stream；它**不能**防御能够同时改写这两个
本地文件的同用户攻击者。已有 HMAC 绑定后 key 缺失、digest 非规范或不匹配、lineage 有歧义，
以及已绑定 terminal 同时缺少 sidecar 与准确消费收据，都会 fail closed 并要求 reconciliation。

terminal fsync 后，**Prepared** 会提升为已认证的 **Active** revision mode。startup 遇到
原 gate 仍开放的孤立 Prepared 时会将其丢弃；若存在匹配的已提交 terminal 则完成提升，任何
不匹配都 fail closed。下一条被接受的 revision turn 会先 durable 写入 consumer command intent
与 browser-message snapshot，再在 Runtime admission 之前 fsync 携带完整 command identity 的
**Claiming** envelope。当 durable intent 的 event id 和 sequence 已确定后，Libra 会在
executor start gate 打开前把 Claiming 提升为绑定 event 的 **Consuming**。

只有请求的 effect 获得 durable proof 后才会提交消费。对于原始文本精确等于
`/intent cancel` 的输入，该 proof 是固定的“不调用 provider”取消 effect；Libra 会在
unlink sidecar 并确认取消前追加、fsync 收据。对于普通 revision note 或严格的
`/intent modify <changes>`，provider 必须恰好一次成功调用 `submit_intent_draft`；
Libra 会先 durable 写入替换 IntentSpec 及其 `IntentReviewRequested` marker，再追加收据并
unlink sidecar。成功 draft 调用为零次或多次时都 fail closed，且不消费 revision。带前后
空白或其它不精确写法的 `/intent cancel` 是普通 explicit-direct 输入，不会获得固定
cancel 特权。Modify suffix 会在写入 Claiming 之前去除首尾空白并限制为 16 KiB。

已提交的消费会以 additive 的 `intent_revision_consumption` 收据字段追加到 workflow
事件流中，绑定准确的 terminal lineage、consumer command intent 以及 consumer intent 的
event id/sequence。SSE v2 把已提交收据投影为专用的
`kind: "intent_revision_consumed"`，payload 为
`{ "consumption": ... }`；其中不含通用 resolution 字段或原始 note。durable effect proof
之前崩溃会保留 Consuming，以便按准确 consumer 身份恢复。替换 review marker 或 cancel
收据之后崩溃时，startup 不会重跑 provider，会规范化残留的 transcript/tool projection，并
保留同一 replacement gate（或已完成的取消）。有效收据会永久闭合整条准确 retry lineage，
即使后续已解决 replacement gate，更晚的 Web command 也不会使历史 consumer 变得有歧义。
互相冲突的 marker/terminal/receipt 顺序（包括在不兼容的 consumer terminal 之后才写入
replacement marker）会 fail closed，且绝不伪造收据。

Prepared 与 Consuming 会故意留下空的 legacy `intentSpec`，因此早于这些 envelope 的 reader
会拒绝它们，而不会激活未提交或消费状态有歧义的 revision。Active 只有在 terminal 已提交后
才对旧 reader 保持可读；若旧 binary 在没有新收据的情况下消费它，之后的新 reader 会 fail
closed，而不会把 sidecar 缺失当作已消费证明。历史 terminal 行省略 `intent_revision`，解码为
`None`；旧 workflow reader 会忽略这个 additive member，仍根据兼容主字段关闭 terminal。
新 reader 只会在存在唯一、准确 terminal lineage 时接受绑定机制之前的 legacy Active sidecar，
并在消费前补上 digest；缺失的非空 legacy note 无法重建。上述 resolution-history 字段继续保持
additive，缺省为空列表。

在 `phase1_formal_write_started` 之前，失败的 Phase 1 command 可以把
`retry_intent_review` 与 `command_terminal_failure` Failure 终态放在同一个
crash-atomic row 和 fsync 中。该行同时是 Failure 终态与唯一的 IntentSpec retry
review authority，因此恢复不会看到缺少替换 gate 的失败。Cancel 会等待已 admission 的
terminal writer，在 durable 地以 `cancel` 关闭内嵌 retry interaction 后才确认取消；
replay 因而不会重新打开该 gate。

### SSE v1 兼容窗口（DEFER-08）

在 wire v2 成为默认、且内置前端/automation 客户端完成迁移之后，v1 snapshot SSE 仍至少保留一个成功的公开 patch release。v1 的物理移除**不属于** plan-20260715；见 DEFER-08 / ADR-CODE-08。移除前置条件清单（须全部满足）：

1. 内置前端已迁移到 v2（W3-09 证据）：SPA 经 `sse-resilience` 的
   `wrapClientForSseResilience` 打开 `GET /api/code/events?wire=2`，用 wire cursor
   重连，并把 `event: resync` / `WIRE_V2_RESYNC_REQUIRED` 当作一次显式 snapshot
   拉取（复用 W2-15 UI）。cursor/序号不在客户端另行编号。
2. 内置 automation 客户端已迁移到 v2。
3. Compat / matrix 测试默认消费 v2。
4. Release notes 写明最后支持 v1 的版本与升级路径。
5. 在 (1)–(4) 之后、v1 仍可用时，至少有一次成功的公开 patch release。


`GET /api/code/threads` 返回 `{ items, nextOffset? }`。每个 item 有 `id`、可选 `title`、`archived`、可选 `currentIntentId`、可选 `workingDir`、`createdAt` 和 `updatedAt`。在 ThreadProjection 持久化 per-thread cwd 之前省略 `workingDir`（不要用 server cwd 冒充 linked-worktree thread）。`limit` 默认 50 并 clamp 到 200；格式错误的 `limit` 或 `offset` 返回 `INVALID_QUERY_PARAM`。

`GET /api/code/skills?provider=<slug>&skill=<name>` 返回 curated A0-07 `{ items: [{ name, provider }] }`。未知 `provider` slug 返回 `INVALID_SKILL_PROVIDER`（与 activate 相同）；省略 `provider` 时列出全部 curated providers。`POST /api/code/skills/activate` 接受 `{ provider, name }`；在 discoverability 校验后当前返回 `SKILL_ACTIVATION_UNSUPPORTED`，直到存在 in-process provider activation 路径。

Code UI API 错误使用 `{ error: { code, message } }`：

| Code | HTTP | 含义 |
|------|------|------|
| `LOOPBACK_REQUIRED` | 403 | 非 loopback client 试图访问 API route。 |
| `PAYLOAD_TOO_LARGE` | 413 | 写请求体超过 256 KiB。 |
| `ORIGIN_REQUIRED` | 403 | 浏览器写/attach 缺少可信 loopback `Origin`（或同源 `Referer`），或提交了跨站 Origin。 |
| `MISSING_BROWSER_BOOTSTRAP` | 403 | 已签发 bootstrap secret 的会话上，浏览器 attach 缺少 `X-Libra-Browser-Bootstrap`。 |
| `INVALID_BROWSER_BOOTSTRAP` | 403 | `X-Libra-Browser-Bootstrap` 与本 Libra Code 会话不匹配。 |
| `RATE_LIMITED` | 429 | 当前 session 写配额耗尽；等待速率窗口恢复后重试（见 `Retry-After`）。 |
| `REDACTION_FAILED` | 500 | Session / diagnostics / SSE 投影无法应用 secret redactor（规则为空或序列化失败）。Fail-closed：响应不包含未脱敏 payload；重启 `libra code` 或修复 redactor 配置后重试。 |
| `INVALID_WIRE_VERSION` | 400 | `GET /api/code/events` 的 `wire` / `libra-wire` 取值非法（仅接受 `1`/`v1` 与 `2`/`v2`）。 |
| `WIRE_V2_REQUIRES_DURABLE_SESSION` | 503 | SSE wire v2 需要 SessionStore-backed workflow hub（当前挂在默认 Web headless persistence；managed Codex Web 尚未暴露）。 |
| `WIRE_V2_CURSOR_AHEAD` | 409 | `?cursor=` 超过 durable workflow 尾部；丢弃 cursor 并 resync（超前 cursor 会导致后续 live 事件永久跳过）。 |
| `WIRE_V2_RESYNC_REQUIRED` | SSE `resync` 后断流 | Transport backlog 超限（1,024 条 / 8 MiB）；拉取 snapshot 并以 `cursor=<durableTail>` 重连。 |
| `WIRE_V2_REPLAY_FAILED` | 500 | Wire v2 无法从指定 cursor 回放 durable workflow 事件（缺口或 I/O；容量出口用 `WIRE_V2_RESYNC_REQUIRED`）。 |
| `CONTROL_DISABLED` | 403 | 当前进程未启用 automation control。 |
| `MISSING_CONTROL_TOKEN` / `INVALID_CONTROL_TOKEN` | 403 | Automation control token 缺失或无效。 |
| `MISSING_CONTROLLER_TOKEN` / `INVALID_CONTROLLER_TOKEN` | 403 | Lease token 对写路由缺失或无效。 |
| `INVALID_CONTROLLER_KIND` | 400 | Controller attach 请求了不支持的 kind。 |
| `CONTROLLER_CONFLICT` | 409 | 另一个 live controller 拥有 lease，或会话正忙。 |
| `INTERACTION_NOT_ACTIVE` | 409 | respond 目标 interaction 没有活跃的 runtime turn。 |
| `PHASE1_WORKSPACE_CHANGED` | 409 | Execute 的精确 checkout identity/content 不再匹配已审 Plan，或 Libra 无法验证/重新捕获。仅 metadata 变化的 `workspaceWarning` 可能通过精确复核，本身不会产生此错误。不会启动 mutation，陈旧 Execute 保留 pending gate。同一 repository 的新 HEAD 可用 Modify 重生成；不同 repository 必须 Cancel 后发起新 request。重新捕获失败不会消费 note。 |
| `PLAN_EXECUTION_NOT_AVAILABLE` | 409 | 历史 Web 409：当时 confirmed-plan execution 尚未接线。W2-04 之后 Network Allow 会把执行送入串行 runtime 队列，而不再产生该码。旧客户端仍可解码目录中的 409。 |
| `SESSION_BUSY` | 409 | 有 turn 运行时重复 submit、无 turn 可取消时 cancel，或 IntentSpec revision authority 活跃时发送新的 explicit-direct command。精确 terminal retry 与匹配 live `commandId` retry 是幂等确认，不追加事件。 |
| `BROWSER_CONTROL_DISABLED` | 403 | 浏览器写控制已禁用。 |
| `AUTOMATION_CONTROLLER_REQUIRED` | 403 | 用非 automation lease 调用了 automation-only 路径。 |
| `CODE_UI_UNAVAILABLE` | 404 | 没有 active `libra code` session 附加到 Web 服务器。 |
| `INVALID_QUERY_PARAM` | 400 | 查询或 interaction response 校验失败，包括 `/threads` 分页，以及超过 16 KiB（16,384 UTF-8 bytes）上限的 IntentSpec Modify note。过大的 plain 或 `/intent modify` 输入会在 Claiming、Runtime admission 与任何 workflow append 前被拒绝。 |
| `PLAN_REVISION_NOTE_REQUIRED` | 400 | Plan Modify 后的下一条纯文本消息为空或只有空白。Revision authority 保持 pending 且未消费；请发送非空修改说明，或 Cancel。 |
| `INVALID_COMMAND_ID` | 400 | `commandId` 为空、过长，或包含空白/控制字符。 |
| `THREAD_GRAPH_INVALID_ID` | 400 | `GET /api/code/thread-graph` 的 `threadId` 不是 canonical UUID。 |
| `STORAGE_PATH_INVALID` / `STORAGE_ROOT_UNRESOLVED` / `STATUS_UNAVAILABLE` / `THREAD_LIST_FAILED` / `DB_UNAVAILABLE` / `USAGE_UNAVAILABLE` / `THREAD_GRAPH_STORAGE_UNAVAILABLE` / `THREAD_GRAPH_UNAVAILABLE` / `SESSION_RESUME_LOAD_FAILED` / `INTERNAL_ERROR` | 500 | 服务端 storage、status、projection、database、usage、thread graph、resume load 或 fallback internal failure。 |
| `THREAD_GRAPH_NOT_FOUND` | 404 | 请求的 `threadId` 没有 indexed thread projection。 |
| `INVALID_SKILL_PROVIDER` / `SKILL_NOT_DISCOVERABLE` | 400 | skill provider 不是 A0-07 slug，或 skill 对该 provider 不可发现。 |
| `SKILL_ACTIVATION_UNSUPPORTED` | 422 | skill 可发现，但尚无 in-process activation 路径。 |
| `SESSION_RESUME_BUSY` | 409 | thinking 或 tool-running session 不能被替换。 |
| `SESSION_RESUME_NOT_FOUND` | 404 | 当前工作目录下没有匹配 session。 |
| `SESSION_RESUME_REQUIRES_RESTART` | 422 | 目标 thread 可加载，但尚无 in-process AgentRuntime swap；需用 `libra code --resume <threadId>` 重启。 |
| `RECONCILIATION_REQUIRED` | 409 | 变异 turn 需要人工 reconciliation；在检查 durable session 数据前不要自动重放。 |
| `COMMAND_PAYLOAD_CONFLICT` | 409 | 相同 `commandId` 被复用到不同的消息 payload。 |
| `COMMAND_ALREADY_TERMINAL` | 409 | 相同 `commandId` 已以 failed/cancelled/indeterminate 终态结束；重试需分配新的 `commandId`。 |
| `PLAN_REPAIR_RETRY_LIMIT_REACHED` | 409 | plan-repair Continue 请求未提高已耗尽的自动重试上限。使用更高的 `maxAttempts` 重试（例如当前上限为 2 时 `{ "selectedOption": "continue", "maxAttempts": 3 }`）、提供手动修订指导，或取消 repair。 |
| `UNSUPPORTED_OPERATION` | 422 | Runtime 拒绝尚不支持的请求操作。 |

### Web Search

`web_search` 工具要求会话网络策略允许 outbound access。如果 `BRAVE_SEARCH_API_KEY` 可从 `vault.env.BRAVE_SEARCH_API_KEY` 或进程环境获得，Libra 会先尝试 Brave Search API，并返回结果标题、URL 和 snippets。如果 Brave 未配置或请求失败，Libra 会回退到零配置 DuckDuckGo HTML endpoint。

### Approval Policies

| 值 | 别名 | 说明 |
|----|------|------|
| `never` | -- | 不提示；危险命令直接拒绝。 |
| `allow-all` | `allow_all`、`always`、`accept` | 不提示；本会话允许每一条命令（`allows_all_commands`）。 |
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
# 使用默认 Gemini provider 启动 Web Code UI
libra code

# 使用 Anthropic Claude 启动
libra code --provider anthropic --model claude-sonnet-4-20250514

# 将 Web UI 绑定到所有接口；远程浏览器会看到 loopback-only notice
# （须显式 --browser-control off：默认 Web 为 loopback，会拒绝非 loopback host）
libra code --port 8080 --host 0.0.0.0 --browser-control off

# 远程人工应 SSH port-forward 到已绑定的端口，而不是直接开放写面
# ssh -L 8080:127.0.0.1:8080 user@host
# 然后在本地浏览 http://127.0.0.1:8080

# 浏览器驱动的本地 Ollama 会话（默认启用 loopback 浏览器写租约）
libra code --provider ollama --port 4400

# managed Codex 默认 Web 路径（浏览器写租约默认为 loopback）
libra code --provider codex

# 启用本地自动化写控制（写入 token + lease discovery 文件）
libra code --control write

# 以 JSON-RPC NDJSON 驱动已有 write-control 会话（client-only）。
# 默认读取 `.libra/code/control.json` + sibling `control-token`。
libra code --control stdio

# 显式 endpoint 覆盖（仍仅限 loopback）
libra code --control stdio \
  --control-url http://127.0.0.1:3000 \
  --control-token-file .libra/code/control-token

# 从 dotenv 风格文件加载 provider keys（覆盖陈旧 shell env vars）
libra code --env-file .env.test

# 弃用的 MCP-only legacy（tools/resources；非 turn control）。
# 自动化请用 --control stdio；独立 `libra mcp --stdio` 计划在 W5 之后。
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

# 使用本地 Ollama 模型时捕获 provider diagnostics
LIBRA_LOG='libra::internal::ai=debug' \
LIBRA_LOG_FILE=/tmp/libra-code.log \
libra code --repo=/Volumes/Data/linked --provider ollama --model gemma4:31b

# 在非 Codex Web 会话中恢复规范 Libra 线程
libra code --resume 11111111-1111-4111-8111-111111111111
libra code --provider ollama --resume 11111111-1111-4111-8111-111111111111

# 检查同一线程的版本图
libra graph --json 11111111-1111-4111-8111-111111111111

# 从该仓库外部检查线程图
libra graph --json 11111111-1111-4111-8111-111111111111 --repo /Volumes/Data/linked

# 以 code review 上下文和严格 approval 启动
libra code --context review --approval-policy untrusted

# 使用 Codex 的先规划后执行模式
libra code --provider codex --plan-mode
```

## 人工输出

输出会根据模式通过 Web UI 或 MCP 协议交付。Web 模式会在 stdout 打印 URL / control 信息并前台常驻直到 SIGINT/SIGTERM。在 generic provider workflow 中，普通纯文本请求会自动启动 plan workflow；显式 slash commands 保持其命令专用行为。Generic provider planning 使用两步审阅：LLM 首先起草 IntentSpec 供确认，然后确认后的 IntentSpec 会被送回 LLM，用于在任何执行开始前生成可审阅执行计划。Modify 会等待下一条纯文本 revision note，再展示替换计划；Execute 只推进到强制 network-policy gate。Deny 与 Back 可用；Network Allow 会把 confirmed plan execution 送入串行 runtime 队列。Mutating tools 仍须 approval/sandbox/ACL，分类失败进入 W2-11 repair loop。Web 服务器提供嵌入式 Next.js 应用。Stdio 模式通过遵循 Model Context Protocol 的 JSON-RPC 消息通信。

## Diagnostics

`libra code` 支持通过 `RUST_LOG` 或 `LIBRA_LOG` tracing；两者都设置时，`LIBRA_LOG` 优先。设置 `LIBRA_LOG_FILE=<path>` 可将 diagnostics 写入普通日志文件。当设置 `LIBRA_LOG_FILE` 但没有显式 log filter 时，Libra 默认使用 `libra=debug`。

对 Ollama provider 失败，有用的 diagnostics 是：

```bash
mkdir -p /tmp/libra-logs
LIBRA_LOG='libra::internal::ai=debug' \
LIBRA_LOG_FILE=/tmp/libra-logs/libra-code-ollama.log \
libra code --repo=/Volumes/Data/linked --provider ollama --model gemma4:31b
```

如果会话报告 Ollama 503，也捕获本地 server 状态：

```bash
ollama ps >> /tmp/libra-logs/libra-code-ollama.log
ollama list >> /tmp/libra-logs/libra-code-ollama.log
```

## 设计动机

### 为什么采用 Web Code UI？

Web Code UI 是主要的（也是唯一的交互式）协作入口。遗留 TUI 及其裸 `--provider codex --resume` resume driver 已在 W5 breaking 发布中删除（W5-06）；弃用的 `--web` / `--web-only` 别名与 `LIBRA_CODE_LEGACY_TUI` 回滚环境变量更早于同一发布中删除（W5-07）。

### 为什么支持多个 AI provider？

不同 provider 擅长不同任务，并具有不同成本/延迟画像。Gemini 因慷慨的免费层和快速响应而作为默认值。Anthropic Claude 擅长谨慎 reasoning 和代码审查。本地 Ollama 支持完全离线开发。通过抽象在 `CompletionClient` trait 后面，添加新 provider 只需要实现该 trait，无需触碰 session、tool 或 Web UI 层。

### 为什么集成 MCP？

Model Context Protocol（MCP）是连接 AI clients 与 tool servers 的开放标准。弃用的 `libra code --stdio` 仍可让 Libra 作为 Claude Desktop 等 client 的 MCP tool/resource server（仅 tools/resources——不是 live Code turn control）。独立的 `libra mcp --stdio` 计划在 W5 之后（DEFER-02）；在此之前该 legacy 入口会打印弃用警告。本地自动化请优先使用 `libra code --control stdio` 驱动 write-control Web 会话。Libra 暴露 allowlisted `run_libra_vcs` tool，用于 `status`、`diff`、`branch`、`log`、`show`、`show-ref`、`ls-files`、`add`、`commit` 和 `switch` 等版本控制操作，因此外部 AI agents 直接使用 Libra，而不是调用 Git。`run_libra_vcs` 只接受这些 Libra 子命令；它不是 Git 兼容 shell。检查仓库状态时，优先使用 `status --json` 或 `status --porcelain v2 --untracked-files=all`，并使用 `ls-files` 检查 tracked 与 untracked 仓库路径（例如 `ls-files --others --exclude-standard` 列出忽略感知的 untracked 文件）。Libra-managed execution 也会拒绝直接的 `git` shell 命令。

### 为什么需要 approval policies？

AI agents 在开发者机器上执行 shell 命令存在真实安全风险。五层 approval 系统在效率和控制之间取得平衡：
- `never` 用于完全锁定环境，agent 只能读取。
- `allow-all` 是另一个极端：不提示且每条命令都运行，适用于摩擦大于风险的可信一次性或沙箱环境。
- `on-failure` 允许 agent 尝试沙箱执行，只有失败时才询问。
- `on-request`（默认）把所有操作放进沙箱，并在 agent 或沙箱策略需要时升级。
- `untrusted` 是最保守的交互模式，对已知安全读取之外的任何操作都提示。

已写入 `approved_permission` 的 Always 审批按仓库身份持久化，对该仓库的每个 worktree 可见。Session/TTL memo 只存在于当前 controller lease 的内存 cache 中，并在 lease takeover、detach 或 expiry 时丢弃（含 browser/automation 首次从前一个 controller 接手）。

### 为什么持久化和恢复会话？

长编码会话会积累大量上下文：文件编辑、对话历史、工具输出。意外关闭终端后丢失这些上下文很痛苦。Session persistence 会存储完整 conversation 和 tool state，而 `--resume <thread_id>` 会恢复规范 Libra 线程。

嵌入式 Code UI 在其 session snapshot 中以 `threadId` 暴露相同规范标识。较旧的 `session_id` 字段仍保留以维持兼容，但新集成应使用 `threadId` 作为 resume、Web、MCP 和 diagnostics 流程的 key。

对于持久化的非 Codex Web session，初始 session 写入是启动 turn 的前提：写入失败时 Libra 不会启动 turn，浏览器可修复存储后重试。Approval 与 user-input response 也会在释放 continuation 前写入 checkpoint；若该 checkpoint 失败，原 interaction 保持 pending、尚未启动获批操作，修复存储后可以重试同一 response。若 response 或 side-effect boundary 已被消费后再发生持久化失败，live session 会变为 `indeterminate_side_effect`，并阻止新的 submit 或 interaction reply；重启或 reconciliation 前必须检查 durable session data。

收到 `Ctrl-C` / `SIGINT` 或 `SIGTERM` 时，非 Codex headless / web-only 进程会关闭浏览器命令 admission，再走统一的进程级 lifecycle shutdown owner（runtime / listeners / managed child / control），并共享同一 deadline。read-only/model 工作会 cooperative cancel；已经开始的 mutating tool 在预算内被允许完成。若超过期限，`libra code` 会以明确的 shutdown failure 退出，重启前必须检查 session 并完成 reconciliation。进程编排应优先发送 `SIGTERM`（或 `Ctrl-C`/`SIGINT`），避免直接 `SIGKILL`，以便端口、lease 与子进程被干净释放。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|------|-------|-----|----|
| 交互式 AI 会话 | `libra code` | 不可用 | 不可用 |
| Web 模式 | 默认（唯一交互模式；`--web`/`--web-only` 别名已在 W5-07 删除，遗留 TUI 已在 W5-06 删除） | 不可用 | 不可用 |
| MCP/stdio 模式 | `--stdio` | 不可用 | 不可用 |
| AI provider 选择 | `--provider` | 不可用 | 不可用 |
| 会话恢复 | `--resume <thread_id>`（Web / 非 Codex；Web Codex 拒绝 `--resume`，裸 codex+resume 以 usage error 失败） | 不可用 | 不可用 |
| 工具 approval policy | `--approval-policy` | 不可用 | 不可用 |

注意：Git 和 jj 都没有 `libra code` 的等价物。该命令体现了 Libra 作为 AI-agent-native 版本控制系统的核心差异。Git 生态中最接近的类似物是 GitHub Copilot CLI 或 aider 等第三方工具，它们是独立应用，而不是集成 VCS 命令。

## 错误处理

| 场景 | 行为 | 退出 |
|------|------|------|
| 指定 `--web` / `--web-only` | W5-07 已删除：clap unexpected-argument usage error 加迁移提示（`libra code` 默认即 Web Code UI） | non-zero |
| 裸 `--provider codex --resume <thread_id>` | W5-06 已删除：clap usage error 加迁移提示（遗留 TUI resume driver 已删除；managed Codex Web resume 尚未落地） | non-zero |
| 选中 provider 缺少 API key | 带 provider 名称和期望 env var 的 fatal error | non-zero |
| 端口已被占用 | fatal：指出 `host:port`，并要求显式 `--port`（不自动扫描） | non-zero |
| `--network-access allow` | 所有模式下均为 usage error，直到 Plan network-policy gate 接管每次执行的 sandbox 网络 | non-zero |
| 恢复时找不到 Thread ID | 带规范 `thread_id` 的 fatal error | non-zero |
| `--control write --stdio` | 用法错误；MCP `--stdio`（tools/resources）与 `--control stdio` 自动化是不同模式 | non-zero |
| `--control write --host 0.0.0.0` 或其他非 loopback host | 用法错误；write control 仅限 loopback | non-zero |
| 另一个 live `--control write` 拥有相同 control lock | 可用时带已有 PID/URL 的 `CONTROL_INSTANCE_CONFLICT` | non-zero |
| Control token file 是 symlink、非普通文件，或在 Unix/macOS 上不是 `0600` | Web 服务器启动前 fatal setup error | non-zero |
