# Agent MCP 服务端计划（2026-10-19）

**模板版本:** `v2.14`（整份按 v2.14 新建；见 [`plan-template.md`](plan-template.md)）

**计划状态:** 实施中（MC-01、MC-22、MC-16、MC-19 已 `done` / `complete`；MC-29 发布候选 `0.30.79`，D 组未绿前 Acceptance 仍空）

**计划基线 commit:** `85dcc505998eddfb3303242623e47a053db6bbf4`（`main`，该提交中 `Cargo.toml` 为 `version = "0.30.56"`，核对时间 `2026-10-19 08:01:54 UTC`；此后工作区已出现其它计划未提交的 `2026101902_worktree_layout_capability` migration 与 `0.31.0` 版本改动（DEP-MC-03），本计划不以它们为基线；开工时按 ER-02 刷新）

## 文档职责

本文解决「外部 MCP 客户端（WorkBuddy、Grok Bot 等）无法以标准 Model Context Protocol 接入 Libra 仓库能力」的问题，目标是新增一个**标准 MCP 服务端入口** `libra agent mcp`：stdio 传输（`--stdio`）供本机客户端按需拉起（无需常驻进程）；Streamable HTTP 传输（`--transport http`）在 loopback 上监听，由运维方提供的 HTTPS 终端转发给远程客户端（如 Grok Bot）。服务端内部复用现有 `agent bridge` 的仓库操作实现（checkpoint、commit、workspace lease、diff）以及 `libra status`、agent trace、workspace registry 的读取能力，把它们暴露为标准 MCP tools。现有 `libra agent bridge --stdio` **对外行为保持原样**，继续只服务 DeepSeek Harness，两者互不干扰。

**术语（强调）：** 本计划中的「Grok Bot」专指作为**远程 MCP 客户端**接入 `libra agent mcp --transport http` 的 Grok Bot 这个**工具**——它通过运维方提供的 HTTPS 终端调用本计划暴露的 MCP 工具；本计划**不**接入、调用、捕获或评测 Grok 模型本身，也与 `plan-20261018-grok-capture`（`libra agent` 对 Grok CLI 会话的观测/捕获，`agent_kind = grok`）无关。凡本文出现「Grok Bot」，均指该客户端工具；出现 `grok-capture` 处仅表示与该独立计划的写集依赖（DEP-MC-01），不是本计划的接入对象。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

**plan-status 同步义务（使用规则）:** 执行本计划的 Agent 在完成或推进任意任务卡时，必须在同一变更中同步更新 [`plan-status.md`](plan-status.md)（状态、发布版本/commit、`YYYY-MM-DD HH:MM:SS UTC`）；本计划在成稿提交中于 `plan-status.md`「一、计划一览」「二、未启动的计划与卡」「四·零」与 `plan-long.md`「日期计划索引」各登记一行（与本文件同一提交）。证据只写入本文件（v2.12），不新建任何证据文件或目录。

### 适用范围

- 新命令表面 `libra agent mcp`：`--stdio`（等价 `--transport stdio`）、`--transport stdio|http`、`--allow-write`、`--actor`；HTTP 专属 `--listen`、`--token-out`、`--allowed-host`、`--allowed-origin`；help EXAMPLES。
- 新内部模块 `src/internal/ai/mcp_server/`：MCP JSON-RPC 协议核心（legacy 与 modern 两个协议纪元）、结果信封（脱敏、大小上限、读 deadline）、tool 注册表、MCP 写会话与崩溃收敛、stdio 与 Streamable HTTP 传输。
- `src/internal/ai/agent_bridge/` 的按 source 分表：Harness 继续使用 `agent_bridge_*`（结构与 SQL 不变），MCP 使用新建的同构表集 `agent_mcp_*`；对应 Repository 角色 migration（只建新表）。
- 从 `src/command/agent/{session,checkpoint,workspace}.rs` 抽出 CLI 与 MCP 共用的查询函数（GC-02 单一事实源），CLI 输出逐字节不变。
- 文档：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md` 的 `agent` 行（只在第 252 行行内追加，不增删行）、`sql/migrations/README.md`、`docs/development/tracing/deepseek-harness.md`、`tests/INDEX.md`、相邻网站仓库 `../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。
- 真实客户端联调验收（WorkBuddy stdio、Grok Bot 远程 HTTPS）：MC-15。

### 非目标

- 不改变 `libra agent bridge --stdio` 的任何对外行为：NDJSON 帧、20 个 method 的 allowlist、错误码目录、`initialize` 握手、Harness 表结构与标识语义、Harness 插件 fixture 不变（ADR-MC-02、GC-MC-01）。
- 不恢复 0.23.0 删除的 `libra code` MCP `--stdio`、`rmcp` 依赖或内部 AgentRuntime；本计划的 MCP 服务端只暴露仓库操作，不执行模型、不托管 Agent（ADR-MC-03）。
- 不在 Libra 进程内实现 TLS 或 OAuth 2.1 授权服务器；Libra 只在 loopback 上监听（ADR-MC-07）。
- 不支持 MCP `2025-03-26` 与 `2024-11-05`（前者要求 JSON-RPC batch，本计划不实现 batch；ADR-MC-04）。
- 不暴露 `review.run`、session/event ingress、transcript 原文导出、`push`、agent checkpoint `rewind`、任意路径/revision 选择器、TaskCopy workspace 创建（ADR-MC-09，DEFER-MC-01）。
- 不实现 MCP 的 resources、prompts、sampling、roots、logging、completions、tasks 扩展、`subscriptions/listen`、SSE 流（ADR-MC-03）。
- 不在 Windows 上提供 HTTP 传输（令牌文件权限无法强制，ADR-MC-07，DEFER-MC-05），也不在非 Unix 上开放 `--allow-write`（写路径端到端门（写锁、写入期限退出、hook 拒绝）只在 Unix 上验证，DEFER-MC-08）；Windows 上只读 stdio 可用。
- MCP 提交不运行仓库 hook：存在任一会执行的提交 hook 时 `libra_commit` 拒绝（`LBR-AGENT-035`，`hooks_present`），不绕过、不放宽（ADR-MC-06，DEFER-MC-09）。
- 不按仓库状态精确收敛崩溃中断的 commit（保留显式 `quarantined` 不确定态，DEFER-MC-06）。
- 本计划不提供 MCP restore 工具：Libra 没有让 CLI/Harness/MCP 共同遵守的仓库写锁，restore 在检查与写入之间无法排除并发写入者，按「互不干扰」要求整体延后（DEFER-MC-07）。

### 成功定义

- `libra agent mcp --stdio` 在仓库内启动后，标准 MCP 客户端完成 legacy 握手（`2025-11-25` / `2025-06-18`）或发出 modern 请求（`2026-07-28`），`tools/list` 返回确定顺序的工具集，读工具结果等于对应 CLI `--json` 数据经结果信封（脱敏）后的值。
- 默认只读；加 `--allow-write` 后，`libra_checkpoint_create`、`libra_commit` 经 bridge mutation 管线（客户端必填的 `operation_id` 幂等与跨重启重放、digest 冲突拒绝、approval 门、HEAD/clean fence、provenance 关联）写入 MCP 自己的 `agent_mcp_*` 表集；崩溃中断的写入要么按确定性规则收敛（checkpoint），要么进入显式 `quarantined` 并拒绝重放（commit）；`libra_commit` 以事务内 CAS 原子拒绝在 fence 之后移动 HEAD 的任何写入者（MC-18）。workspace lease 工具（claim/renew/release）只作用于当前仓库工作树（主工作树以保留身份 `main:<repository_id 的 SHA-256 十六进制>`，MC-40），要求客户端 `operation_id`，lease 效果与 MCP operation 记录在同一 SQLite 事务内提交，同一 `operation_id` 重放返回首次结果（记录从不删除；每个 lease 至多 16 次 renew；MC-11、MC-38、MC-41；首次结果持久化于 `agent_mcp_intent.result_json`；重放前先复核意图中保存的可信工作树身份，不同工作树的重放以 `LBR-AGENT-032` 拒绝）。Harness 与 MCP 使用独立表集，同名标识互不影响。
- `libra agent mcp --transport http --listen 127.0.0.1:<port> --token-out <路径>` 只在 loopback 上监听，`POST /mcp` 必须带正确的 `Authorization: Bearer` 令牌，非法 `Origin`/`Host` 返回 403，非 loopback 地址与 Windows 平台启动即被拒绝。
- `libra agent bridge --stdio` 的全部既有测试 target（`agent_bridge_*` 共 15 个）保持绿色，且新增隔离用例证明 Harness 与 MCP 互不可见、同名标识双向互不干扰。
- WorkBuddy 经 stdio 配置、Grok Bot 经 HTTPS 终端实际完成工具调用，证据写入本计划（MC-15）。
- 每张实现卡按 ER-08 完成 `patch + 1` 发布；全部卡完成后全量收口门（fmt、clippy、双 env nextest）全绿，计划方可标记完成。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。下表核对于 `2026-10-19 11:01:16 UTC`。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `libra agent bridge --stdio` 只接受 `--stdio`，按 `libra.repoid` 解析可信仓库身份，构造 `BridgeContext` 后跑 NDJSON 循环，退出前 `supervisor::shutdown()` | `src/command/agent/bridge.rs:34-40`、`:112-158`、`:163-186` |
| 代码入口 | `AgentSubcommand::Bridge` 注册与分发；`AGENT_EXAMPLES` 常量含 bridge 行 | `src/command/agent/mod.rs:109-145`、`:232-233`、`:451` |
| 协议权威 | bridge source 常量 `deepseek-harness`；帧/批/结果上限 256 KiB、in-flight 64、页 100；20-method allowlist | `src/internal/ai/agent_bridge/protocol.rs:18`、`:37-51`、`:62-82` |
| 读方法 | `context.get`/`status.get`/`history.search`/`checkpoint.list`/`checkpoint.show`/`diff.get`；缺失 checkpoint 返回 `LBR-AGENT-032` | `src/internal/ai/agent_bridge/methods.rs:52-90`、`:221-245`、`:342-552` |
| 写方法 | mutation 管线：`operation_id` 必填 → 可信 session scope → `upsert_operation`（只按 `operation_id` 冲突、digest 不同则拒绝）→ approval 门 → actor 绑定（硬编码 `deepseek-harness`）→ 重放短路（`applied` 返回原结果；`pending` 返回 retryable denied；`failed`/`quarantined` 允许再执行）→ 先执行效果、后把 operation 记为 `applied`；参数摘要覆盖全部 `params` | `src/internal/ai/agent_bridge/mutations.rs:39-56`、`:67-75`、`:77-190`、`:200-225`、`:146`、`:333`、`:574`；`storage.rs:405-449` |
| checkpoint 写 | `checkpoint.create` 只登记客户端给出的 `checkpoint_id`/`target_oid`/`agent_checkpoint_id` 元数据，不读取 HEAD、不做快照 | `src/internal/ai/agent_bridge/mutations.rs:515-583`；`storage.rs:641-667` |
| commit/restore | `commit.create` 只提交当前 index，支持 `allow_empty` 与可选 `expected_head`；restore 要求 `expected_head` 与干净 index/worktree，把工作树恢复到 checkpoint 固定的提交且不移动 HEAD | `src/internal/ai/agent_bridge/vcs.rs:404-458`、`:500-532`；`docs/commands/agent.md:324` |
| diff | bridge diff 返回原始 patch 文本（无输出脱敏）：封闭 `mode` + 校验后的仓库相对路径，强制 `--no-ext-diff`/`--no-textconv` | `src/internal/ai/agent_bridge/vcs.rs:212-302`、`:303-313` |
| 脱敏 | bridge 只对入站事件持久化脱敏（`review::redact_untrusted` + 控制字符清洗，失败 `LBR-AGENT-034`），不作用于读结果 | `src/internal/ai/agent_bridge/redaction.rs:20-53` |
| 授权 | `checkpoint.create` 等为 Normal；commit/restore/review 为 Dangerous，需 `approval.decision = "approved"` | `src/internal/ai/agent_bridge/authorization.rs:32-43`、`:54-100` |
| 会话 scope | `trusted_session_scope` 与 ingress 的 session 校验只比较 `repository_id` | `src/internal/ai/agent_bridge/provenance.rs:159-179`；`ingress.rs:46-63` |
| 上下文 | `BridgeContext { conn, repository_id, worktree_id }`，无 source 字段；10 个既有测试文件直接以结构体字面量构造它 | `src/internal/ai/agent_bridge/ingress.rs:22-26`；`tests/agent_bridge_{approval,checkpoint,ingress,mutation,read_methods,redaction,security,span,vcs,workspace}_test.rs` |
| workspace lease | bridge `workspace.claim` 要求客户端给出绝对 `path`，无 `worktree_id` 时创建 `TaskCopy`；renew/release 只校验 owner/fence；owner 固定 `deepseek-harness:<session_id>`；lease 方法不经过 operation 管线 | `src/internal/ai/agent_bridge/workspace.rs:79-104`、`:109-178`、`:179-268` |
| 数据/状态 | `agent_bridge_session.source CHECK(source = 'deepseek-harness')`；`agent_bridge_operation.operation_id` 与 `agent_bridge_checkpoint.checkpoint_id` 为全局主键；子表以外键 `ON DELETE CASCADE` 引用 session；bootstrap SQL 不含 bridge 表（只由 migration 创建） | `sql/migrations/2026081801_agent_bridge_capture.sql:13-75`；`sql/migrations/2026082401_agent_bridge_link_relations.sql:37-60`；`rg -n agent_bridge_session sql/*.sql` 零命中 |
| migration 注册 | `repository_migrations()` 中的条目只属于 Repository 角色；当前最大版本 `2026101902`（`worktree_layout_capability`） | `src/internal/db/migration.rs:1738`（`repository_migrations`）、`:2438`、`:2569`；`src/internal/db/schema.rs:239-258` |
| 降级屏障 | 库 schema 版本高于二进制 latest 时分类为 `UnsupportedFuture`，打开时报 `… is newer than this Libra binary supports …; install a newer Libra binary` | `src/internal/db/schema.rs:368-375`、`:389-400` |
| 读能力 | `collect_status_json_envelope_for_api(working_dir)` 返回与 `libra status --json` 同源的 envelope，进程内串行化，要求 `working_dir` 等于进程 cwd | `src/command/status.rs:477-521` |
| trace 读 | `agent session list/show`、`agent checkpoint list/show` 的查询与输出耦合在 CLI 函数内 | `src/command/agent/session.rs:407-500`、`:501-584`、`:765-780`；`src/command/agent/checkpoint.rs:260-329`、`:330-405`、`:1531-1557` |
| workspace 读 | `agent workspace list/show` 的查询与 `record_json` 在 CLI 模块内，输出经 `emit_json_data("agent_workspaces", …)` | `src/command/agent/workspace.rs:76-96`、`:97-147`、`:148-202` |
| CLI 测试位置 | `command_test` 中已有 `agent_workspace_test`、`agent_checkpoint_test` 模块 | `tests/command/agent_workspace_test.rs`；`tests/command/agent_checkpoint_test.rs`；`tests/command/mod.rs:898` |
| lib 测试路径 | `src/internal/ai/**` 内单元测试的完整名以 `internal::ai::` 开头 | `src/internal/mod.rs`、`src/internal/ai/mod.rs` 模块树 |
| 锁模型 | CLI 分发对 `Commands::Agent(_)` 不持有命令级共享维护锁 | `src/cli.rs:2264-2300`（`command_holds_shared_maintenance_lock`） |
| 共享存储 | `try_get_storage_path` 返回仓库共享存储目录（仓库数据库所在、linked worktree 共用） | `src/utils/util.rs:1233`（工作区当前行号；基线提交中为 `:1177`） |
| 表归属注册表 | `CODE_AGENT_TABLE_OWNERSHIP` 与 `MUTABLE_STATE_OWNERSHIP` 对 `agent_*` 表做穷举守卫，新表必须登记 | `src/internal/config_ownership.rs:180-260`（`agent_bridge_operation` 在 `:247`）；`src/internal/mutable_state_ownership.rs:264`、`:1223-1240` |
| 锁原语 | `acquire_control_lock(path)`：advisory 文件锁，另一存活进程持有时快速失败；`validate_token_file_perms` 在非 Unix 平台无条件成功 | `src/command/control_lock.rs:624-674`、`:752-790` |
| HTTP 先例 | `libra service` 只绑定 loopback（解析期与绑定期双重校验），每个端点校验 loopback peer，数据端点要求 0600 令牌文件 | `src/command/service.rs:1-11`、`:104-107`、`:168-176` |
| 依赖 | `axum = "0.8.9"`、`uuid`（v4/v5/v7）已在依赖中；`rmcp` 已于 0.23.x 删除 | `Cargo.toml:101`、`:28`；`docs/development/plan/plan-20260920.md:152` |
| 用户命令 | 无任何 MCP 服务端命令；`libra code` 的 MCP `--stdio` 已删除且文档明确「不恢复」 | `docs/commands/code.md:3`；`COMPATIBILITY.md:252`（agent 行） |
| 文档 | agent 命令文档 bridge 行与 EXAMPLES（EN/zh） | `docs/commands/agent.md:125`、`:324`、`:898-900`；`docs/commands/zh-CN/agent.md:107`、`:250`、`:645` |
| 网站文档 | 本机唯一网站仓库 `/Volumes/Data/GitMono/libra-backend`（Libra 仓库，当前 `cf...origin/cf`）；其 `AGENTS.md` 要求：命令文档同步脚本 `--check`、typecheck + build、Claude Code PASS 后在 `cf` 提交推送并核对 Cloudflare 部署；该文件写的路径 `/Volumes/Sea/libra-backend` 在本机不存在 | `../libra-backend/AGENTS.md:24-29`；`ls /Volumes/Sea/libra-backend`（不存在，2026-10-19）；`../libra-backend/package.json:12`、`:15` |
| CI | `base.yml` job：`format`、`clippy`、`owner-liveness-macos`、`scoped-input-windows`、`redundancy`、`test`、`network-remotes`、`opencode-export-sandbox-linux`；`codeql.yml`：`analyze`；`release.yml`：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest`；release artifact URL 形如 `https://download.libra.tools/libra/releases/v<ver>/libra-darwin-arm64` | `.github/workflows/base.yml:13-532`；`codeql.yml:21`；`release.yml:15`、`:310`、`:480`、`:628`、`:690`、`:582` |
| 测试 | bridge 测试 target 15 个（`tests/INDEX.md:108-122`），均以 `[[test]]` 注册 | `Cargo.toml:373-383` 起 |
| 错误码 | bridge 稳定码：`LBR-AGENT-022` lease 被持有、`023` lease 已结算、`027` invalid params、`028` 超上限、`032` scope mismatch / 对象不存在、`033` digest conflict、`034` redaction failed、`035` denied、`036` timeout、`038` stale fence；CLI 参数错误 `LBR-CLI-002`（退出码 129） | `src/internal/ai/agent_bridge/protocol.rs:271-390`；`src/utils/error.rs:823-826`；`docs/error-codes.md:94` |
| 外部参照 | MCP 规范仓库 `modelcontextprotocol/modelcontextprotocol@1155ca460e5097019f32769b38dcba194432e2fb`（main，提交时间 2026-10-18T02:02:09Z）；版本目录 `2024-11-05`、`2025-03-26`、`2025-06-18`、`2025-11-25`、`2026-07-28`、`draft` | `docs/specification/2025-11-25/basic/transports.mdx`（stdio 第 28-34 行；HTTP 第 70-106 行；版本头一节）、`docs/specification/2025-11-25/server/tools.mdx`（error handling）、`docs/specification/2026-07-28/{changelog.mdx,basic/versioning.mdx,basic/index.mdx,basic/transports/streamable-http.mdx}`；核对 2026-10-19 |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-MC-01 | 没有标准 MCP 服务端；bridge 是私有 NDJSON 协议 | WorkBuddy、Grok Bot（客户端工具）无法握手 | `protocol.rs:62-82`；`docs/commands/code.md:3` | MC-01、MC-02 |
| GAP-MC-02 | bridge store 只允许一个 source；operation/checkpoint 主键全局；actor/owner 写死 | 复用管线会伪装成 Harness，同名标识互相干扰 | `2026081801_agent_bridge_capture.sql:16`、`:56`、`:72`；`mutations.rs:146` | MC-03、MC-04 |
| GAP-MC-03 | Harness 读面按 `repository_id` 查询全部 session | 第二个 source 会污染 Harness 读面 | `methods.rs:342-420` | MC-04 |
| GAP-MC-04 | diff 结果无输出脱敏 | patch 中的 token 会流向 MCP 客户端 | `vcs.rs:212-302` | MC-16（脱敏）、MC-19（上限）、MC-05 |
| GAP-MC-05 | 无 MCP 写会话；bridge 先写效果后记 applied，崩溃窗口无收敛 | 写操作不可恢复或被误判 | `mutations.rs:67-75`、`:200-225` | MC-06、MC-08 |
| GAP-MC-06 | checkpoint.create 不派生目标提交 | 创建出不可恢复的 checkpoint | `mutations.rs:515-583` | MC-06、MC-07 |
| GAP-MC-07 | 无 MCP commit | 外部 Agent 无法经标准协议可追溯写入；bridge commit 无原子 HEAD 前提 | `vcs.rs:404-458`（无 CAS） | MC-18、MC-08 |
| GAP-MC-08 | workspace 读查询耦合 CLI；lease 需要客户端路径、可建 TaskCopy、renew/release 不校验工作树 | 无法安全经 MCP 协调 | `workspace.rs`（command）`:97-202`；`agent_bridge/workspace.rs:109-268` | MC-10、MC-11 |
| GAP-MC-09 | trace 读查询耦合 CLI 输出 | MCP 无法复用 | `session.rs:407-584`；`checkpoint.rs:260-405` | MC-12、MC-20 |
| GAP-MC-10 | 无 HTTP 传输 | Grok Bot 无法接入 | — | MC-13 |
| GAP-MC-11 | 仅 legacy 纪元不足以覆盖 `2026-07-28` 客户端 | 新版客户端失败 | 规范 `2026-07-28/basic/versioning.mdx` | MC-14 |
| GAP-MC-12 | 无真实客户端联调证据 | 无法确认需求方的接入目标 | — | MC-15 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-long.md` | B 类路线「外部 Agent 观测、traces、hooks、bridge…可追溯协作」；MEM-04（经鉴权的 Memory MCP）；SB-02（非 loopback 强制认证、令牌不得来自 URL query） | 本计划是 B 类机器接口扩展；遵守 SB-02 判据；不触碰 MEM-04；成稿时在「日期计划索引」登记一行 |
| [`plan-20260818.md`](plan-20260818.md) | bridge（LB-01..06）来源计划，定义 GC-LB-02/04/06/07/08/10 | 复用其实现为库；不修改其协议面与表结构；MCP 使用独立表集（MC-03 建表、MC-04 按 source 选择表集），LB 全部测试保持绿 |
| [`plan-20260920.md`](plan-20260920.md) | 0.23.0 拆除 `libra code`（含 MCP `--stdio`）与 `rmcp`；DEFER-RC-04（独立 `libra mcp --stdio`）2026-09-20 以「完整移除 MCP」关闭 | 本计划是用户 2026-10-19 新提出的独立能力（只暴露仓库操作，不含执行器），不恢复旧模块与依赖；边界见 ADR-MC-03 |
| [`plan-20261018-grok-capture.md`](plan-20261018-grok-capture.md)（Grok CLI 会话捕获，与本计划的 Grok Bot 客户端无关） | GK-03..05 pending，可能触碰 `src/internal/db/migration.rs` | MC-03 对 `migration.rs` 的写入按 DEP-MC-01 串行 |
| [`plan-20261019-worktree-layout.md`](plan-20261019-worktree-layout.md) | 成稿时正在修改 `docs/commands/agent.md`、`COMPATIBILITY.md`、`src/cli.rs`、`src/command/control_lock.rs` 等，并已加入 migration `2026101902` | DEP-MC-03 串行；MC-03 的 migration 版本号在开工时取下一个空闲值 |
| `docs/development/tracing/deepseek-harness.md` | Harness 桥接事实源 | MC-03、MC-04 追加 MCP 表集与 `BridgeSource` 段，不改既有契约 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 用户明确需要标准 MCP 入口供 WorkBuddy（stdio）与 Grok Bot（远程 HTTPS）接入；复用 bridge 避免第二套写入实现 | 以 bridge 为库复用（GC-02） |
| 可行性 | `axum`、`uuid` 已在依赖中；所需 MCP 子集小；分表 migration 只建空表 | 41 张卡（MC-01..MC-43，MC-09 撤销；MC-42 已于 R48 并入 MC-06） |
| 任务卡粒度 | 每卡一个行为轴；AC 一行一个谓词；协议核心（MC-01）、bridge 分表（MC-04）、写会话（MC-06）、commit 工具（MC-08）、trace session（MC-12）、trace checkpoint（MC-20）、HTTP（MC-13）、modern（MC-14）、lease claim（MC-11）、lease renew（MC-38）、lease release（MC-41）十一张卡的验收是各自恢复轴上的门族，按用户批准的门族型豁免登记为 EX-MC-01..11；新增公开工具的卡为 `M` | 见 waiver 表与审计表 |
| 依赖与顺序 | 每张实现卡都新增 `[[test]]` 并改 `tests/INDEX.md`（I–I 相交），全串行 | 线性 DAG |
| 完整性 | 每卡含测试、EN/zh/网站文档、COMPATIBILITY 行内追加、INDEX；网站交付门写入 GC-MC-05；崩溃窗口有门 | ER-06a 逐文件列出 |
| 安全性 | 默认只读；显式 `--allow-write`；全部工具结果经结果信封；HTTP 只绑 loopback + Bearer + Origin/Host；Windows 不开放 HTTP；lease 只作用于当前工作树 | ADR-MC-05/06/07、GC-MC-03 |
| 功能正确性 | 版本协商、错误分层、重放、fence、分表隔离、崩溃不确定态、HTTP 状态码分别有门 | 见各卡 |
| 接口兼容 | bridge 协议与表结构零变化；Libra-only 新子命令；无新错误码 | GC-MC-01 |
| 数据流与控制流 | 写操作复用 `operation_id` 幂等；每 actor 单写进程（锁）；崩溃后 pending 按收敛器或 quarantine 处理 | ADR-MC-05/06 |
| 性能与容量 | 结果 ≤ 256 KiB、读 deadline 30 s（有界可取消执行，读并发 ≤ 4）、HTTP 并发读 16 + 等待队列 64、分页沿用 CLI 上限 | 「性能与容量摘要」 |
| 可靠性与容错 | 重放、重启、崩溃窗口、迁移中断有门 | 「故障恢复矩阵」 |
| 可维护性 | 协议常量、工具注册表、结果信封单一事实源；表名来自固定枚举 | GC-MC-02 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-10-19 08:01:54 UTC | 成稿 | 按 v2.14 新建计划，7 张卡 MC-01..MC-07（未提交草稿） | N/A | 全文 |
| 2026-10-19 10:38:26 UTC | Codex R1 FAIL（18×P1、4×P2）与用户批准 G-03 门族型豁免 | 重排为 12 张卡：输出脱敏、写会话、checkpoint 目标派生、commit/restore 拆卡、workspace 读/lease 拆卡、legacy HTTP 版本头、移除 `2025-03-26`、HTTP 按请求头路由、Windows 不开放 HTTP、错误分层、性能门、客户端联调；登记 EX-MC-01..06、DEP-MC-03..05 | 草稿 MC-01..07 → MC-01..12 | 全文 |
| 2026-10-19 11:01:16 UTC | Codex R2 FAIL（10×P1、2×P2） | 重排为 15 张卡（草稿未发布，编号整体重排）：MCP 改用独立表集 `agent_mcp_*`（不再放宽 Harness CHECK，两个 source 主键空间独立）；`operation_id` 对写工具必填；崩溃收敛改为「checkpoint 确定性收敛 + commit/restore 显式 quarantined」并新增崩溃窗口卡；renew/release 复核当前工作树；协议核心与 `libra_status` 拆卡、diff 的 checkpoint 模式移到 checkpoint 读卡；AC 一行一个谓词并重新计数；新增公开工具的卡改为 `M`；lib 测试改用完整路径并新增 GC-MC-06「恰好 1 个用例通过」执行规则；新增旧二进制兼容门；MC-15 改为记录子进程 PID；EX-MC-01..06 改挂到新编号（协议核心 MC-01、分表 MC-03、写会话 MC-05、trace MC-12、HTTP MC-13、modern MC-14），豁免范围与用户批准时一致 | MC-01..12 → MC-01..15 | 全文 |
| 2026-10-19 11:29:32 UTC | Codex R3 FAIL（10×P1、2×P2） | 仍为 15 张卡（草稿未发布，编号整体重排）：原 MC-03 拆为 MC-03（migration：新建 `agent_mcp_*` 五表含意图表 `agent_mcp_intent`，登记两个表归属注册表，A 组强制门与 GC-13 门列为不计数强制门，旧二进制门改为下载上一发布版实开新库）与 MC-04（implementation：bridge 按 `BridgeSource` 选择表集，EX-MC-02 改挂此卡，结构守卫改为可复制零命中命令）；原崩溃卡删除，commit/restore 的崩溃门并入 MC-08/MC-09，通用 quarantine 门留在写会话卡 MC-06；写锁移到共享存储并加跨 linked worktree 门；checkpoint 收敛器先补写链接；意图表保证 HEAD 前进后重放；checkpoint show/diff/restore 复核 actor；每卡生成逐文件 ER-06a 判定命令；网站线上核对改用 `https://libra.tools/en/docs/commands/agent` | MC-03 → MC-03、MC-04；MC-04..MC-08 → MC-05..MC-09；原 MC-09 删除 | 全文 |
| 2026-10-19 11:38:22 UTC | Codex R4 FAIL（9×P1、1×P2） | 卡数不变（15）：写会话卡 MC-06 增加跨 actor 仓库写锁与测试写工具 `libra_test_repo_write` 及两道互斥门；MC-15 的 C/D 覆盖改为预登记的计划收口点 CLOSE-MC；凡列网站页的卡自动加 `DEP-MC-02`；MC-03 旧二进制门改为按平台解析 artifact、断言版本与新库 receipt、`trap` 清理的脚本；MC-04 结构守卫先单独判定 `libra diff` 退出码；MC-04 digest 门拆两序、MC-12 transcript 门拆四工具、MC-07 外部 checkpoint 门只测另一 actor；landing/prod 由写集自动计算（MC-01 为 4、MC-06 为 3）；同卡文档门改为逐文件多 token 并加语义复核；MC-01 增加 `notifications/initialized` 静默门；新增 GC-MC-08 预检二进制规则 | 无拆并 | 审计表、MC-01/03/04/06/07/12/15、发布分组、GC |
| 2026-10-19 11:51:37 UTC | Codex R5 FAIL（5×P1、2×P2） | 15 → 18 张卡：MC-01 拆出结果信封 MC-16、MC-13 拆出并发准入 MC-17（G-01）；新增 MC-18「commit 的原子 HEAD 前提」（`CommitArgs.expected_parent` 内部 CAS，MC-08 改用 `vcs::commit_create_fenced`）；ADR-MC-06 明确 MCP 写锁只串行化 MCP 写入者，restore 与非 MCP 写入者的窗口登记 DEFER-MC-07 并写入文档警告；新增 GC-MC-09（nextest 与 `cargo test --all` 同时执行）；MC-15 进程门改为两阶段脚本（先证明 WorkBuddy 祖先与 PID/启动时间，再核对关闭）；意图表主键明确为 actor 限定 id 并在 MC-06 加两 actor 同客户端 id 门；旧二进制门改为精确比较 `--version`；基线版本注明 | MC-01 → MC-01、MC-16；MC-13 → MC-13、MC-17；新增 MC-18 | 实施顺序、Phase、审计表、waiver、ADR-MC-06、GC、追溯、测试矩阵、完成判据 |
| 2026-10-19 12:04:33 UTC | Codex R6 FAIL（8×P1、1×P2） | 18 → 19 张卡：撤销 MC-09（restore 工具，DEFER-MC-07，编号保留）；MC-16 拆出 MC-19（大小上限与读 deadline，MC-16 只留脱敏）；MC-12 拆出 MC-20（checkpoint trace，EX-MC-06 覆盖两卡）；MC-13 增加临时安全模式（并发 1、等待 30 s 后 503）及两道门；MC-18 增加确定性竞态门（暂停点 + 另一进程 CLI 提交，断言错误、ref、reflog），新 target `commit_fenced_race_test`（`#[serial(cwd)]`）；去掉 `libra_checkpoint_create` 的 `label`；`agent.mcp.request` span 归 MC-01 并加两道门（新 target `agent_mcp_span_test`）；MC-15 以随机探针分支证明 WorkBuddy/Grok 真实调用；MC-17 补现行 axum 锚点 | MC-09 撤销；MC-16 → MC-16、MC-19；MC-12 → MC-12、MC-20 | 实施顺序、Phase、审计表、waiver、ADR-MC-06/09、GAP、追溯、测试矩阵、风险、完成判据 |
| 2026-10-19 12:14:54 UTC | Codex R7 FAIL（8×P1） | 卡数不变（19）：MC-08 改 bridge 分发器按 source 路由（MCP → `commit_create_fenced`，Harness 不变）并以端到端 MCP-对-CLI 竞态门替换「调用前移动 HEAD」门；MC-13 临时模式改为忙即 503（不排队）；GC-MC-03/MC-19 改以完整 JSON-RPC 响应计量并用服务端生成的 `libra_test_blob` 测试；MCP session 明确为仓库级（`worktree_id` NULL）并在 MC-06 加三道跨工作树门；EX-MC-06 拆成 EX-MC-06（MC-12）与 EX-MC-07（MC-20）；MC-15 改用一次性探针仓库（不改本仓库）、探针名不入提示词并以 span `tool` 字段关联、联调后删除，补现行锚点；MC-01 span 增 `tool` 字段门 | 无拆并 | MC-01/06/08/13/15/17/19/20、waiver、GC-MC-03、ADR-MC-05 |
| 2026-10-19 12:29:01 UTC | Codex R8 FAIL（6×P1） | 卡数不变（19）：ADR-MC-04/MC-14 允许 stdio「先 `server/discover`、后 `initialize`」回退并加门，MC-14 同卡把 MC-01 未握手用例期望改为 `-32602`；MC-14 增加缺失 `Mcp-Method`/`Mcp-Name` 的四道门与现行锚点；MC-15 探针目录固定为 `$TMPDIR/libra-mcp-probe-{wb,grok}`，WorkBuddy/Grok 返回截取以命令比对探针分支，清理改为可执行命令，进程门拆为记录与核对两门；拆分 MC-06 AC-18、MC-08 AC-6（ref 判定留在 MC-18）、MC-13 AC-24、MC-18 AC-1、MC-19 AC-2 的复合谓词 | 无拆并 | MC-06/08/13/14/15/18/19、ADR-MC-04 |
| 2026-10-19 12:38:48 UTC | Codex R9 FAIL（5×P1、1×P2） | 卡数不变（19）：七个 EX 卡的「判据规范」逐一列名全部门；MC-01 span 门拆为数量/`method`/`id` 三门；MC-19 成功门改为单一谓词（`len` 回报）；MC-15 关闭核对门改为显式传入证据段记录的 PID/启动时间；MC-14 增加「modern `_meta` 但缺版本头 → 400 + `-32020`」两门并写入 ADR-MC-04；GC-MC-09 改为依 `CLAUDE.md` 计划驱动节以 nextest 为唯一证据、`cargo test --all` 仅诊断（取代 R5 时的双门口径）；刷新 `try_get_storage_path` 锚点 | 无拆并 | 全部 EX 卡、MC-01/14/15/19、GC-MC-09、CLOSE-MC、完成判据 |
| 2026-10-19 12:50:49 UTC | Codex R10 FAIL（4×P1、2×P2） | 卡数不变（19）：MC-01 span 字段改为只记已知方法名、`id_hash`、已注册工具名，不记 `clientInfo`，并加四道「标记不外泄」门；MC-14 不一致头增加 400 门、`resultType` 覆盖 discover 与工具成功/错误结果、`Mcp-Name` Base64 哨兵解码与两道门；MC-04 增加 `status.get`、`history.search`、`checkpoint.show`、checkpoint diff 的 Harness 隔离门；更新自审表的豁免摘要 | 无拆并 | MC-01/04/14、ADR-MC-05、自审表 |
| 2026-10-19 13:16:51 UTC | Codex R11 FAIL（3×P1、2×P2） | 19 → 20 张卡：新增 MC-21「HTTP 工具调用速率限制」（令牌桶 10/s、突发 20、超出 429）；MC-01 建立工具注册契约与注册表级 `outputSchema` 一致性测试（新 GC-MC-10，新 target `agent_mcp_conformance_test`，dev-dependency `jsonschema`）；MC-06 增加 120 s 写入硬期限（超时进程退出释放锁、下次启动 quarantine）及三道门；MC-04 定义 MCP 无 event 表（计数 0、ingress 拒绝）及两道门；MC-14 Base64 `Mcp-Name` 门改为断言工具结果与 `-32020` | 新增 MC-21 | 实施顺序、Phase、ADR-MC-06、GC-MC-10、MC-01/04/06/14、审计表 |
| 2026-10-19 14:02:49 UTC | Codex R12 FAIL（5×P1、1×P2）与用户批准 EX-MC-08 | 卡数不变（20）：MC-21 改为传输无关的核心限流（工具错误 `LBR-AGENT-029`），排到 HTTP 之前并加持续负载与豁免方法门，MC-13 把它映射为 429 并加三门（含 `initialize` 不限流）；MC-01 一致性测试按分支断言（成功示例非错误且符合成功分支、错误示例为错误且符合错误结构）；MC-08 登记 EX-MC-08 并加 hook 阻塞超期三门；MC-03 在 agent 命令文档 EN/zh 与网站页加升级/降级说明 | 无拆并 | 实施顺序、Phase、waiver、MC-01/03/08/13/21 |
| 2026-10-19 14:31:58 UTC | Codex R13 FAIL（4×P1、2×P2；前两次运行因模型容量错误中断，按 ER-10 退避后第三次得到结论） | 卡数不变（20）：MC-01 的未握手门改为只断言「是 JSON-RPC 错误」、生产工具门改为「无 `libra_test_` 工具」，均不随后续卡失效，MC-14 不再改 MC-01 用例；MC-17 同卡移除 MC-13 的三个临时模式用例并加零命中守卫；GC-MC-10 改为每工具一致性夹具（真实前置条件）；GC-MC-08 扩展为所有门命令都用兼容二进制（`PATH` 前置 `target/debug`）；MC-21 明确 stdio `initialize` 不存在耗尽后场景、HTTP 由 MC-13 验证；`axum` 锚点更正为 `Cargo.toml:101` | 无拆并 | MC-01/13/14/17/21、GC-MC-08/10、事实基线 |
| 2026-10-19 14:44:03 UTC | Codex R14 FAIL（3×P1、2×P2） | 20 → 22 张卡：MC-01 拆出 MC-22（请求 tracing span，8 门）；新增 MC-23「仓库 hook 的期限覆盖与进程组清理」（CLI 默认 15 分钟不变，超时 `SIGKILL` 进程组），MC-08 在其作用域内提交并加「hook 进程组已清理」门；MC-13 增加令牌强度三门（空、过短拒绝，256 bit 接受）并写入 ADR-MC-07；MC-15 探针目录改为 `mktemp -d` + 随机标记，清理先核对标记；性能/测试矩阵改以 MC-17 为最终树证据，MC-13 临时模式门标为历史 | MC-01 → MC-01、MC-22；新增 MC-23 | 实施顺序、Phase、waiver、ADR-MC-06/07、MC-01/08/13/15、测试矩阵、追溯 |
| 2026-10-19 14:54:53 UTC | Codex R15 FAIL（3×P1、2×P2） | 卡数不变（22）：MC-23 改为任务局部**绝对**期限（`deadline`/`reserve`，非正不启动 hook）并加一门，MC-08 以写调用的绝对终点与 10 s 保留调用之，hook 门改为「另一 actor 先持锁 3 s 后 hook 晚启动」且断言回收先于锁释放；MC-13 增加 `--generate-token`（CSPRNG、0600、不覆盖）与全零/周期令牌拒绝等六门；GC-MC-05 扩展到所列全部网站页（含 repository-hooks）并统一线上核对；MC-17 写明「先准入、取得槽位时才取令牌」并加一门与 debug-only 桶容量覆盖；MC-22 证据改为警示 bridge span 记录原始值、不得照搬 | 无拆并 | MC-08/13/17/22/23、GC-MC-05、ADR-MC-06/07 |
| 2026-10-19 15:09:30 UTC | Codex R16 FAIL（6×P1） | 22 → 23 张卡：新增 MC-24「commit 前提包含 HEAD 符号引用」（同 OID 分支切换竞态四门）；`--allow-write` 限 Unix（DEFER-MC-08），MC-06 加平台判定两门；令牌改为只接受 `--generate-token` 生成的 v1 格式（含校验段），`--generate-token` 为与传输互斥的独立模式并写入 ADR-MC-01；拆分 MC-06/MC-08/MC-23/MC-17 的复合谓词（MC-17 去掉由 MC-18 覆盖的父提交门以保持 8）；MC-04 测试文件改用全限定路径，守卫改为 `pipefail` + awk | 新增 MC-24 | 实施顺序、Phase、ADR-MC-01/06/07、非目标、MC-04/06/08/13/17/23、延后项 |
| 2026-10-19 15:22:57 UTC | Codex R17 FAIL（5×P1） | 卡数不变（23）：HTTP 令牌改为服务每次启动自行生成并写入 `--token-out` 新文件（`O_EXCL`、0600、轮换），删除 `--token-file`/`--generate-token` 与 v1 校验格式；MC-02 状态结果增加 `index_tree`，MC-24/MC-08 增加 `expected_tree` 前提（并发 `add` 被拒）；MC-23 hook 正常结束后也杀进程组，保证范围收窄为进程组内（DEFER-MC-09）并拆分 AC-1；MC-01/19/21 同卡更新 `docs/error-codes.md`（含门）；MC-21 AC-5 改为单一聚合谓词 | — | ADR-MC-01/06/07、非目标、MC-01/02/08/13/19/21/23/24、追溯、延后项 |
| 2026-10-19 15:35:12 UTC | Codex R18 FAIL（5×P1、1×P2） | 卡数不变（23）：MC-02 增加由不可变树得出的一致快照 `head`/`staged_changes` 与并发 `add` 暂停门；MC-23 改为任务局部 hook 拒绝策略（作用域外不变，含 CLI/Harness 回归门），MC-08 存在提交 hook 时拒绝（`hooks_present`）替换 hook 阻塞门，DEFER-MC-09 改为「MCP 下运行 hook」；D-MC-1 与 CLOSE-MC 要求 merge/记账 SHA 上的 `base.yml` success；ADR-MC-08 与完成判据改为要求 `docs/error-codes.md` 注记；ADR-MC-09 补齐 `libra_commit` 参数 | — | ADR-MC-06/08/09、GC-MC-04、CLOSE-MC、非目标、EX-MC-08、MC-02/06/08/23、追溯、测试矩阵、延后项 |
| 2026-10-19 15:47:52 UTC | Codex R19 FAIL（2×P1、1×P2） | 卡数不变（23）：`--token-out` 允许原子替换已存在的合格文件（非链接、本用户、0600、目录不可他写），同命令行可重启，并加正常退出/写入期限退出后重启与旧令牌失效门，文档化服务管理器与 TLS 终端的令牌交接；MC-23 增加 CLI/Harness `post-commit` 回归门，内置空 pre-commit 与 `commit-msg` 两门移至 MC-08（MCP 可见行为）；MC-13 临时模式门范围改为按用例名计算 | — | ADR-MC-07、目标、MC-08/13/23、追溯 |
| 2026-10-19 15:57:57 UTC | Codex R20 FAIL（2×P1、1×P2） | 23 → 24 张卡：MC-02 拆出 MC-25「`libra_status` 一致快照字段」——树 OID 在内存计算（`tree_plumbing` 不落盘变体），只读调用不写对象库（加对象库不变门），未出生 HEAD 定义为 `head = null`、基准空树（加三门）；MC-02 只留 `status` 概要并加未出生 HEAD 门；MC-08 改为依赖 MC-25；M2 版本数改为十一 | MC-02 → MC-02、MC-25 | 实施顺序、Phase、ADR-MC-06/09、MC-02/08/25、追溯、里程碑 |
| 2026-10-19 16:06:20 UTC | Codex R21 FAIL（2×P1、1×P2） | 24 → 25 张卡：MC-25 改为快照身份（增加 `head_ref`，detached 为 `null`；`index_tree` 与写入器同一过滤并加 intent-to-add 门），`staged_changes` 拆为 MC-26（同一过滤后的条目、比较 mode，加新路径/已跟踪路径 intent-to-add 与仅 mode 变化门）；MC-08 增加必填 `expected_head_ref` 与「status 与 commit 之间切分支」两门，MC-24 改为接受调用方给出的符号引用并在 fence 入口与事务内比较；收口清单补 MC-25/MC-26 文档 | MC-25 → MC-25、MC-26 | 实施顺序、Phase、ADR-MC-06/09、MC-08/24/25/26、追溯、里程碑、收口清单 |
| 2026-10-19 16:16:12 UTC | Codex R22 FAIL（5×P1） | 25 → 26 张卡：MC-24 把期望父提交改为 `ExpectedParent::{Commit, Unborn}`，MC-08 `expected_head = null` 映射 `Unborn` 并加首个提交与竞争首个提交三门；新增 MC-27「staged diff 绑定快照身份」（同一载入生成 patch 与 `index_tree`，含同路径换内容被拒门）；CLOSE-MC 改为先干净切换并快进本地 `main`、推送后核对远端 tip；MC-26/MC-27 全量触发器改为 T-5（GC-MC-01）；在 plan-status/plan-long 实际登记 | 新增 MC-27 | 实施顺序、Phase、ADR-MC-06/09、CLOSE-MC、MC-08/24/26/27、追溯、里程碑、登记 |
| 2026-10-19 16:27:36 UTC | Codex R23 FAIL（4×P1、1×P2） | 26 → 27 张卡：MC-27 移到 MC-08 之前并在 `src/command/diff.rs` 抽出接受 index 快照的共享入口 `diff_index_snapshot`（加并发重新暂存同一路径时 patch 与 `index_tree` 不变两门），提交被拒门移入 MC-08；`ExpectedParent::Unborn` 自 MC-24 拆出为 MC-28（入口/事务内、竞争首个提交三门）；CLOSE-MC 改为在 MC-15 写证据之前于干净树上切换并快进；自审可行性行改为 27 张卡 | MC-24 → MC-24、MC-28 | 实施顺序、Phase、ADR-MC-06、CLOSE-MC、自审、MC-08/24/25/27/28、追溯、里程碑 |
| 2026-10-19 16:37:38 UTC | Codex R24 FAIL（3×P1） | 27 → 28 张卡：新增 MC-29「读工具的有界可取消执行」（`spawn_blocking` + 容量 4 信号量、超时取消标志、槽位在任务返回后归还，diff 引擎文件/hunk 取消检查点，八门），排在 MC-19 之后、任何生产读工具之前，MC-05 改用可取消 diff；MC-27 改动 `src/command/diff/compare.rs` 使 `run_diff` 接受已捕获的 index 与 HEAD（prod 3），并增加 `review_complete`（脱敏或截断即 `false`）两门 | MC-19 → MC-19、MC-29 | 实施顺序、Phase、GC-MC-03、自审、MC-05/19/21/27/29、追溯、里程碑、清单 |
| 2026-10-19 16:52:40 UTC | Codex R25 FAIL（4×P1、1×P2） | 28 → 30 张卡：MC-29 只留读执行器，diff 取消检查点与单文件 10 000 行硬上限拆为 MC-31（含真实 diff 超时与槽位归还端到端门），排在任何生产读工具之前；`review_complete` 拆为 MC-30，在最终结果构造后计算并对路径过滤、省略 patch、页上限、二进制、脱敏、截断一律为 `false`（六门）；MC-26 增加 `status_consistent`（status 收集后复核快照，两门）；MC-14 的 `server/discover` 增加 `ttlMs = 0`、`cacheScope = "private"` 与固定 schema 校验三门；令牌长度改为「64 个十六进制字符」 | MC-29 → MC-29、MC-31；MC-27 → MC-27、MC-30 | 实施顺序、Phase、GC-MC-03、ADR-MC-04/07、自审、MC-05/08/13/14/21/26/27/29/30/31、追溯、里程碑、清单 |
| 2026-10-20 03:30:38 UTC | Codex R26 FAIL（3×P1、1×P2）与使用者裁决 | 卡数不变（30）：使用者裁决「bridge 原样」= 对外契约不变、允许内部重构（写入 ADR-MC-02 与 GC-MC-01）；MC-31 改为逐文件引擎（不再整组 `Diff::diff`）并增加 1 MiB 字节上限，加单行 2 MiB 省略、逐文件与整组 patch 等价、对抗性单文件有界三门（替换 CLI 大文件门）；MC-30 增加信封截断门；MC-26 标志改名 `head_index_unchanged` 并以「只改工作树时仍为 true」门写明覆盖范围 | — | ADR-MC-02、GC-MC-01、GC-MC-03、MC-05/26/30/31 |
| 2026-10-20 03:42:29 UTC | Codex R27 FAIL（5×P1、1×P2） | 卡数不变（30）：MC-29 改为读子进程执行器（隐藏 `--read-worker`，经正常 hash-kind 预检；超时 `SIGKILL`/`TerminateProcess` 并回收后归还槽位，覆盖 status、diff、rename 等全部读路径），六门含 SHA-256；MC-31 改为 MCP 专用有界 diff 入口：分配前尺寸预检（对象头/stat）、行数上限、一律关闭 rename 检测，八门；MC-30 改为由信封返回 `EnvelopeOutcome` 并经 `finalize` 回调写 `review_complete`（写集加 `output.rs`），加「路径含 secret」最终响应门；追溯表 MC-29/MC-31 行同步 | — | GC-MC-03、MC-05/29/30/31、追溯 |
| 2026-10-20 03:54:36 UTC | Codex R28 FAIL（6×P1、3×P2） | 30 → 33 张卡：新增 MC-32「对象尺寸（delta 感知）与工作树单句柄有界读取」、MC-33「status 有界收集（每列表 1000 条 + `omitted_count`）」、MC-34「信封 `EnvelopeOutcome` 与 `finalize` 后重新计量」；MC-29 读子进程改为只读预检（不迁移、不修复对象索引、不启动升级检查，四门）；MC-31 改用 MC-32 helper 并接受快照；MC-27 的 staged 快照只经有界入口（尺寸与 rename 两门）；MC-02 改用有界收集并加大仓库并发门；MC-30 消费 MC-34；追溯、恢复与锚点（`diff.rs:1011`）订正 | 新增 MC-32/33/34 | 实施顺序、Phase、里程碑、自审、MC-02/21/27/29/30/31/32/33/34、追溯、恢复表、清单 |
| 2026-10-20 04:05:34 UTC | Codex R29 FAIL（3×P1、2×P2） | 33 → 35 张卡：新增 MC-35「读子进程内存上限」（Linux `RLIMIT_AS`、macOS 50 ms 采样后 `SIGKILL`、Windows Job Object；超限 `LBR-AGENT-028` + `worker_memory_limit`，五门；更新 `docs/error-codes.md`），兜底 status 扫描与多文件 diff 的内存；工作树有界读取自 MC-32 拆为 MC-36，保持符号链接（不跟随）与 LFS 指针语义并加两门；MC-31 增加 256 KiB 全局收集预算（一门）并订正 AC-1 自足表述；MC-33 如实写明扫描内存由 MC-35 兜底；追溯行与写集对齐 | MC-32 → MC-32、MC-36；新增 MC-35 | 实施顺序、Phase、里程碑、自审、GC-MC-03、MC-31/32/33/35/36、追溯、清单 |
| 2026-10-20 04:15:21 UTC | Codex R30 FAIL（5×P1、1×P2） | 35 → 36 张卡：MC-33 重写为扫描路径预算（目录读取改用剩余预算、达到即标记未完成）+ 全部 16 个动态数组（逐一列名）上限，`omitted_count` 只在省略时出现（MC-02 等价门成立），新 target `agent_mcp_status_bounded_test`，八门含真实改名与不可读目录 fixture；MC-35 如实改为纵深防御并限定保证范围；MC-36 的 LFS 指针与正文都只用同一不跟随句柄并加替换竞态门；全局预算自 MC-31 拆为 MC-37（patch 字节 + 文件条目 1000 + 长路径结果大小，五门） | MC-31 → MC-31、MC-37 | 实施顺序、Phase、里程碑、自审、GC-MC-03、MC-02/05/30/31/33/35/36/37、追溯、清单 |
| 2026-10-20 04:25:47 UTC | Codex R31 FAIL（4×P1、1×P2） | 卡数不变（36）：`--allow-write` 必须显式 `--actor`（ADR-MC-05，文档示例 WorkBuddy/Grok 各用其 actor，MC-06 加一门）；读子进程把 worktree I/O 执行器设为纯进程内（`in_process_only`），MC-35 加「无后代进程」「超时后进程树为空」两门；MC-37 改为按条目整体（JSON 转义后）计入 1 MiB 预算并丢弃未保留的 `hunks`/`raw_diff`，加两门；MC-13 先绑定监听再发布令牌，加三门；MC-21 粒度记录与审计行同步 | — | ADR-MC-05/07、MC-06/13/21/35/37 |
| 2026-10-20 04:33:38 UTC | Codex R32 FAIL（4×P1、2×P2） | 卡数不变（36）：MC-13 改为整个生命周期持有 `<token-out>.lock` 独占锁后再绑定与发布令牌，加不同端口第二实例三门；MC-37 保留字节计入 `raw_diff` 与保留 `hunks`，长路径与转义路径各一计数器门；MC-36 为 LFS 哈希增加原始字节上限（64 MiB）并加大文件与增长两门，订正 `lfs.rs` 锚点；MC-08 预检后出现的 `post-commit` 以 `post_commit_skipped = true` 明示并加两门（MC-23 返回 `AdvisoryOutcome`）；MC-35 峰值门写明受控分配速率与采样容差 | — | ADR-MC-06/07、MC-08/13/23/35/36/37 |
| 2026-10-20 04:41:43 UTC | Codex R33 FAIL（3×P1、1×P2） | 卡数不变（36）：MC-13 无论新建或替换都要求令牌目录私有，锁文件以 `O_NOFOLLOW` 打开并校验、不复用 `acquire_control_lock`，加三门；MC-08 写集加 `commit.rs`/`vcs.rs`，`run_commit` 经 `CommitOutcome` 回传 post-commit 结果；MC-14 HTTP 不支持版本加 `-32022`、`data.supported`、`data.requested` 三门；MC-37 以新 target `diff_bounded_alloc_test` 的计数分配器测峰值堆（两门） | — | ADR-MC-07、MC-08/13/14/37、测试矩阵 |
| 2026-10-20 04:51:16 UTC | Codex R34 FAIL（4×P1） | 卡数不变（36）：MC-06 新增 `acquire_private_lock`（私有锁目录、`O_NOFOLLOW`、`fstat`），actor 锁移至 `mcp/actors/`（`repo` 不再冲突），加四门；MC-13 复用该 helper；MC-11 三个 lease 工具改为必填 `operation_id` 并经意图/operation 重放与确定性收敛（claim/release）或 quarantine（renew），以两个丢失响应重放门替换两个由注册表级门覆盖的门；ADR-MC-04 HTTP 先路由纪元再分派方法，MC-14 加 modern `initialize` 三门 | — | ADR-MC-04/06、MC-06/11/13/14 |
| 2026-10-20 04:58:38 UTC | Codex R35 FAIL（4×P1） | 36 → 38 张卡：lease 效果与 MCP operation 在同一 SQLite 事务内提交（`WorkspaceStore` 按 `_with_conn` 泛化），崩溃后 `pending` 必为未生效 → `failed`；MC-11 只留 claim（含重放与提交前崩溃门），renew/release 拆为 MC-38；ADR-MC-09 同步；MC-14 modern `initialize` 加 HTTP 404 门；按独立谓词重计：MC-02 并发门拆二、MC-36 大文件门拆二，LFS 部分拆为 MC-39 | MC-11 → MC-11、MC-38；MC-36 → MC-36、MC-39 | 实施顺序、Phase、里程碑、自审、ADR-MC-04/09、MC-02/11/12/14/31/36/38/39、追溯、清单 |
| 2026-10-20 05:08:14 UTC | Codex R36 FAIL（3×P1、2×P2） | 38 → 39 张卡：新增 MC-40「MCP lease 的工作树身份」（主工作树用保留身份 `main:<repository_id 的 SHA-256 十六进制>`，linked worktree id 字符集不含 `:`，四门），MC-11 改在主工作树验收、MC-38 加主工作树 renew 门；MC-15 探针的绝对路径与标记只存本机 env 文件（0600），证据只记 name、标记哈希前缀与分支，后续命令 `source` 本机文件；MC-33 AC-5 合并为单一等价谓词；lease 改用 `src/internal/workspace.rs` 既有 `*_with_conn`（MC-11 写集订正）；成功定义写明 lease 重放契约 | 新增 MC-40 | 成功定义、实施顺序、Phase、里程碑、自审、MC-11/15/33/38/40、追溯、清单 |
| 2026-10-20 05:17:05 UTC | Codex R37 FAIL（2×P1、1×P2） | 卡数不变（39）：MC-40 对读出的 linked worktree id 做保留命名空间校验并在主身份冲突时 fail-closed（两门，经真实元数据读取路径）；MC-38 renew 增加可选 `lease_ttl_ms`，缺省取 claim 时的 TTL 且永不缩短到期时间（一门）；lease 提交前崩溃的表述改为「不留任何行、重试按新请求执行；提交后响应丢失则重放」，MC-11 崩溃门同时断言无 operation/intent 行 | — | ADR-MC-09、MC-11/38/40 |
| 2026-10-20 05:26:20 UTC | Codex R38 FAIL（3×P1、1×P2） | 39 → 40 张卡：MC-03 的 migration 增加 `agent_mcp_intent.result_json`（首次结果在 `applied` 同一事务内持久化）与 `agent_mcp_lease`（claim 的 TTL），表集合改为六表；MC-11 claim 缺省 TTL 600000 ms 并写入两处持久化；MC-38 只留 renew（owner 须等于可信 actor 派生值、缺省 TTL 取 claim 记录，延时续期门替换「不缩短」门），release 拆为 MC-41（含跨 actor 门）；两卡与 MC-11 的文档门加 `lease_ttl_ms`、缺省值与「不会缩短」；MC-40 改为 `Result<String, BridgeError>` 并带 registry 连接 | MC-38 → MC-38、MC-41 | 成功定义、实施顺序、Phase、里程碑、自审、ADR-MC-09、MC-03/11/12/38/40/41、追溯、清单 |
| 2026-10-20 05:36:54 UTC | Codex R39 FAIL（3×P1、1×P2）与使用者批准 EX-MC-09 | 卡数不变（40）：三个 lease 工具各加「提交前崩溃后意图与 operation 行也回滚」与「同一 `operation_id` 重试成功」两门，renew 另加提交前崩溃保持到期时间与显式更短 TTL 不缩短两门；MC-11/MC-38 按使用者批准登记门族型豁免 EX-MC-09/EX-MC-10；MC-38/MC-41 文档写全 `workspace_id` 等必填参数并加门，claim 网站门加 `600000`，renew 网站门加 `lease_ttl_ms`；MC-11/MC-40 中 release 归属改为 MC-41 | — | 豁免表、自审、MC-11/38/40/41 |
| 2026-10-20 05:44:54 UTC | Codex R40 FAIL（2×P1、2×P2） | 卡数不变（40）：`outputSchema` 根增加 `"type": "object"`（GC-MC-10、MC-01），MC-01 加 `tools/list` 对 2025-11-25 与 2025-06-18 两个固定 schema 的校验两门；MC-11 崩溃门加 `agent_mcp_lease` 回滚与重试后恰一行两门；MC-38/MC-41 的文档门改为按工具小节检查完整参数（源文件与线上页面）；恢复矩阵订正为 MC-15 AC-5 | — | GC-MC-10、MC-01/11/38/41、恢复矩阵 |
| 2026-10-20 05:53:59 UTC | Codex R41 FAIL（1×P1、2×P2） | 卡数不变（40）：三个 lease 工具的意图另存可信工作树身份，重放前复核、跨工作树重放以 `LBR-AGENT-032` 拒绝（每卡一门）；新增 `tests/tools/doc_section_check.py`（DOM 小节提取 + 单行 HTML 自检，MC-11 交付），claim/renew/release 的线上页面门改为按 DOM 小节检查完整参数，claim 小节门加 `operation_id`/`lease_ttl_ms`/`600000` | — | ADR-MC-09、MC-11/38/41 |
| 2026-10-20 06:03:26 UTC | Codex R42 FAIL（2×P1、1×P2） | 卡数不变（40）：MC-13 令牌锁移到保留命名空间 令牌父目录中的 `.libra-mcp-token-<hash>.lock`，令牌路径不得落在该命名空间或仓库存储内，加三门（含 `token`/`token.lock` 冲突）；MC-06 增加写记录配额（每 actor 50 000）与 7 天保留期、超限 `LBR-AGENT-035`（`write_quota`），加四门并写入文档与容量表；`doc_section_check.py` 增加 Markdown 模式（同级或更高级标题结束小节，`###` 自检 fixture），三张 lease 卡的源文件小节门改用之 | — | ADR-MC-07、MC-06/11/13/38/41、容量表 |
| 2026-10-20 06:14:31 UTC | Codex R43 FAIL（5×P1） | 卡数不变（40）：MC-13 只在已存在文件的哈希等于令牌锁记录（本服务发布且未改动）时替换，加两门；MC-06 配额改为覆盖全部 MCP 行的 operation 组（每 actor 50000 组、载荷 256 MiB）与 30 天整组删除（复用 `operation_id` 不绑定旧 checkpoint），拆分保留门并加整组删除、复用、字节配额、持续写入四门；配额、保留期与 `write_quota` 写入 EN/zh/网站与 `docs/error-codes.md` 并加门 | — | MC-06/13、容量表 |
| 2026-10-20 06:26:45 UTC | Codex R44 FAIL（7×P1、2×P2） | 40 → 41 张卡：撤回 MC-06 中的按 actor 配额与自动删除（引发 lease 续期、重放与 G-01 问题），MC-06 只加 `operation_id` ≤ 128 字节门；新增 MC-42「仓库级写记录容量上限」（全部 actor 合计 100000 组、载荷 512 MiB，计入 MCP 创建的 `workspace_record`，不自动删除，清理命令 DEFER-MC-10，七门）；MC-13 令牌哈希改为 `pending`/`current` 两阶段并加两个崩溃重启门；MC-14 AC-11/12 指明 modern `tools/list` | MC-06 → MC-06、MC-42 | 实施顺序、Phase、里程碑、自审、容量表、延后项、MC-06/07/13/14/42、追溯、清单 |
| 2026-10-20 06:34:12 UTC | Codex R45 FAIL（6×P1、1×P2） | 卡数不变（41）：MC-13 首次创建同样走 `pending`→文件→`current` 并加两个崩溃门；MC-03 增加单行用量表 `agent_mcp_usage`（七表）；MC-42 重写——组上限 10000、用量单行计数 O(1)、写前预留 256 KiB 结果上界、接入点为 MC-06 的 `session.rs`，加预留与单行读取两门；renew/release 对已存在 lease 不受组上限阻塞（MC-38 每 lease 保留最近 16 条 renew 记录，MC-38/MC-41 加配额下结算门），MC-11 加 claim/release 循环计入配额与 registry 行数两门；MC-06 字符集统一为 `[A-Za-z0-9._:-]` 并加 `:` 接受门 | — | ADR-MC-05、容量表、MC-03/06/11/13/38/41/42 |
| 2026-10-20 06:44:49 UTC | Codex R46 FAIL（5×P1、1×P2） | 卡数不变（41）：MC-42 重写——写入意图的同一事务即计入一组并预留字节（失败组同样计入）、首次打开时在写锁下回填旧记录、claim 预留 16 次 renew + 1 次 release 的结算额度（结算不再受配额阻塞），八门；MC-38 不再删除 renew 记录，改为每 lease 至多 16 次 renew（第 17 次 `renew_limit`），重放始终返回首次结果；MC-13 首次创建与轮换都 fsync 令牌文件与目录、锁文件创建 fsync 目录，加顺序单元门 | — | 成功定义、MC-13/38/41/42 |
| 2026-10-20 06:53:55 UTC | Codex R47 FAIL（4×P1、2×P2） | 卡数不变（41）：用量计数前移到 MC-06（自第一个可写版本起计数，加组数与实际字节两门），MC-42 去掉回填与回填门、只强制上限，并改称「记录载荷上限」（不含 SQLite 页/索引/WAL）；MC-13 轮换顺序改为写→fsync→rename→fsync 目录并加顺序单元门；GC-MC-05 编辑前要求本地无领先提交、推送前断言领先提交恰为本卡一条；D-MC-4 安装脚本先完整下载再检查 | — | GC-MC-04/05、MC-06/13/42、容量表 |
| 2026-10-20 07:06:10 UTC | Codex R48 FAIL（4×P1） | 41 → 40 张卡：MC-42 并入 MC-06（容量计数与上限必须与第一个写工具同版本生效；EX-MC-03 门族范围同步说明），MC-42 编号保留；MC-38/MC-41 所有拒绝条件在写入意图之前判定并各加「重复被拒不写行」门；MC-13 令牌哈希改存同目录状态文件并整体原子替换，加撕裂写崩溃门；GC-MC-05 改为以私有唯一本地分支固定已核验提交后 `push <私有分支>:cf` | MC-42 并入 MC-06 | 实施顺序、Phase、里程碑、自审、豁免表、GC-MC-05、MC-06/07/11/13/38/41、追溯、容量表、清单 |
| 2026-10-20 07:27:40 UTC | Codex R49 FAIL（2×P1） | 卡数不变（40）：MC-13 首次创建改为写完整临时文件并 fsync 后以 `link` 原子发布（不替换任何已存在文件），加部分写崩溃门；GC-MC-05 增加 ①′ 目标路径编辑前必须干净与 ⑤ 提交前暂存区精确等于本卡路径，由 MC-01 交付的 `tests/tools/website_preflight.sh`（含脏目标与多余暂存两种自检）执行 | — | GC-MC-05、MC-01/13 |
| 2026-10-20 07:40:33 UTC | Codex R50 FAIL（5×P1） | 卡数不变（40）：令牌首次创建在 ADR-MC-07 与 MC-13 中统一为「临时文件 + `link`」；GC-MC-05 改为在本卡专用临时克隆中交付（不建分支，符合网站 `AGENTS.md`；克隆私有，排除其它会话改动），只保留暂存路径预检；MC-01 把预检脚本自检计入 AC（两门）；lease 结算每对请求+结果合计 ≤ 2 KiB（请求 ≤ 1 KiB 在意图前校验），MC-38 加超长请求门、MC-41 加最大尺寸结算门 | — | ADR-MC-07、GC-MC-05、MC-01/06/13/38/41 |
| 2026-10-20 08:00:57 UTC | Codex R51 FAIL（6×P1） | 卡数不变（40）：GC-MC-05 改回按模板 ER-06a 的 Libra 相邻仓库路径（VCS 类型/分支/领先提交/目标路径干净前置检查，审核快照与暂存内容逐字节比较，以轻量 tag + 完整 ref `refs/tags/<t>:refs/heads/cf` 固定推送；三种失败情形的一次性演练为全局门），撤回私有克隆与预检脚本（MC-01 去掉两门，EX-MC-01 范围恢复原状）；ADR-MC-07 令牌来源改为状态文件；lease 意图改存 lease 身份与根目录摘要（与路径长度无关），MC-41 加长路径结算门 | — | GC-MC-05、ADR-MC-07、MC-01/11/38/41 |
| 2026-10-20 08:12:05 UTC | Codex R52 FAIL（4×P1） | 卡数不变（40）：MC-40 对不满足 `^[A-Za-z0-9-]{1,64}$` 的 linked id 取 `wt-<SHA-256>` 有界身份（加一门），MC-41 加 4000 字节元数据 id 的 claim→renew→release 门；GC-MC-05 的 `P` 改为相对网站仓库根的路径，审核快照改为 shell 中的 SHA-256 摘要（不落盘补丁），演练加实际页面路径的正例；MC-38 文档写明 1 KiB 请求上限与 `LBR-AGENT-027` 并加入小节门 | — | GC-MC-05、MC-38/40/41 |
| 2026-10-20 08:24:11 UTC | Codex R53 FAIL（4×P1） | 卡数不变（40）：主工作树身份改为 `main:<repository_id 的 SHA-256>`（69 字节，MC-40 加长仓库身份门，MC-41 加长仓库身份结算门）；GC-MC-05 与 CLOSE-MC 的提交写明 `-s -m`；MC-15 探针状态只留在同一 shell 会话的环境变量中（脚本以 `.` 执行，不写状态文件）；MC-38 线上页面门加 `1 KiB` 与 `LBR-AGENT-027` | — | GC-MC-05、CLOSE-MC、成功定义、MC-11/15/38/40/41 |
| 2026-10-20 08:36:46 UTC | Codex R54 FAIL（3×P1、1×P2） | 卡数不变（40）：GC-MC-05 在 `set -euo pipefail` 下执行、命令替换先赋值再比较，审核改动须非空，演练加故障注入（替身 `libra` 失败时各步中止）；MC-03 的 CHECK 改为 `length(CAST(… AS BLOB))`，MC-06 写前按 UTF-8 字节校验并加多字节边界门；MC-06 线上页面门加 `write_quota`/`10000`/`512 MiB`，错误码文档改列为受影响 | — | GC-MC-05、MC-03/06 |
| 2026-10-20 08:45:04 UTC | Codex R55 FAIL（2×P1、1×P2） | 卡数不变（40）：GC-MC-05 提交后与推送后的 SHA 比较改为先赋值、非空再比较，并纳入故障注入演练；MC-41 明确 renew 次数上限不影响 release；锚点刷新为 `util.rs:1233`（`try_get_storage_path`）与 `migration.rs:1738`（`repository_migrations`） | — | GC-MC-05、事实基线、MC-03/06/41 |
| 2026-10-20 09:10:05 UTC | Codex R56 FAIL（1×P1、1×P2） | 卡数不变（40）：MC-40 AC-4 只保留更强的共存情形（单一谓词）；锚点刷新：`compare.rs:958`（HEAD 读取）、`diff.rs:480`（`DiffFileStat`）、`:486-487`（`raw_diff` 跳过序列化）、`diff.rs:1018`（`run_diff_for_service`） | — | MC-27/29/31/37/40 |
| 2026-10-20 09:22:12 UTC | Codex R57 FAIL（1×P1、1×P2） | 卡数不变（40）：MC-15 AC-3/AC-7 的分支名先单独赋值且非空再搜索，并加故障注入门（失败的 `libra` 替身使判定以非 0 结束）；AC-8 改为「至少一条且与观测时间相关联，重试另行记录」 | — | MC-15 |
| 2026-10-20 09:28:43 UTC | Codex R58 FAIL（2×P1） | 卡数不变（40）：修复编辑脚本在改写 Verification 时丢弃多行脚本块的缺陷（R57 修订误删 MC-15 的四个脚本），自 v57 恢复 MC-15 全部脚本（含删除前的标记核对）后重做 R57 改动；AC-3/AC-7 改为以 `&&` 串联分支查询，故障注入替身改为打印可匹配文本后以 1 退出 | — | MC-15 |
| 2026-10-20 09:37:12 UTC | 使用者要求 | 卡数不变（40）：在开篇增加术语强调——「Grok Bot」专指经远程 HTTPS MCP 接入的客户端工具，本计划不接入、调用或捕获 Grok 模型，也与 `plan-20261018-grok-capture` 无关；ADR-MC-07、GAP-MC-01、上下游表与 DEP-MC-01 同步措辞；MC-15 探针变量由 `GROK_*`/`NAME=grok` 改为 `GROKBOT_*`/`NAME=grokbot` | — | 开篇、ADR-MC-07、GAP-MC-01、DEP-MC-01、MC-15 |
| 2026-10-20 09:41:03 UTC | Codex R59 FAIL（3×P1、1×P2） | 40 → 41 张卡：MC-15 探针脚本的分支查询与标记读取改为先赋值、检查退出码再比较，加标记不可读的故障注入门；ADR-MC-05 如实写明两客户端可同时以写权限运行但写调用经写锁串行（等待 ≤ 30 s，超时 `LBR-AGENT-036`），bridge 不受 MCP 写锁影响，MC-13 加两客户端与 bridge 同仓库并发两门；`expected_tree` 自 MC-24 拆为 MC-43；DEFER-MC-10 触发条件改指 MC-06 上限 | MC-24 → MC-24、MC-43 | ADR-MC-05、实施顺序、Phase、里程碑、自审、MC-08/13/15/24/25/26/28/43、追溯、延后项、清单 |
| 2026-10-20 09:48:55 UTC | Codex R60 FAIL（4×P1、2×P2） | 卡数不变（41）：GC-MC-05 的审核摘要、暂存路径与暂存内容比较都改为先检查赋值再比较，故障注入改为「打印期望值后失败」并覆盖这两项；MC-15 AC-10 先检查赋值并加 20 s 超时，AC-5 区分 ESRCH 与查询失败；MC-13 令牌锁标识定义为 canonical 父目录 + 校验后的文件名，加缺失文件与别名父目录两门；`expected_tree` 归属改为 MC-43（ADR-MC-06、MC-25、MC-26）；MC-13 写串行归属改为 MC-06 | — | GC-MC-05、ADR-MC-06/07、MC-13/15/25/26 |
| 2026-10-20 10:00:45 UTC | Codex R61 FAIL（3×P1、1×P2） | 卡数不变（41）：令牌锁与状态文件移到令牌自身私有父目录的保留文件名（`.libra-mcp-token-<hash>`，与 `LIBRA_HOME` 无关，保留前缀不得作令牌名），加不同 `LIBRA_HOME` 与保留名两门；HTTP 请求体 10 s 绝对接收期限（408），准入槽位在请求体收完后才获取，加停滞请求体两门，MC-17 同步；GC-MC-05 同步脚本改为 `--write --only <页>`，其它页漂移即停止；D-MC-1 记录 `refs/pull/<n>/merge` 合成 SHA 与 `pull_request.head.sha` | — | ADR-MC-07、GC-MC-04/05、MC-13/17 |
| 2026-10-20 10:09:01 UTC | Codex R62 FAIL（4×P1） | 卡数不变（41）：MC-13 接收改为连接准入（64 个连接许可，耗尽即 503）+ `hyper-util` 连接层 `header_read_timeout(10 s)` + 自头部读完起不重置的 10 s 请求体绝对期限（408），不再依赖 `tower_http`；`hyper`/`hyper-util` 加为直接依赖并入写集；滴流请求体、停滞头部、连接上限三门；令牌锁文件名统一为父目录中的 `sha256(文件名)` | — | MC-13/17 |
| 2026-10-20 10:17:45 UTC | Codex R63 FAIL（3×P1、1×P2） | 卡数不变（41）：MC-13 改为只服务 HTTP/1.1（`hyper::server::conn::http1::Builder`，不启用 `server-auto`），HTTP/2 前言被关闭并加一门；MC-02/08/11/13/38/41/43 中「成功且 X」的复合判据改写为以成功为前提的单一谓词（MC-15 同步）；ADR-MC-03 改为「不引入 MCP SDK」并列明允许的直接依赖 | — | ADR-MC-03、MC-02/08/11/13/15/38/41/43 |
| 2026-10-20 10:30:15 UTC | Codex R64 PASS（0×P1、1×P2） | 卡数不变（41）：按 R64 唯一的 P2 修正 MC-13 的 HTTP 内存预算表述——计入 Hyper 连接读缓冲（默认约 400 KiB，hyper#4081 的超额读分配不当作严格上限），改为每连接 ≤ 1 MiB、合计 ≤ 64 MiB 的容量估计；不新增判据（R65 曾尝试以 `max_buf_size` 封顶，被确认在 Hyper 1.8.1 下不可靠，已撤回）；该改动经 R66 确认 | — | MC-13 |
| 2026-10-20 12:40:00 UTC | 使用者要求保持 0.30.xx，且 stable manifest 仍是 0.30.66 | 工作区版本面是未发布的 0.31.1（本地 tag `v0.31.0` 存在，stable `manifest-v1.json` 的 `version` 为 `0.30.66`，control_revision 167）。发布端点拒绝 `version_regression`，因此不把版本面升到 0.31.2，也不改写已发布 tag。MC-01 起把三个版本面从 0.31.1 调回 0.30.67（相对已发布 stable 单调 +1）。 | — | 版本面 |
| 2026-10-20 13:05:00 UTC | `v0.30.67` 的 Linux 产物已上传，Windows 库测试缺少 Tokio `io-std` | `72450c6` 的 arm64/amd64 Linux 对象已在 CDN（HTTP 200），前缀不可覆盖。Windows `cargo test --locked --lib` 在 `mcp.rs` 报 `E0432`（`tokio::io::stdin` 仅由 Unix 依赖间接打开）。取消未完成的 darwin/windows 上传，版本面改为 `0.30.68`，并在 `Cargo.toml` 直接启用 `io-std`。不改写 `v0.30.67`。 | — | 版本面、`Cargo.toml` |
| 2026-10-20 14:05:00 UTC | MC-01 D 组通过 | `v0.30.68` 的 `release.yml` 39427212730 success。main `e4d843d` 的 `base.yml` 39427673509 attempt 2 与 `codeql.yml` 39427673357 success。Codex 与 Claude Code 均为 P0=0、P1=0。MC-01 `done` / `complete`。 | — | MC-01 |
| 2026-10-20 17:50:00 UTC | MC-22 D 组通过 | `v0.30.69` 的 `release.yml` 39440601807 success。main `3b1200a` 的 `base.yml` 39459099628 与 `codeql.yml` 39459099663 success。Codex `VERDICT: PASS`，`P0=0`，`P1=0`。MC-22 `done` / `complete`。 | — | MC-22 |

## 已决议设计决策

实现时若需偏离本节，必须先修改计划并说明原因，不得在代码中静默改语义。

### ADR-MC-01: 入口为 `libra agent mcp`，不新增顶层命令

- **Status:** Accepted
- **Context:** 用户给出 `libra mcp --stdio` 或 `libra agent mcp --stdio` 两个候选。顶层命令需要新增 `COMPATIBILITY.md` 行（compat ledger 以绝对行号锚定，增删行会使 ledger 测试失效）、根 help Command Groups 行与新的 `docs/commands/mcp.md`。
- **Decision:** 采用 `libra agent mcp`，与 `libra agent bridge` 并列；`COMPATIBILITY.md` 只在既有 `agent` 行（第 252 行）内追加文字。`--stdio` 与 `--transport <stdio|http>` 二选一，`--stdio` 等价 `--transport stdio`；两者都缺省或互相冲突时以用法拒绝 `LBR-CLI-002` 结束。
- **Alternatives considered:** 顶层 `libra mcp`（拒绝：增加兼容矩阵行与根命令组）；`agent bridge --protocol mcp`（拒绝：违反「bridge 原样」）。
- **Consequences:** 客户端配置写 `command = "libra"`、`args = ["agent", "mcp", "--stdio"]`、`cwd = <仓库目录>`。
- **Revisit when:** MCP 成为 Libra 主要机器接口且需要独立文档树时。

### ADR-MC-02: MCP 服务端与 bridge 是两个前端、一个后端

- **Status:** Accepted
- **Context:** Harness 插件消费 bridge 的固定 fixture；任何协议或数据语义变化都会破坏 Harness。需求「保留现有 `libra agent bridge --stdio` 原样」与「内部复用 checkpoint、commit、workspace、trace 等仓库操作能力」需要一致解释：**使用者已于 2026-10-20 会话内裁决（Codex R26 后，选项「对外契约不变」）**——「原样」指 bridge 的对外契约（NDJSON 协议、方法 allowlist、参数与结果、错误目录、行为、Harness 的数据与 `agent_bridge_*` 表）完全不变，允许为复用而重构 bridge 内部代码，以全部 `agent_bridge_*` 回归门守护（GC-MC-01）。
- **Decision:** `src/internal/ai/mcp_server/` 是独立前端：它把 MCP tool 调用翻译成内部 `BridgeRequest`（由服务端构造，客户端参数只进入 `params` 的白名单字段），在 `BridgeSource::Mcp` 上下文中调用 `agent_bridge::{methods,mutations,workspace}::dispatch` 与 `vcs` 函数。bridge 的 allowlist、协议版本、上限常量、错误目录、NDJSON 传输、`agent_bridge_*` 表结构与 Harness 的 SQL 文本不改；MCP 自己的协议常量放 `mcp_server/protocol.rs`，数据放 `agent_mcp_*`。
- **Alternatives considered:** 复制 bridge 写逻辑到 MCP 模块（拒绝：GC-02）；让 bridge 同时说 MCP（拒绝：违反「原样」）；放宽 Harness 表的 source CHECK 共用一套表（拒绝：主键全局，同名标识会让 Harness 行为随 MCP 写入而变化）。
- **Consequences:** 需要新建 MCP 表集（MC-03）并让 bridge 按 source 选择表集（MC-04）；每张卡的全量门覆盖全部 `agent_bridge_*`。
- **Revisit when:** bridge 协议升到 v2。

### ADR-MC-03: 自写最小 MCP 服务端，只实现 tools 能力

- **Status:** Accepted
- **Context:** `rmcp` 等依赖已在 plan-20260920 中删除；MCP 在 `2026-07-28` 引入无状态大改，SDK 波动大；所需子集很小。
- **Decision:** 不引入任何 MCP SDK（如 `rmcp`）；协议按 pin 的规范自写。允许的新增直接依赖仅限：dev-dependency `jsonschema`（MC-01，工具结果 schema 一致性测试）与 `hyper`/`hyper-util`（MC-13，HTTP/1 连接层期限与准入；二者已在依赖图中，只改为直接依赖）。按 pin 的规范（`modelcontextprotocol/modelcontextprotocol@1155ca46`）自写：legacy 纪元的 `initialize`、`notifications/initialized`、`ping`、`tools/list`、`tools/call`、`notifications/cancelled`（收到即忽略，规范允许）；modern 纪元的 `server/discover` 与每请求 `_meta`（MC-14）。服务端能力只声明 `tools`（`listChanged: false`）。与 plan-20260920「完整移除 MCP」的边界：被删的是 Code 执行器的 MCP 面；本计划新增的是只读/受控写的仓库操作面，不含模型执行、不含 resources/prompts。
- **Alternatives considered:** 重新引入 `rmcp`（拒绝：依赖重、需要已删的 `tower`/`hyper-util`、对 `2026-07-28` 支持未知）。
- **Consequences:** 规范一致性由本计划门族保证；新规范版本需要新卡。
- **Revisit when:** 需要 resources/prompts/tasks 等能力时。

### ADR-MC-04: 协议版本与纪元路由

- **Status:** Accepted
- **Context:** 现网客户端多为 legacy（handshake）纪元；`2025-03-26` 要求接收 JSON-RPC batch；`2026-07-28` 去掉了 `initialize` 与会话，HTTP 不再有 `Mcp-Session-Id`。
- **Decision:** legacy 支持集合固定为 `["2025-11-25", "2025-06-18"]`：`initialize` 请求其中之一则原样回显，否则回 `2025-11-25`。modern 支持集合 `["2026-07-28"]`（MC-14）。JSON 数组形式的 batch 一律以 `-32600` 拒绝。纪元路由：**stdio** 按连接——`server/discover` 不确定纪元；在第一次成功处理非 `server/discover` 的 modern 请求之前，收到 `initialize` 即该连接进入 legacy（支持「先探测、后回退」的客户端）；其余请求按 `_meta` 走 modern（MC-01 发布时尚无 modern 纪元，未握手请求一律 `-32600`；MC-14 起改为按 modern 规则以 `-32602` 拒绝缺少 `_meta` 的请求）。**HTTP** 按请求、不依赖任何连接或进程状态，且**先路由纪元、后分派方法**（包括 `initialize`）：版本头为 modern 值、或版本头缺失而 `_meta` 声明 modern 版本时，请求按 modern 规则校验与处理——modern 下 `initialize` 不是方法，得到 HTTP 404 与协议错误 `-32601`，缺版本头则 400 + `-32020`；只有路由为 legacy 的 `initialize` 才按 legacy 握手应答（无状态，不签发会话）。其它请求按 `MCP-Protocol-Version` 头路由：legacy 版本 → legacy 处理（HTTP 上不要求先前 `initialize`），modern 版本 → modern 处理（另要求 `Mcp-Method`，`tools/call` 另要求 `Mcp-Name`，缺失或不一致 → 400 + `-32020`）；版本头缺失时看请求体：`_meta` 声明 modern 版本 → 400 + `-32020`（HeaderMismatch），否则按 legacy 规则 → 400；版本头为不支持值 → 400（MC-13 处理 legacy 头，MC-14 增加 modern 头）。 modern 的 `server/discover` 与 `tools/list` 结果都带 `ttlMs` 与 `cacheScope`（`server/discover`：`ttlMs = 0`、`cacheScope = "private"`），`DiscoverResult` 以固定提交的 `schema/2026-07-28/schema.json` 做 JSON Schema 校验（MC-14）。
- **Alternatives considered:** 支持 `2025-03-26` 并实现 batch（拒绝：batch 在 `2025-06-18` 已删除）；HTTP 用进程级「已初始化」标志（拒绝：多客户端相互污染）。
- **Consequences:** 两个纪元共用 tool 注册表与执行路径，只在信封层分叉。
- **Revisit when:** 规范发布新版本或删除 legacy 兼容。

### ADR-MC-05: 身份、会话与独立表集

- **Status:** Accepted
- **Context:** GC-LB-06/07 要求 scope 与 actor 来自可信上下文；MCP `clientInfo` 是客户端自报；bridge 的 `operation_id`、`checkpoint_id` 是全局主键，且 bridge 不限制 Harness 标识的取值，任何命名约定都无法保证与 Harness 不撞；mutation 参数摘要覆盖全部参数，因此会话标识必须跨重启稳定才能重放。
- **Decision:** MCP 使用与 bridge 同构的独立表集 `agent_mcp_session`/`agent_mcp_operation`/`agent_mcp_checkpoint`/`agent_mcp_link`，以及 MCP 专有的意图表 `agent_mcp_intent`（MC-03 建表，MC-04 让 bridge 按 `BridgeSource` 选择表集），Harness 继续使用 `agent_bridge_*`，两个 source 的主键空间完全独立。`--actor`（只允许 `[A-Za-z0-9._-]{1,64}`）是唯一身份来源；`--allow-write` 时**必须显式给出** `--actor`（无缺省，缺失以用法拒绝 `LBR-CLI-002` 结束），只读进程缺省为 `mcp` 且不取写锁。不同客户端各用自己的稳定 actor（文档示例：WorkBuddy 的 stdio 配置 `--actor workbuddy`，Grok Bot 所连 HTTP 服务 `--actor grok`），因此二者可在同一仓库同时以写权限**启动并运行**（同 actor 互斥只防同一客户端重复启动）；但二者的**写调用**经 ADR-MC-06 的 MCP 仓库写锁串行化：一方写入期间另一方的写调用最多等待 30 s，超时为可重试的工具错误 `LBR-AGENT-036`；读调用不受写锁影响；Harness 的 `agent bridge` 不取 MCP 写锁，其读写与 MCP 互不阻塞（仅在同一 HEAD 上以各自的 CAS 前提相互拒绝，见 ADR-MC-06）。每个 actor 一个**稳定、仓库级** session：`bridge_session_id = "mcp~<actor>"`（`agent_id=<actor>`，`worktree_id` 恒为 NULL，不绑定首次使用的工作树），首次写调用时幂等打开，永不由 MCP 关闭，跨进程、重启与 linked worktree 复用；每次写入作用于发起进程所在的工作树（checkpoint 目标取该工作树的 HEAD）。写工具的 `operation_id` 由客户端**必填**（`[A-Za-z0-9._:-]{1,128}`，客户端负责在重试时复用）；durable id 为 `<actor>~<operation_id>`（同一表集内区分 actor），checkpoint id 为 `<actor>~ck~<operation_id>`。workspace lease owner 为 `mcp:<bridge_session_id>`（workspace registry 为共享表，owner 字符串与 Harness 的 `deepseek-harness:` 前缀不相交）。`clientInfo` 不进入 tracing span、不落库、不参与授权；`agent.mcp.request` span 只记录服务端可控或已净化的字段（见 MC-01）。
- **Alternatives considered:** 放宽 Harness 表 CHECK 共用表并约定 `mcp~` 前缀（拒绝：Harness 可合法使用任何 id，碰撞时 Harness 行为改变）；`(source, id)` 复合主键（拒绝：需重建 Harness 表并改其全部 SQL）；服务端生成缺省 `operation_id`（拒绝：响应丢失后客户端无法重放同一 id）；每进程一个 session（拒绝：重启后重放必然 digest 冲突）。
- **Consequences:** 需要 Repository 角色 migration 新建七张表（MC-03，含 `agent_mcp_lease` 与 `agent_mcp_usage`，意图表另有 `result_json`）并登记表归属注册表；同一 actor 同一时刻只允许一个写进程（ADR-MC-06 的 actor 锁）。
- **Revisit when:** 需要每个远程客户端独立身份时（DEFER-MC-04）。

### ADR-MC-06: 写权限、审批与崩溃收敛

- **Status:** Accepted
- **Context:** GC-07 要求 fail-open 必须有显式用户选择、日志与测试；MCP 无可靠的服务端主动审批通道；bridge 先执行效果、后把 operation 记为 `applied`，`pending` 状态下永久拒绝重放，而 `failed`/`quarantined` 会被 bridge 再次执行。
- **Decision:** 默认只读：未加 `--allow-write` 时写工具不出现在 `tools/list`，调用写工具按未知工具处理（`-32602`）。`--allow-write` 是唯一写授权来源，且只在 Unix 上可用（写路径端到端门（写锁、写入期限退出、hook 拒绝）只在 Unix 上验证；非 Unix 以 `LBR-CLI-002` 拒绝，DEFER-MC-08）：对 bridge 归类为 Dangerous 的方法（本计划只暴露 commit），MCP 在内部请求中注入 `approval = {decision: "approved", approver: "operator:--allow-write:<actor>"}`，该审批进入参数摘要可审计；Normal 方法不注入。写工具 `annotations` 如实标注。`--allow-write` 进程启动时以 MCP 专用的 `acquire_private_lock`（`mcp_server/lock.rs`，MC-06 交付；不复用会跟随符号链接并截断写入 PID 的 `acquire_control_lock`）取得写锁：锁目录 `mcp/` 位于 `try_get_storage_path` 解析出的**共享存储目录**（仓库数据库所在、所有 linked worktree 共用），以 0700 创建，且必须属于当前有效用户、不可被组/其他用户写（否则拒绝启动）；锁文件以 `O_CREAT|O_NOFOLLOW`、0600 打开并以 `fstat` 校验为当前用户的普通文件后加独占 advisory 锁。actor 锁位于 `mcp/actors/<actor>.write.lock`，与仓库锁 `mcp/repo.write.lock` 分处不同目录，因此任何合法 actor 名（含 `repo`）都不会与仓库锁冲突；因此同一 actor 在不同 linked worktree 中也只能有一个写进程；另一存活进程持有时以用法拒绝 `LBR-CLI-002` 结束。不同 actor 之间另有一把仓库写锁（`mcp/repo.write.lock`，同一 `acquire_private_lock`），每次写工具调用在 fence 校验与效果执行期间持有，阻塞等待上限 30 s（超时为工具错误 `LBR-AGENT-036`），因此两个 MCP actor 不会在同一 index/worktree 上交错写入；HTTP 与 stdio 进程使用同一把锁。这把锁**只**串行化 MCP 写入者：Libra 没有让 CLI、Harness 与 MCP 共同遵守的仓库级写锁。对这些非 MCP 写入者，`libra_commit` 依靠 MC-18/MC-24 的事务内 CAS 前提（OID 与 HEAD 符号引用）与 MC-43 的暂存树前提原子拒绝（HEAD 在 fence 与写入之间被移动 → `LBR-AGENT-038`，不产生提交）。restore 在 clean 检查与工作树写入之间无法排除非 MCP 写入者，因此本计划不暴露 restore 工具（DEFER-MC-07）。首次处理某个 `operation_id` 时，MCP 把规范化后的工具请求（含服务端派生的值，如 checkpoint 的 `target_oid`）写入 `agent_mcp_intent`；此后同一 `operation_id` 的任何调用都先比较客户端参数与意图中的客户端参数（不同 → 工具错误 `LBR-AGENT-033`），再以意图中的完整请求重放，因此 HEAD 变化不会改变重放的参数摘要。持锁即证明本 actor 不存在其它写进程，因此启动时对本 actor session 下每个 `pending` operation 运行收敛：有**确定性收敛器**的方法按收敛器判定（`checkpoint.create`：checkpoint 行存在 → 先幂等补写该 checkpoint 的全部关联链接，再记为 `applied` 并记录其 id；行不存在 → `failed`）；其余方法（本计划为 `commit.create`）的效果是否发生无法从 operation 记录判定，置为 `quarantined`（显式不确定态，不得标记 `failed`）。MCP 前端在调用 bridge 之前读取 operation 状态：`quarantined` 一律返回工具错误 `LBR-AGENT-035`（不交给 bridge 再执行），客户端核对仓库后改用新的 `operation_id`；因 commit 要求 `expected_head` 且以 CAS 校验，新请求不会在已生效的效果上重复执行。同一进程内写工具串行。写工具不受 30 s 读 deadline 约束，但有 120 s 写入硬期限（自写调用开始计，含等待写锁的时间，是一个绝对终点）：超时后进程记录日志并以非零退出码终止（释放 actor 锁与仓库写锁，HTTP 服务需由运维方重启）；MCP 提交**不运行任何仓库 hook**：Unix 进程组无法约束主动 `setsid`/守护化的派生进程，而这类进程可在写锁释放后继续写工作树，因此 `commit.create`（source 为 MCP）先以 MC-23 的 `runnable_commit_hooks()` 预检，存在任一会执行的提交 hook 即返回 `LBR-AGENT-035`（`reason = "hooks_present"`）且不写仓库；否则在 MC-23 的 `HookPolicy::Refuse` 作用域内提交，预检之后才出现的 hook 在执行点被拒绝而不启动进程（`post-commit` 此时提交已生效，结果以 `post_commit_skipped = true` 明示，不静默）。该作用域只在 MCP 调用内生效，CLI 与 Harness bridge 的 hook 执行完全不变（MC-23 回归门）。需要 hook 的仓库在 MCP 下只能使用只读工具与 checkpoint，直至 DEFER-MC-09 提供能约束全部派生进程的隔离。因此写入期限到期退出时不存在 hook 派生进程。分支与暂存内容由 MC-25/MC-26 的一致快照（`head`/`head_ref`/`index_tree`/`staged_changes` 同一次读取、内存计算、不写对象）与 MC-24/MC-08 的前提保护：客户端把审核过的 `head_ref` 作为 `expected_head_ref` 传入，fence 入口与 HEAD 更新事务内都比较，status 与 commit 之间被切到同 OID 的其它分支时以 `LBR-AGENT-038` 拒绝；未出生分支以 `expected_head = null` 表示，映射为 MC-28 的显式 `ExpectedParent::Unborn`（fence 入口与事务内都要求分支 ref 不存在），从不退化为「不校验」；MC-27（先于 MC-08 发布，经共享入口 `diff_index_snapshot` 以同一次载入的 index 生成 patch）让 staged diff 携带同源的 `head`/`head_ref`/`index_tree`，客户端以所审核 diff 的快照作提交前提，同一路径换内容后提交被拒；MC-43 的 `expected_tree` 前提保护：`libra_status` 的 `head`/`index_tree`/`staged_changes` 由同一次读取的不可变树对象得出，`run_commit` 由 index 算出树之后、写提交对象之前与 `expected_tree` 比较且提交正使用该树，不同即 `LBR-AGENT-038`，因此并发 `libra add` 不会让未审核内容进入 MCP 提交，遗留 `pending` 由下次启动收敛为 `quarantined`；不在进程内强行中断写入，避免半写状态。
- **Alternatives considered:** 依赖客户端 elicitation/MRTR（拒绝：支持不确定）；参数里带 approval（拒绝：模型自报审批等于无审批）；pending 一律标 failed（拒绝：效果已发生时会误报失败并丢失原结果）；按仓库状态猜测 commit 是否完成（延后：DEFER-MC-06）。
- **Consequences:** `--allow-write` 进程的审批粒度为整个进程；文档要求只对受信客户端开启；commit 崩溃后需要客户端换新 `operation_id`。
- **Revisit when:** 主流客户端普遍支持 MRTR/elicitation，或 DEFER-MC-06 重启时。

### ADR-MC-07: HTTP 只绑 loopback，HTTPS 由运维方终端提供，Windows 不开放

- **Status:** Accepted
- **Context:** 远程客户端是 **Grok Bot 这个工具**（经 HTTPS 远程 MCP 接入），不是 Grok 模型，本计划不捕获或调用 Grok 本身；规范要求校验 `Origin`、本地服务 SHOULD 只绑 localhost、SHOULD 认证；SB-02 要求令牌不得来自 URL query；`libra service` 有 loopback-only + 0600 令牌先例；`validate_token_file_perms` 在非 Unix 无条件成功。
- **Decision:** `--transport http` 只在 Unix 平台可用，其它平台以用法拒绝 `LBR-CLI-002` 结束（平台判定为纯函数并单测两个分支）。`--listen` 必须是字面 loopback IP 加端口（缺省 `127.0.0.1:0`；就绪后 stderr 打印一行 `libra agent mcp: listening on http://<addr>/mcp`），非 loopback 或主机名在解析期与绑定期都拒绝；每个请求再校验 loopback peer。先要求令牌所在目录私有（属于当前用户、不可被组/其他用户写），再在令牌文件的私有父目录中以保留文件名 `.libra-mcp-token-<sha256(文件名)>.lock` 以不跟随符号链接方式打开并校验、加独占锁取得令牌路径所有权（服务生命周期内持有；与 `LIBRA_HOME` 无关；令牌文件名使用保留前缀或令牌路径落在仓库存储目录内即拒绝）并绑定监听套接字，二者都成功之后（任一失败都不触碰 `--token-out`，因此无论端口是否相同，第二个实例都不会替换存活服务的令牌），令牌由服务进程**每次启动时**用操作系统 CSPRNG 生成 256 bit，以 64 个小写十六进制字符写入 `--token-out`：路径不存在时先在同目录以 `O_CREAT|O_EXCL`、0600 写完整临时文件并 `fsync`，再以 `link(临时文件, --token-out)` 原子发布（目标已存在则失败，绝不替换已有文件）、`fsync` 目录、删除临时文件；已存在时只接受内容哈希等于同目录状态文件 `<hash>.state` 中 `current` 或 `pending`（本服务按「pending → 发布 → current」两阶段写入、每次整体原子替换，见 MC-13）的非符号链接、属主为当前有效用户、权限恰为 0600、所在目录不可被组/其他用户写的普通文件，并以同目录 `O_EXCL` 临时文件 + `fsync` + `rename` 原子替换（旧令牌失效），其余情形拒绝启动且不改动该路径。因此服务管理器可用同一命令行在正常退出、写入期限退出或崩溃后反复重启。服务从不读取外部提供的秘密，不存在弱令牌输入面（格式本身无法证明熵，R16/R17 结论）。客户端交接：远程客户端（Grok Bot）只持有 TLS 终端自身的凭据，终端在服务就绪（stderr `listening on` 行）后由服务管理器触发重载，从 `--token-out` 读取令牌并注入上游 `Authorization: Bearer`；本机客户端每次连接前重读该文件；令牌只从 `Authorization: Bearer` 读取，以 SHA-256 摘要常数时间比较，从不写日志。单一端点 `/mcp`：`POST` 一条 JSON-RPC 消息——请求得 200 `application/json`，通知或响应得 202 空体；`GET`/`DELETE` 返回 405；不签发 `Mcp-Session-Id`。`Origin` 存在且不在 `--allowed-origin` 集合 → 403；`Host` 不在 loopback 形式与 `--allowed-host` 集合 → 403。远程 HTTPS 由运维方在同机运行的 TLS 终端转发到 loopback 端口。
- **Alternatives considered:** 内置 rustls 监听公网（拒绝：扩大攻击面，违反 `libra service` 的「不做 hosted server」红线）；OAuth 2.1（拒绝：规范对 HTTP 授权为可选）；Windows 上不校验权限也开放（拒绝：GC-07）。
- **Consequences:** 远程部署需要运维方配置 TLS 终端；令牌泄露等同进程授权，文档要求轮换。
- **Revisit when:** Windows DACL 校验落地（DEFER-MC-05）或需要多租户时。

### ADR-MC-08: 工具命名、结果形状与错误分层

- **Status:** Accepted
- **Context:** 部分模型平台把 MCP 工具映射为函数名，不接受 `.`；规范 `2025-11-25/server/tools.mdx` 区分协议错误与工具执行错误，并要求输入校验错误作为工具执行错误返回以便模型自我纠正。
- **Decision:** 工具名 `libra_<noun>[_<verb>]`，只用 `[a-z_]`；`tools/list` 按名字字典序返回；每个工具有 `inputSchema`（JSON Schema 2020-12，`additionalProperties: false`）与 `outputSchema`。成功结果同时给 `structuredContent`（对象）与一个 `type: "text"` 内容（同一对象的紧凑 JSON 文本）。**协议错误**（JSON-RPC error）：JSON 解析失败 `-32700`；非法请求/batch `-32600`；未知方法 `-32601`；`params` 不是对象、`name` 缺失或不是字符串、`arguments` 不是对象、未知工具名 → `-32602`。**工具错误**（`isError: true`）：参数不符合 `inputSchema`（类型、枚举、缺必填、多余字段）、路径逃逸、bridge `BridgeError`、仓库错误；`structuredContent = {stable_code, bridge_code, message, retryable}`，`stable_code` 为既有 `LBR-*`（输入校验为 `LBR-AGENT-027`）。
- **Alternatives considered:** 点号命名（拒绝：函数名兼容性）；schema 不符走 `-32602`（拒绝：与规范 tools 错误处理冲突）。
- **Consequences:** 不新增错误码；复用码在 MCP 下的含义写入 `docs/error-codes.md`：MC-01 在 `LBR-AGENT-022..038` 段落前加 MCP 复用注记，MC-19 更新 `028`/`036` 行、MC-21 更新 `029` 行的描述（各卡同卡文档门以 `rg` 校验）。
- **Revisit when:** 需要多仓库命名空间时。

### ADR-MC-09: 工具范围与参数映射

- **Status:** Accepted
- **Context:** 用户列举 checkpoint、commit、workspace、trace 等能力；bridge 部分方法接受客户端路径或会创建 TaskCopy workspace。
- **Decision:** 只读：`libra_status`（MC-02 返回 `status` 概要；MC-25 增加不写对象的快照身份 `head`/`head_ref`/`index_tree`，MC-26 增加同源的 `staged_changes`；未出生 HEAD 时 `head = null`）、`libra_diff`（MC-05 开放 `worktree`/`staged`，MC-07 开放 `checkpoint`，MC-27 使 staged 结果携带同源 `head`/`head_ref`/`index_tree`）、`libra_checkpoint_list`/`libra_checkpoint_show`（MC-07）、`libra_workspace_list`/`libra_workspace_show`（MC-10）、`libra_trace_session_list`/`libra_trace_session_show`（MC-12）、`libra_trace_checkpoint_list`/`libra_trace_checkpoint_show`（MC-20）。写（需 `--allow-write`，`operation_id` 必填）：`libra_checkpoint_create`（MC-06；目标提交由服务端读取当前 HEAD，unborn HEAD 拒绝）、`libra_commit`（MC-08；`message` + `expected_head`（`null` = 未出生分支）+ `expected_head_ref` + `expected_tree` + `operation_id` + 可选 `allow_empty`；存在提交 hook 时拒绝）、`libra_workspace_claim`（MC-11）/`libra_workspace_renew`（MC-38）/`libra_workspace_release`（MC-41）（只作用于当前仓库工作树，路径与 worktree 由服务端派生并在 renew/release 时复核，永不创建 TaskCopy；三者都要求客户端 `operation_id`，lease 效果与 MCP operation 记录在同一 SQLite 事务内提交，同一 `operation_id` 重放返回首次结果；提交前崩溃不留下任何行，重试按新请求执行）。不暴露 `review.run`、ingress、transcript 原文、push、rewind（DEFER-MC-01）与 restore（DEFER-MC-07）。
- **Alternatives considered:** 暴露 bridge 全部 20 方法（拒绝：ingress 是 Harness 专用捕获语义，review.run 拉起外部进程）。
- **Consequences:** 每组工具一张卡。
- **Revisit when:** 用户提出新的工具需求。

## 全局工程约束

GC-01 至 GC-13 按模板原文对本计划全部任务生效（见 [`plan-template.md`](plan-template.md)「全局工程约束」）。本计划追加：

- **GC-MC-01 bridge 原样（口径见 ADR-MC-02 的使用者裁决：对外契约不变、允许内部重构）:** 任何卡不得改变 `libra agent bridge --stdio` 的对外行为；`src/internal/ai/agent_bridge/protocol.rs` 中 `METHOD_ALLOWLIST`、`PROTOCOL_MAJOR/MINOR`、上限常量与错误目录不得修改；`agent_bridge_*` 表结构不得修改；每张卡 C 组全量（各实现卡均命中 T-1）必须包含全部 `agent_bridge_*` target 绿色。
- **GC-MC-02 单一事实源:** MCP 协议版本集合、JSON-RPC 错误码、上限、结果信封与工具注册表只在 `src/internal/ai/mcp_server/` 定义；表名集合只在 `BridgeSource::tables()` 定义；CLI 与 MCP 共享的查询（status、trace、workspace）只有一份实现，MCP 不得复制 SQL。
- **GC-MC-03 MCP 结果信封:** 所有工具结果在返回前经过 `mcp_server` 的唯一结果信封：① `structuredContent` 序列化后经 `review::redact_untrusted` 脱敏、重解析并清洗控制字符，失败则返回工具错误 `LBR-AGENT-034`，不得回退原文；② 以完整序列化的 JSON-RPC 响应（含 `structuredContent` 与 `content[0].text` 两份内容）计量，超过 256 KiB：可截断的文本字段（diff patch）截断并置 `truncated: true`，截断后仍超限或不可截断的结果返回工具错误 `LBR-AGENT-028`；③ 读工具 30 s deadline，超时返回工具错误 `LBR-AGENT-036`（`retryable: true`）；读工具在 MC-29 的读子进程内运行（并发 ≤ 4；超时即回复并强制终止子进程，回收后才归还槽位；MC-35 为每个子进程设内存上限 512 MiB）；MCP diff 经 MC-31 的有界入口：分配前尺寸预检（单文件 ≤ 1 MiB）、行数 ≤ 10 000、关闭 rename 检测，并受 MC-37 的全局预算（patch ≤ 256 KiB、文件条目 ≤ 1000）约束；MCP status 经 MC-33 的扫描路径预算与全部数组上限。stdio 模式 stdout 只写合法 JSON-RPC 消息（每行一条），诊断只写 stderr。「与 CLI 一致」一律指「等于 CLI `--json` 的 `data` 经同一信封处理后的值」。
- **GC-MC-04 C/D 组统一口径:** 每张实现卡的 C 组按 ER-04 第 ①–⑨ 步执行（feature branch → PR → 同一 PR head 上 `base.yml` + CodeQL 全绿 → squash merge → `gh release create v<version> --target <merge sha>`）。D 组四项：**D-MC-1** `base.yml` 的 `format`、`clippy`、`owner-liveness-macos`、`scoped-input-windows`、`redundancy`、`test`、`network-remotes`、`opencode-export-sandbox-linux` 在两个 ref 上都全部 success：事件 `pull_request`（GitHub 在合成的 `refs/pull/<number>/merge` 上运行；取证同时记录该 run 的合成 merge SHA 与事件字段 `pull_request.head.sha`，并核对后者等于 PR 源分支 head SHA、run 关联的 PR 号等于本卡 PR），以及 squash merge 后事件 `push`（ref = 用于发布的 merge SHA）；merge SHA 上的 run 若被 `concurrency` 取消，以 `gh run rerun <run-id>` 重跑至 success，未 success 前该卡不得记为 `complete`；**D-MC-2** `codeql.yml` 的 `analyze`（全部 matrix 语言）在 PR head 与 merge 后 `main` push（ref = merge SHA）均 success；**D-MC-3** `release.yml`（事件 `push` tag `v<version>`，由 `gh release create` 触发，tag 指向 merge SHA）的 `build-and-upload`（全部 matrix）、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 success；**D-MC-4** `curl -fsSI https://download.libra.tools/libra/releases/v<version>/libra-darwin-arm64` 返回 200，且 `D="$(mktemp -d)" && curl -fsS -o "$D/install.sh" https://download.libra.tools/install.sh && grep -q 'DEFAULT_VERSION="v<version>"' "$D/install.sh"` 退出 0（先以检查过退出码的完整下载落盘，再检查内容）。每卡在 `Current evidence` 追加 run ID、SHA 与结论。
- **GC-MC-05 网站文档同卡交付（按模板 ER-06a 的 Libra 相邻仓库路径）:** 凡卡内 `Docs and compatibility impact` 列出 `../libra-backend/apps/tanstack-app/content/docs/commands/` 下的任一网站页（如 `agent.en.md`、`repository-hooks.en.md`），同卡在相邻仓库 `../libra-backend` 的 `cf` 分支上交付（不建分支、不建克隆；ER-06a 的隔离克隆例外只适用于 Git 相邻仓库，本机相邻仓库是 Libra 仓库），记 `P=<本卡所列网站页相对 `../libra-backend` 根的路径…>`（如 `apps/tanstack-app/content/docs/commands/agent.en.md`；以下命令都在 `cd ../libra-backend` 之后执行，与 `libra diff --name-only` 输出的仓库相对路径同形）。**整个交付在 `set -euo pipefail` 的 shell 中执行；每个命令替换先单独赋值（`X="$(cmd)"`，命令失败即中止，不会把失败当作空输出），再用 `test -n`/比较判定**：① 前置检查：`test -d ../libra-backend/.libra && test ! -e ../libra-backend/.git`（两种元数据同时存在或都不存在即停止）；`cd ../libra-backend && libra status --short --branch` 显示当前分支为 `cf`（不是则 `libra switch cf` 后复查，切换被阻止即停止）；`libra fetch origin`；`H="$(libra rev-parse HEAD)"; O="$(libra rev-parse origin/cf)"; test -n "$H" && test "$H" = "$O"`（本地有任何领先提交即停止——Libra 相邻仓库没有克隆例外）；`ST="$(libra status --short -- $P)"; test -z "$ST"`（`libra status` 失败即中止；任一目标路径不干净即停止，不编辑）；② 编辑所列网站页，`python3 scripts/sync-libra-command-docs.py --libra-root ../libra --write --only <本卡所列命令页名，如 agent>` 只同步本卡页面，随后 `--check --only <同上>` 退出 0；再运行不带 `--only` 的 `--check`：若报告本卡所列页面之外的漂移，停止并报告该漂移的归属（不在本卡同步，不提交其它页面）；③ `pnpm typecheck` 与 `pnpm build` 退出 0；④ Claude Code 对网站改动给出 PASS，随即在当前 shell 中固定审核快照摘要 `RD="$(libra diff -- $P)" && test -n "$RD" && REVIEWED="$(printf '%s' "$RD" | shasum -a 256 | cut -d' ' -f1)" && test -n "$REVIEWED"`（审核的改动非空）（不落盘任何补丁文件；计划证据只记录该摘要与比较结果，ER-11/v2.12）；⑤ `libra add $P` → `SN="$(libra diff --cached --name-only)" && A="$(printf '%s\n' "$SN" | sort)" && E="$(printf '%s\n' $P | sort)" && test -n "$A" && test "$A" = "$E"`（暂存路径集合恰为本卡所列）→ `SD="$(libra diff --cached -- $P)" && SH="$(printf '%s' "$SD" | shasum -a 256 | cut -d' ' -f1)" && test -n "$SH" && test "$SH" = "$REVIEWED"`（暂存内容与审核快照逐字节相同；不同则 `libra restore --staged $P` 并停止）→ `libra commit -s -m "docs(agent): sync website command docs for <卡号>"` 并按 ER-07 校验签名与 DCO → `COMMIT_SHA="$(libra rev-parse HEAD)"; test -n "$COMMIT_SHA"`，`libra fetch origin` 后 `PARENT="$(libra rev-parse HEAD~1)"; BASE="$(libra rev-parse origin/cf)"; test -n "$PARENT" && test -n "$BASE" && test "$PARENT" = "$BASE"`（每个 `rev-parse` 先赋值，失败即中止，空值不得相等）→ 以轻量 tag（非分支）固定该提交：`libra tag mcp-docs-<卡号>-<随机后缀> "$COMMIT_SHA"` → `libra push origin refs/tags/mcp-docs-<卡号>-<随机后缀>:refs/heads/cf`（必须写完整 ref：简写 `<tag>:cf` 会在远端建 tag 而非更新分支；`libra push` 不接受 `HEAD` 或裸 SHA 作源）→ 断言 `RS="$(libra ls-remote origin refs/heads/cf | cut -f1)"; test -n "$RS" && test "$RS" = "$COMMIT_SHA"`，且 `libra ls-remote origin refs/tags/mcp-docs-<卡号>-<随机后缀>` 为空 → 删除本地 tag，记录推送前后远端 SHA；⑥ Cloudflare 部署后，对每个所列页面的线上 URL `https://libra.tools/en/docs/commands/<页面名>` 执行该卡「同卡文档门」中的线上核对命令并退出 0。任一步失败本卡停在 `locally-accepted`。**⓪ 一次性演练（全局强制，不计入 G-03；首张执行 GC-MC-05 的卡在交付前执行并记录结果）：** 在 `mktemp -d` 的临时 Libra 仓库中分别构造「目标路径有未提交改动」「暂存区含非目标路径」「暂存内容与审核快照不同（审核后又改动了目标文件）」三种情形，断言①的干净检查、⑤的路径集合检查与⑤的摘要比较各自以非 0 结束，并以一个形如 `apps/tanstack-app/content/docs/commands/agent.en.md` 的实际网站页路径断言⑤的路径集合检查在正确暂存时以 0 结束；另做故障注入：以 `libra` 替身（`PATH` 前置一个先打印「期望值」再退出 1 的 `libra` 脚本，模拟「打印后失败」）运行①的状态检查与 `rev-parse` 比较、④的摘要固定、⑤的暂存路径集合比较、暂存内容摘要比较、提交后的父提交比较与推送后的远端比较，断言每一步都以非 0 中止而不是当作「干净」或「相等」；并在含本地远端的临时仓库中验证⑤的完整 ref 推送只更新远端 `refs/heads/cf`。
- **GC-MC-06 客户端配置示例:** 文档中的客户端配置示例只写通用 MCP 配置形状（`command`/`args`/`cwd`，HTTP `url`/`headers`）；具体产品是否可用以 MC-15 证据为准。
- **GC-MC-08 门命令所用的 `libra` 二进制:** 本计划所有门、预检与脚本中的 `libra` 都必须解析为与当前仓库 schema 兼容的二进制。执行任何卡的 ER-01 预检与 A/B 门之前先运行 `cargo build && export PATH="$PWD/target/debug:$PATH"`，并在该卡 Current evidence 记录 `command -v libra` 与 `libra --version`；已安装的 `libra` 因库 schema 较新而拒绝时（`UnsupportedFuture`，如 2026-10-19 本机已安装版本只支持 `2026101901` 而仓库已到 `2026101902`）不得用于任何门。C 组的安装步骤（ER-04 ⑥）照常执行，不改变发布顺序。相邻网站仓库中的 `libra` 命令同样在该 `PATH` 下执行。
- **GC-MC-09 全量门与 `CLAUDE.md` 的口径对齐:** 本计划是 `CLAUDE.md`「Layered test execution for plan-driven work (ER-13)」（`CLAUDE.md:192-194`）所指的计划驱动工作，该节规定测试门按模板 ER-13 分层，因此本计划的全量与收口测试证据以模板 v2.14 为准：只能是 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`（ER-13/ER-14）。`source .env.test && cargo test --all` 只在诊断 nextest 自身问题时运行，其结果不构成通过证据、也不是完成门；fmt 与 clippy 两门照常必跑。
- **GC-MC-10 工具注册契约与结果一致性:** 每个工具注册时必须给出 `outputSchema = {"type": "object", "anyOf": [<成功结构>, TOOL_ERROR_SCHEMA]}`（`TOOL_ERROR_SCHEMA` 在 `mcp_server/protocol.rs` 单点定义）与一个一致性夹具（在独立的临时仓库中建立该工具的真实前置条件——`--allow-write` 会话、暂存改动与 `expected_head`、先行 claim 得到的 fence 等——返回一组成功参数与一组工具错误参数并负责清理），缺项无法编译注册；MC-01 的注册表级测试 `agent_mcp_conformance_test` 对注册表中**所有**工具逐一运行夹具、调用两组参数，并按分支以 `jsonschema` 校验 `structuredContent`，因此后续每张新增工具的卡在其 C 组全量（各卡均命中 T-1）中自动对新工具执行该校验，新工具不满足即全量失败。
- **GC-MC-07 判定术语与门的执行规则:** AC 中的固定术语各是一个谓词：「协议错误 `N`」= 响应为 JSON-RPC error 且 `error.code == N`；「工具错误 `X`」= 响应 `result.isError == true` 且 `result.structuredContent.stable_code == X`（二者构成同一个「错误形状等于期望」的比较）；「以用法拒绝 `LBR-CLI-002` 结束」= 进程退出码等于 129（`docs/error-codes.md:94` 中 `LBR-CLI-002` 的退出码）。卡内「Verification」「强制门」「同卡文档门」中的每个 `cargo test … -- --exact` 门按 `bash -o pipefail -c '<命令> 2>&1 | tee /dev/stderr | grep -Eq "test result: ok\. 1 passed; 0 failed"'` 执行：命令失败或实际执行的用例数不是 1 均判为失败；记录证据时写入该行输出。

## 执行检查必备需求（强制）

ER-01 至 ER-14 按模板原文对本计划全部任务生效（见 [`plan-template.md`](plan-template.md)「执行检查必备需求」）。本计划不登记 ER 例外。终止链末端提交按模板 v2.14 预登记：本计划最终纯文档记账提交的 D 组结果，由下一次 `plan-status.md` 例行同步补记 SHA、run ID、结论与观察 UTC 时间。

## 实施顺序

- `MC-01 -> MC-22`
- `MC-22 -> MC-16`
- `MC-16 -> MC-19`
- `MC-19 -> MC-29`
- `MC-29 -> MC-35`
- `MC-35 -> MC-32`
- `MC-32 -> MC-36`
- `MC-36 -> MC-39`
- `MC-39 -> MC-31`
- `MC-31 -> MC-37`
- `MC-37 -> MC-33`
- `MC-33 -> MC-21`
- `MC-21 -> MC-02`
- `MC-02 -> MC-03`
- `MC-03 -> MC-04`
- `MC-04 -> MC-05`
- `MC-05 -> MC-06`
- `MC-06 -> MC-07`
- `MC-07 -> MC-18`
- `MC-18 -> MC-23`
- `MC-23 -> MC-24`
- `MC-24 -> MC-43`
- `MC-43 -> MC-28`
- `MC-28 -> MC-25`
- `MC-25 -> MC-26`
- `MC-26 -> MC-27`
- `MC-27 -> MC-34`
- `MC-34 -> MC-30`
- `MC-30 -> MC-08`
- `MC-08 -> MC-10`
- `MC-10 -> MC-40`
- `MC-40 -> MC-11`
- `MC-11 -> MC-38`
- `MC-38 -> MC-41`
- `MC-41 -> MC-12`
- `MC-12 -> MC-20`
- `MC-20 -> MC-13`
- `MC-13 -> MC-17`
- `MC-17 -> MC-14`
- `MC-14 -> MC-15`

任务编号不等于执行顺序（模板「任务卡」节）：MC-16、MC-17、MC-19、MC-20、MC-22 分别自 MC-01、MC-13、MC-16、MC-12、MC-01 拆出，MC-23 为 R14 新增（R18 改为 hook 拒绝策略）、MC-24 为 R16 新增、MC-25 为 R20 自 MC-02 拆出（快照身份，不写对象）、MC-26 为 R21 自 MC-25 拆出（暂存差异）、MC-27 为 R22 新增（staged diff 绑定快照身份，R23 起排在 MC-08 之前）、MC-28 为 R23 自 MC-24 拆出（未出生前提）、MC-29 为 R24 自 MC-19 拆出（读执行器）、MC-31 为 R25 自 MC-29 拆出（diff 取消检查点与单文件上限），二者排在任何生产读工具之前，MC-30 为 R25 自 MC-27 拆出（审核完整性标志），MC-32、MC-33、MC-34 为 R28 新增（对象尺寸、status 有界收集、信封处理结果与重新计量），MC-35、MC-36 为 R29 新增（读子进程内存上限、工作树有界读取），MC-37 为 R30 自 MC-31 拆出（diff 全局预算），MC-38、MC-39 为 R35 分别自 MC-11、MC-36 拆出（lease 续期/释放、LFS 指针有界生成），MC-40 为 R36 新增（lease 工作树身份，含主工作树），MC-41 为 R38 自 MC-38 拆出（release），MC-43 为 R59 自 MC-24 拆出（暂存树前提），MC-42 为 R44 自 MC-06 拆出、R48 重新并入 MC-06（编号保留不复用），MC-18 为 R5 新增、MC-21 为 R11 新增（R12 起为传输无关的核心限流，排在 HTTP 之前），均按上列依赖边插入；MC-09（restore 工具）已于 R6 撤销并由 DEFER-MC-07 承接，编号保留不复用。

每张实现卡都修改 `Cargo.toml`（`[[test]]`）与 `tests/INDEX.md`，多数还共享 `src/internal/ai/mcp_server/tools/mod.rs` 与 agent 文档（I–I 相交），因此全串行（G-10）。

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-MC-01 | incoming | 跨计划 | `plan-20261018-grok-capture` 对 `src/internal/db/migration.rs` 的在途写入（GK-03..05） | grok-capture 计划执行者（与 Grok Bot 客户端无关，仅写集依赖） | MC-03 开工时 `libra status --short --branch` 显示 `migration.rs` 无他人未提交改动，且 grok 计划没有「已 bump 未推送」的卡 | `plan-status.md` grok 行；`migration.rs:2421-2438`（2026-10-19） | 等待其推送后基于新 `main` 开工；超过 3 个工作日登记阻塞并请用户裁决 |
| DEP-MC-02 | incoming | 外部服务 | 网站仓库 `/Volumes/Data/GitMono/libra-backend` 的 `cf` 分支与 Cloudflare 部署 | 网站维护者 | GC-MC-05 ①–⑥ 全部可执行 | `../libra-backend/.libra`；`../libra-backend/AGENTS.md:24-29`（2026-10-19） | 分支不是 `cf` 则 `libra switch cf`；被脏区阻止或部署失败则停止并报告，本卡停在 `locally-accepted` |
| DEP-MC-03 | incoming | 跨计划 | 其它在途计划对本计划共享文件（`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`src/cli.rs`、`src/command/control_lock.rs`、`sql/migrations/`）的未提交改动；成稿时 [`plan-20261019-worktree-layout.md`](plan-20261019-worktree-layout.md) 正在修改其中多数 | 对应计划执行者 | 每卡开工时 `libra status --short --branch` 显示本卡 `Implementation write set` 内无他人未提交改动 | `libra status --short --branch`（2026-10-19 08:20 UTC 观察） | 等待其提交推送后基于新 `main` 开工；不得覆盖、暂存或提交他人改动（GC-12）；超过 3 个工作日登记阻塞并请用户裁决 |
| DEP-MC-04 | incoming | 外部服务 | WorkBuddy 客户端环境（可配置 stdio MCP 服务器的安装） | 用户（计划请求者） | 可在 WorkBuddy 中添加 `command`/`args`/`cwd` 形式的 MCP 服务器并调用工具 | 用户需求（2026-10-19） | MC-15 停在 `blocked`，不得以其它客户端替代；用户可在修订历史登记放弃该项 |
| DEP-MC-05 | incoming | 外部服务 | Grok Bot 远程 MCP 环境 + 运维方 HTTPS 终端（公网可达域名转发到 loopback） | 用户（计划请求者） | Grok Bot 可配置远程 MCP URL 与 `Authorization` 头，且 HTTPS 终端可用 | 用户需求（2026-10-19） | 同上 |

### 发布分组与并发窗口

本计划无 `REL-*`：每张实现卡独立发布（G-07）；MC-15 为 `no-release` audit 卡。

**并发声明:** 全串行（G-10）。

**发布者:** 执行本计划的单一 Claude Code 会话（开工时在修订历史登记会话 ID）负责全部 C 组与 D 组跟踪（ER-12）。

**发布窗口顺序:** MC-01 → MC-22 → MC-16 → MC-19 → MC-29 → MC-35 → MC-32 → MC-36 → MC-39 → MC-31 → MC-37 → MC-33 → MC-21 → MC-02 → MC-03 → MC-04 → MC-05 → MC-06 → MC-07 → MC-18 → MC-23 → MC-24 → MC-43 → MC-28 → MC-25 → MC-26 → MC-27 → MC-34 → MC-30 → MC-08 → MC-10 → MC-40 → MC-11 → MC-38 → MC-41 → MC-12 → MC-20 → MC-13 → MC-17 → MC-14（同一时刻只允许一张卡处于「已 bump 未推送 / 未触发 release」）；MC-15 为 no-release，其后为 CLOSE-MC。

**计划收口点 CLOSE-MC（MC-15 的 C/D 覆盖来源）:** 在 MC-15 写入任何证据**之前**、于干净工作树上同步本地 `main`（MC-15 的证据只在此后的本地 `main` 上写入）：① `libra status --short` 输出为空后 `libra switch main`；② `libra pull --ff-only origin main`，断言 `libra rev-parse HEAD` 等于 `libra ls-remote origin refs/heads/main` 给出的 SHA，且 `libra merge-base --is-ancestor <MC-14 的 merge SHA> HEAD` 退出码为 0。然后执行 MC-15（在本地 `main` 上写入其证据与 `plan-status.md` 同步），再在该最终树上执行全量收口门（`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`；`cargo test --all` 仅按 GC-MC-09 用于诊断），随后以 `libra add <本计划文件> docs/development/plan/plan-status.md docs/development/plan/plan-long.md` 精确暂存、`libra commit -s -m "docs(plan): close out plan-20261019-agent-mcp-server"`（ER-07 签名校验）、`libra push origin main` 提交纯文档记账（不 bump、不发布），推送后断言 `libra ls-remote origin refs/heads/main` 给出的 SHA 等于 `libra rev-parse HEAD`（即记账提交）；推送被拒（远端已前移）时回到步骤②重新快进并重放记账改动，不强推。D 组：该推送在 `main` 上触发的 `base.yml`（D-MC-1 所列全部 job）与 `codeql.yml` `analyze`（ref = 记账提交 SHA）全部 success（被取消则 `gh run rerun` 至 success）；按模板 v2.14 终止链末端例外（本节即预登记），其 SHA、run ID、结论与观察 UTC 时间由下一次 `plan-status.md` 例行同步补记。

### Phase 0: 协议核心、只读工具与存储分表

**目标:** 标准客户端能经 stdio 握手并读取状态与 diff；bridge 存储按 source 分表。

**进入条件:** 计划级 Codex review PASS 并已推送。

**退出条件:** MC-01、MC-22、MC-16、MC-19、MC-29、MC-35、MC-32、MC-36、MC-39、MC-31、MC-37、MC-33、MC-21、MC-02、MC-03、MC-04、MC-05 `done` / `complete`。

### Phase 1: 写入、协调与 trace

**目标:** MCP 写会话、checkpoint、commit（原子前提）、崩溃不确定态、workspace 读与 lease、trace 读。

**进入条件:** MC-05 `done`。

**退出条件:** MC-06、MC-07、MC-18、MC-23、MC-24、MC-43、MC-28、MC-25、MC-26、MC-27、MC-34、MC-30、MC-08、MC-10、MC-40、MC-11、MC-38、MC-41、MC-12、MC-20 `done` / `complete`。

### Phase 2: 远程传输、新协议纪元与联调

**目标:** loopback HTTP；`2026-07-28` modern 纪元；真实客户端联调。

**进入条件:** MC-20 `done`。

**退出条件:** MC-13、MC-17、MC-14、MC-15 `done` / `complete`；CLOSE-MC 完成。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`（每卡完整 C 组含 `patch + 1` 与 `gh` 发布）
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（发布后只能发新版本修复；降级指引：用户经 `install.sh` 指定上一版本；兼容窗口：MCP 为新增表面，降级只会失去 `agent mcp` 的对应能力。MC-03 起的二进制会推进仓库 schema，按 MC-03 的降级屏障说明处理）
- **Migration and rollback 默认:** `N/A：无 schema 迁移`（MC-03 除外）
- **Security and privacy 默认:** 继承 GC-07、GC-11、GC-MC-03
- **Performance budget 默认:** 继承 GC-10 与「性能与容量摘要」

**默认覆盖:**

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| MC-03 | Task type | `migration`：Repository 角色新建 `agent_mcp_*` 表集 |
| MC-03 | Rollback mode | `forward-only`：`agent_mcp_*` 行不可无损撤回；down 在存在行时拒绝 |
| MC-03 | Migration and rollback | 见卡内 |
| MC-15 | Task type / Release boundary / Rollback mode | `audit` / `no-release` / `revert`：只写本计划证据段 |

**规则 waiver（`EX-*`）:**

| 例外 ID | 任务 | 豁免项 | 理由与补偿措施 | Approver | Review round | 证据 | 有效期 |
|---|---|---|---|---|---|---|---|
| EX-MC-01 | MC-01 | G-03 条目上限（门族型验收） | 握手、错误分层、stdio 帧与工具注册一致性是同一恢复轴（「stdio 上的 MCP 协议核心」）上的一致性门族；请求 span 已拆到 MC-22，结果信封已拆到 MC-16/MC-19；按门拆卡会把同一协议实现与其测试拆散（G-01/G-02）。补偿：每个门是卡内「判据规范」中的具名用例，按 GC-MC-07 执行，失败即整卡不达标；分子如实记 | 使用者（本计划请求者） | R1 后会话内选择「Approve gate-family waivers」；R2 后按新编号改挂（范围不变） | 本行；MC-01「判据规范」 | 本计划内 |
| EX-MC-02 | MC-04 | G-03 条目上限（门族型验收） | `implementation` 卡：bridge 读写按 source 隔离的门族（表名集合守卫、Harness 读面排除、同名标识双向、session 互斥、owner、既有测试只加一行的结构守卫）属同一恢复轴；按门拆卡会把同一存储层改动与其测试拆散（G-01/G-02）。补偿：每个门是卡内具名用例或可复制命令，按 GC-MC-07 执行；分子如实记 | 使用者（本计划请求者） | 同上 | 本行；MC-04「判据规范」 | 本计划内 |
| EX-MC-03 | MC-06 | G-03 条目上限（门族型验收） | 写会话（锁、稳定 session、标识、收敛框架、quarantine 拦截）与首个写工具 checkpoint 的门族。补偿同上；R48 起门族另含写记录容量（计数与上限）门——计数与强制必须与第一个写工具同版本生效（R44/R47/R48 结论），属同一「MCP 可恢复写入」恢复轴 | 使用者（本计划请求者） | 同上 | 本行；MC-06「判据规范」 | 本计划内 |
| EX-MC-04 | MC-13 | G-03 条目上限（门族型验收） | HTTP 传输的访问控制与状态码门族（loopback、令牌、Origin、Host、方法、版本头、体积、临时串行模式、停机）属同一传输轴；并发 16/64 准入已拆到 MC-17。补偿同上 | 使用者（本计划请求者） | 同上 | 本行；MC-13「判据规范」 | 本计划内 |
| EX-MC-05 | MC-14 | G-03 条目上限（门族型验收） | modern 纪元的规范一致性门族（discover、`_meta`、错误码、结果字段、HTTP 头）。补偿同上 | 使用者（本计划请求者） | 同上 | 本行；MC-14「判据规范」 | 本计划内 |
| EX-MC-06 | MC-12 | G-03 条目上限（门族型验收） | 只覆盖 session 查询族：CLI 字节稳定、MCP 与 CLI 等价、分页、缺失对象、transcript 不外泄、空库与只读标注门是同一「session 查询共享」轴的门族，拆门会把同一查询抽取与其消费者拆散。补偿同上 | 使用者（本计划请求者） | 同上 | 本行；MC-12「判据规范」 | 本计划内 |
| EX-MC-07 | MC-20 | G-03 条目上限（门族型验收） | 只覆盖 checkpoint 查询族，门族构成与理由同 EX-MC-06（R6 自 MC-12 拆出，豁免范围仍在用户批准的 trace 查询门族之内）。补偿同上 | 使用者（本计划请求者） | 同上（R7 由 EX-MC-06 分出独立行） | 本行；MC-20「判据规范」 | 本计划内 |
| EX-MC-08 | MC-08 | G-03 条目上限（门族型验收） | `libra_commit` 写路径的门族（正常提交、重放、digest 冲突、事务内 CAS 端到端竞态、运维审批摘要、效果后崩溃、存在提交 hook 时拒绝且不执行/不改 HEAD）属同一「MCP 提交」恢复轴，拆出故障路径会让工具先于其故障门发布（R3 结论）。补偿：每个门为卡内具名用例，按 GC-MC-07 执行 | 使用者（本计划请求者） | R12 后会话内选择「Approve EX-MC-08」 | 本行；MC-08「判据规范」 | 本计划内 |
| EX-MC-09 | MC-11 | G-03 条目上限（门族型验收） | `libra_workspace_claim` 的门族（当前工作树路径与 owner、他人持有、重放同一 lease、提交前崩溃后 lease/意图/operation/`agent_mcp_lease` 全部回滚、同一 `operation_id` 重试成功、既有 lease 不受影响）属同一「事务绑定的 lease 写入」恢复轴；拆出故障路径会让工具先于其故障门发布。补偿：每个门为卡内具名用例，按 GC-MC-07 执行 | 使用者（本计划请求者） | R39 后会话内选择「批准 EX-MC-09」（批准范围为 claim/renew/release 的 lease 门族） | 本行；MC-11「判据规范」 | 本计划内 |
| EX-MC-10 | MC-38 | G-03 条目上限（门族型验收） | `libra_workspace_renew` 的同一 lease 门族（工作树与 actor 边界、重放、主工作树、TTL 缺省与不缩短、提交前崩溃回滚与重试）；与 EX-MC-09 同轴，按每卡一行登记 | 使用者（本计划请求者） | 同 EX-MC-09（同一次批准覆盖 lease 门族，MC-38 登记为独立行） | 本行；MC-38「判据规范」 | 本计划内 |
| EX-MC-11 | MC-41 | G-03 条目上限（门族型验收） | `libra_workspace_release` 的同一 lease 门族（工作树与 actor 边界、重放、配额满时结算、提交前崩溃回滚与重试、重复被拒不写行）；与 EX-MC-09 同轴，按每卡一行登记 | 使用者（本计划请求者） | 同 EX-MC-09（该次批准的范围明确包含 MC-11/MC-38/MC-41） | 本行；MC-41「判据规范」 | 本计划内 |

### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| MC-01 | implementation | stdio 上的 legacy MCP 协议核心（握手、错误分层、帧） | immutable-release | yes | yes | 29/8@EX-MC-01 | 29/8@EX-MC-01 | 4/7 | M | DEP-MC-03,DEP-MC-02 | no-overlap | independent | N/A | EX-MC-01 |
| MC-02 | implementation | `libra_status` 只读工具 | immutable-release | yes | yes | 8/8 | 8/8 | 1/2 | M | MC-21,DEP-MC-03,DEP-MC-02 | 序列化于 MC-21 | independent | N/A | N/A |
| MC-03 | migration | Repository 角色新建 MCP 专用表集（不触碰 `agent_bridge_*`） | forward-only | yes | yes | 8/8 | 8/8 | 3/5 | M | MC-02,DEP-MC-01,DEP-MC-03,DEP-MC-02 | 序列化于 MC-02 | independent | N/A | N/A |
| MC-04 | implementation | bridge 读写按 source 选择独立表集（Harness SQL 不变） | immutable-release | yes | yes | 17/8@EX-MC-02 | 17/8@EX-MC-02 | 2/8 | M | MC-03,DEP-MC-03 | 序列化于 MC-03 | independent | N/A | EX-MC-02 |
| MC-05 | implementation | `libra_diff` 只读工具（worktree/staged 模式） | immutable-release | yes | yes | 8/8 | 8/8 | 1/2 | M | MC-04,DEP-MC-03,DEP-MC-02 | 序列化于 MC-04 | independent | N/A | N/A |
| MC-06 | implementation | MCP 可恢复写入（写会话 + 首个写工具 `libra_checkpoint_create`） | immutable-release | yes | yes | 54/8@EX-MC-03 | 54/8@EX-MC-03 | 4/6 | M | MC-05,DEP-MC-03,DEP-MC-02 | 序列化于 MC-05 | independent | N/A | EX-MC-03 |
| MC-07 | implementation | MCP 读取本 actor 的 checkpoint | immutable-release | yes | yes | 8/8 | 8/8 | 1/3 | M | MC-06,DEP-MC-03,DEP-MC-02 | 序列化于 MC-06 | independent | N/A | N/A |
| MC-08 | implementation | `libra_commit` 写工具（含崩溃不确定态） | immutable-release | yes | yes | 23/8@EX-MC-08 | 23/8@EX-MC-08 | 4/5 | M | MC-30,DEP-MC-03,DEP-MC-02 | 序列化于 MC-30 | independent | N/A | EX-MC-08 |
| MC-10 | implementation | workspace 只读工具与查询共享 | immutable-release | yes | yes | 7/8 | 7/8 | 2/3 | M | MC-08,DEP-MC-03,DEP-MC-02 | 序列化于 MC-08 | independent | N/A | N/A |
| MC-11 | implementation | MCP 为当前工作树取得 lease（事务绑定、可重放） | immutable-release | yes | yes | 15/8@EX-MC-09 | 15/8@EX-MC-09 | 1/2 | M | MC-40,DEP-MC-03,DEP-MC-02 | 序列化于 MC-40 | independent | N/A | EX-MC-09 |
| MC-12 | implementation | trace session 只读工具与查询共享 | immutable-release | yes | yes | 12/8@EX-MC-06 | 12/8@EX-MC-06 | 2/3 | M | MC-41,DEP-MC-03,DEP-MC-02 | 序列化于 MC-41 | independent | N/A | EX-MC-06 |
| MC-13 | implementation | loopback Streamable HTTP 传输与访问控制（legacy 纪元） | immutable-release | yes | yes | 74/8@EX-MC-04 | 74/8@EX-MC-04 | 2/4 | M | MC-20,DEP-MC-03,DEP-MC-02 | 序列化于 MC-20 | independent | N/A | EX-MC-04 |
| MC-14 | implementation | `2026-07-28` modern 纪元协议协商 | immutable-release | yes | yes | 49/8@EX-MC-05 | 49/8@EX-MC-05 | 1/4 | M | MC-17,DEP-MC-03,DEP-MC-02 | 序列化于 MC-17 | independent | N/A | EX-MC-05 |
| MC-15 | audit | 需求方客户端（WorkBuddy stdio、Grok Bot 远程 HTTPS）的接入验收 | revert | yes | yes | 12/20 | 12/20 | 0/0 | S | MC-14,DEP-MC-04,DEP-MC-05 | 序列化于 MC-14 | no-release | N/A | N/A |
| MC-16 | implementation | MCP 工具结果脱敏 | immutable-release | yes | yes | 2/8 | 2/8 | 1/2 | M | MC-22,DEP-MC-03,DEP-MC-02 | 序列化于 MC-22 | independent | MC-01 | N/A |
| MC-17 | implementation | HTTP 传输的并发准入与过载行为 | immutable-release | yes | yes | 8/8 | 8/8 | 1/2 | M | MC-13,DEP-MC-03,DEP-MC-02 | 序列化于 MC-13 | independent | MC-13 | N/A |
| MC-18 | implementation | commit 在 HEAD 更新事务内校验期望父提交（内部 CAS，CLI 不暴露） | immutable-release | yes | yes | 8/8 | 8/8 | 2/2 | M | MC-07,DEP-MC-03 | 序列化于 MC-07 | independent | N/A | N/A |
| MC-19 | implementation | MCP 工具结果的资源上限（大小、读 deadline） | immutable-release | yes | yes | 6/8 | 6/8 | 2/2 | M | MC-16,DEP-MC-03,DEP-MC-02 | 序列化于 MC-16 | independent | MC-16 | N/A |
| MC-20 | implementation | trace checkpoint 只读工具与查询共享 | immutable-release | yes | yes | 11/8@EX-MC-07 | 11/8@EX-MC-07 | 2/3 | M | MC-12,DEP-MC-03,DEP-MC-02 | 序列化于 MC-12 | independent | MC-12 | EX-MC-07 |
| MC-21 | implementation | MCP 工具调用速率限制（进程级令牌桶，传输无关） | immutable-release | yes | yes | 8/8 | 8/8 | 1/3 | M | MC-33,DEP-MC-03,DEP-MC-02 | 序列化于 MC-33 | independent | N/A | N/A |
| MC-22 | implementation | MCP 请求的 tracing span（只记服务端可控或已净化字段） | immutable-release | yes | yes | 8/8 | 8/8 | 1/2 | M | MC-01,DEP-MC-03 | 序列化于 MC-01 | independent | MC-01 | N/A |
| MC-23 | implementation | hook 运行器的调用方作用域拒绝策略（作用域外行为不变） | immutable-release | yes | yes | 8/8 | 8/8 | 1/1 | M | MC-18,DEP-MC-03 | 序列化于 MC-18 | independent | N/A | N/A |
| MC-24 | implementation | commit 前提扩展：HEAD 符号引用（入口与事务内） | immutable-release | yes | yes | 6/8 | 6/8 | 2/2 | M | MC-23,DEP-MC-03 | 序列化于 MC-23 | independent | N/A | N/A |
| MC-25 | implementation | `libra_status` 的快照身份（`head`/`head_ref`/`index_tree`） | immutable-release | yes | yes | 8/8 | 8/8 | 2/2 | M | MC-28,DEP-MC-03,DEP-MC-02 | 序列化于 MC-28 | independent | MC-02 | N/A |
| MC-26 | implementation | `libra_status` 的暂存差异（与 `index_tree` 同源） | immutable-release | yes | yes | 8/8 | 8/8 | 1/1 | M | MC-25,DEP-MC-03,DEP-MC-02 | 序列化于 MC-25 | independent | MC-25 | N/A |
| MC-27 | implementation | `libra_diff` staged 模式的快照绑定（patch 与 `head`/`head_ref`/`index_tree` 同源） | immutable-release | yes | yes | 8/8 | 8/8 | 3/3 | M | MC-26,DEP-MC-03,DEP-MC-02 | 序列化于 MC-26 | independent | N/A | N/A |
| MC-28 | implementation | commit 期望父提交的「未出生」状态（入口与事务内） | immutable-release | yes | yes | 6/8 | 6/8 | 2/2 | M | MC-43,DEP-MC-03 | 序列化于 MC-43 | independent | MC-24 | N/A |
| MC-29 | implementation | 读工具子进程执行器（有界并发、超时强制终止、回收后归还槽位） | immutable-release | yes | yes | 8/8 | 8/8 | 4/4 | M | MC-19,DEP-MC-03,DEP-MC-02 | 序列化于 MC-19 | independent | MC-19 | N/A |
| MC-31 | implementation | MCP diff 资源上限（尺寸预检、行数上限、关闭 rename） | immutable-release | yes | yes | 7/8 | 7/8 | 3/3 | M | MC-39,DEP-MC-03 | 序列化于 MC-39 | independent | MC-29 | N/A |
| MC-32 | implementation | 对象尺寸（delta 感知、不重建对象） | immutable-release | yes | yes | 5/8 | 5/8 | 1/1 | M | MC-35,DEP-MC-03 | 序列化于 MC-35 | independent | MC-31 | N/A |
| MC-33 | implementation | status 有界收集（扫描路径预算 + 全部动态数组上限） | immutable-release | yes | yes | 8/8 | 8/8 | 4/4 | M | MC-37,DEP-MC-03 | 序列化于 MC-37 | independent | MC-02 | N/A |
| MC-34 | implementation | 信封处理结果回报与 finalize 后重新计量 | immutable-release | yes | yes | 5/8 | 5/8 | 2/2 | M | MC-27,DEP-MC-03 | 序列化于 MC-27 | independent | MC-30 | N/A |
| MC-35 | implementation | 读子进程内存上限 | immutable-release | yes | yes | 7/8 | 7/8 | 2/2 | M | MC-29,DEP-MC-03 | 序列化于 MC-29 | independent | MC-29 | N/A |
| MC-36 | implementation | 工作树 blob 有界读取（符号链接语义保持） | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | MC-32,DEP-MC-03 | 序列化于 MC-32 | independent | MC-32 | N/A |
| MC-37 | implementation | diff 全局收集预算（patch 字节、条目整体、文件条目） | immutable-release | yes | yes | 8/8 | 8/8 | 2/2 | M | MC-31,DEP-MC-03 | 序列化于 MC-31 | independent | MC-31 | N/A |
| MC-38 | implementation | MCP 续期当前工作树的 lease（事务绑定、可重放、不缩短） | immutable-release | yes | yes | 15/8@EX-MC-10 | 15/8@EX-MC-10 | 1/1 | M | MC-11,DEP-MC-03,DEP-MC-02 | 序列化于 MC-11 | independent | MC-11 | EX-MC-10 |
| MC-41 | implementation | MCP 释放当前工作树的 lease（事务绑定、可重放） | immutable-release | yes | yes | 13/8@EX-MC-11 | 13/8@EX-MC-11 | 1/1 | M | MC-38,DEP-MC-03,DEP-MC-02 | 序列化于 MC-38 | independent | MC-38 | EX-MC-11 |
| MC-43 | implementation | 暂存树前提（写提交对象前比较） | immutable-release | yes | yes | 3/8 | 3/8 | 1/1 | M | MC-24,DEP-MC-03 | 序列化于 MC-24 | independent | MC-24 | N/A |
| MC-39 | implementation | LFS 指针有界生成（同一句柄、原始字节上限） | immutable-release | yes | yes | 5/8 | 5/8 | 2/2 | M | MC-36,DEP-MC-03 | 序列化于 MC-36 | independent | MC-36 | N/A |
| MC-40 | implementation | MCP lease 工作树身份（含主工作树） | immutable-release | yes | yes | 8/8 | 8/8 | 1/1 | M | MC-10,DEP-MC-03 | 序列化于 MC-10 | independent | MC-11 | N/A |
| MC-30 | implementation | staged diff 结果是否构成完整审核依据 | immutable-release | yes | yes | 8/8 | 8/8 | 1/1 | M | MC-34,DEP-MC-03,DEP-MC-02 | 序列化于 MC-34 | independent | MC-27 | N/A |

### Task MC-01: stdio 上的 legacy MCP 协议核心

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 新增 `libra agent mcp --stdio`：在当前仓库内按 legacy 纪元（`2025-11-25`/`2025-06-18`）提供 `initialize`、`notifications/initialized`、`ping`、`tools/list`、`tools/call`，以及 ADR-MC-08 的错误分层（协议错误与工具错误）。本卡不提供任何生产工具（`tools/list` 为空）；为验证工具错误路径，注册仅在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时存在的测试工具 `libra_test_echo`（原样返回参数对象）；建立工具注册契约（GC-MC-10）：每个工具注册时必须给出 `outputSchema = {"type": "object", "anyOf": [<成功结构>, TOOL_ERROR_SCHEMA]}`（`TOOL_ERROR_SCHEMA` 在 `protocol.rs` 单点定义）以及一个一致性夹具（在独立的临时仓库中完成该工具的前置条件——如 `--allow-write` 会话、暂存改动与 `expected_head`、先行 claim 取得的 fence——并返回一组成功参数与一组工具错误参数，结束时清理），缺任一项无法编译注册（根上的 `"type": "object"` 为 2025 两个 legacy 版本 wire schema 对 `Tool.outputSchema` 的要求）；新增注册表级一致性测试，逐个工具运行其夹具后以两组参数调用并用 dev-dependency `jsonschema`（以 `cargo add --dev jsonschema` 加入并写成精确版本 `=x.y.z`，版本号记入本卡证据）校验 `structuredContent`。请求 tracing span 由 MC-22 承接。结果信封由 MC-16（脱敏）与 MC-19（大小上限、读 deadline）承接。唯一行为轴：「stdio 上的 MCP 握手、错误分层与帧」。

**Out of scope:** 请求 tracing span（由 MC-22 承接）；结果信封（由 MC-16、MC-19 承接）；生产工具（由 MC-02 起承接）；HTTP（由 MC-13 承接）；modern 纪元（由 MC-14 承接）；`--repo`（DEFER-MC-03）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge 命令形状与 repo 身份解析 | `src/command/agent/bridge.rs:34-40`、`:112-158`、`:163-186` |
| agent 子命令注册与 EXAMPLES | `src/command/agent/mod.rs:109-145`、`:232-233`、`:451` |
| 脱敏函数 | `src/internal/ai/agent_bridge/redaction.rs:20-53`（`review::redact_untrusted`、`scrub_controls`） |
| bridge 稳定码 | `src/internal/ai/agent_bridge/protocol.rs:271-390` |
| 规范 stdio 规则、tools 错误处理 | 规范 `2025-11-25/basic/transports.mdx` 第 28-34 行；`2025-11-25/server/tools.mdx`「Error Handling」（pin `1155ca46`） |
| 版本面 | `Cargo.toml` `0.30.68`；`install.sh` `DEFAULT_VERSION="v0.30.68"`；`install.ps1` `$DefaultVersion = "v0.30.68"`。`v0.30.67` 只发布了 Linux 对象，stable manifest 仍是 `0.30.66`。 |
| 门二进制 | `command -v libra` = `/mnt/data/libra/target/debug/libra`；`libra --version` = `libra 0.30.68` |
| jsonschema | dev-dependency `jsonschema` `=0.59.0`（`default-features = false`） |
| A/B 门 | `agent_mcp_stdio_test` 22 个 `--exact` 与 `agent_mcp_conformance_test` 7 个 `--exact` 均 `test result: ok. 1 passed; 0 failed`（`source .env.test && source .env.live-test`）。`0.30.68` 上两次真实 `libra agent mcp --stdio`：`protocolVersion` 为 `2025-11-25`，`serverInfo.name` 为 `libra`，`serverInfo.version` 为 `0.30.68`；`tools/list` 无 `libra_test_`。 |
| C 组全量 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`：`Summary [ 554.824s] 10576 tests run: 10576 passed (11 slow, 1 flaky), 21 skipped`，exit 0。首轮唯一失败是 `compat_serial_registry` 的外部组计数（自定义 `mcp_registry` 车道），已改为 `#[serial(env)]` 并重跑至绿。 |
| 网站 | `cf` `e3a18c80c692a68c6b824022f39b730c357ce25a` 只改 `agent.en.md`。源文件与 `https://libra.tools/en/docs/commands/agent` 都含 `agent mcp --stdio`、`2025-11-25`、`2025-06-18`、`LBR-AGENT-027`、`"cwd"`。Claude Code：`CLAUDE_VERDICT: PASS`，`P0=0`，`P1=0`。全库 `--check` 仍有 9 个非本卡页面漂移，未同步。GC-MC-05 ⓪ 演练在临时库通过（本机无 `shasum`，摘要用 `sha256sum`）。 |
| D 组 | tag `v0.30.68` 指向 `e418297e8d94e6edd7d263963ed0eb4bbc2603f3`。`release.yml` [39427212730](https://github.com/libra-tools/libra/actions/runs/39427212730) success；四个平台产物 HTTP 200；安装脚本 `DEFAULT_VERSION="v0.30.68"`。main `e4d843d1c6591f7064c9e1a8478e046f503c1978` 的 `base.yml` [39427673509](https://github.com/libra-tools/libra/actions/runs/39427673509) attempt 2 success（attempt 1 仅 `compat-redundancy` 在下载 `actions/checkout` 时 SSL 失败，重跑成功）。`codeql.yml` [39427673357](https://github.com/libra-tools/libra/actions/runs/39427673357) success。Codex：`VERDICT: PASS`，`P0=0`，`P1=0`（P2 是 Examples 代码块缺命令，本收口已补）。 |

**Acceptance criteria:**

- [x] AC-1 `initialize` 请求 `2025-11-25` 时结果 `protocolVersion` 等于 `"2025-11-25"`。
- [x] AC-2 `initialize` 请求 `2025-06-18` 时结果 `protocolVersion` 等于 `"2025-06-18"`。
- [x] AC-3 `initialize` 请求 `1900-01-01` 时结果 `protocolVersion` 等于 `"2025-11-25"`。
- [x] AC-4 `initialize` 结果 `serverInfo.name` 等于 `"libra"`。
- [x] AC-5 `initialize` 结果 `serverInfo.version` 等于 `Cargo.toml` 的 `[package].version`。
- [x] AC-6 `initialize` 结果 `capabilities` 等于 `{"tools":{"listChanged":false}}`。
- [x] AC-7 成功 `initialize` 之前发送不带 `_meta` 的 `tools/list` 得到 JSON-RPC 错误响应（具体错误码：MC-01 时为 `-32600`，MC-14 起为 `-32602`，由 MC-14 的门判定）。
- [x] AC-8 `initialize` 之后 `ping` 的结果等于 `{}`。
- [x] AC-9 `initialize` 之后发送 `notifications/initialized` 再发送 `ping`，stdout 新增的行恰为该 `ping` 的响应（通知不产生任何输出）。
- [x] AC-10 JSON 数组形式的 batch 得到协议错误 `-32600`。
- [x] AC-11 未知方法得到协议错误 `-32601`。
- [x] AC-12 不是合法 JSON 的输入行得到协议错误 `-32700`。
- [x] AC-13 未设置 `LIBRA_TEST` 时 `tools/list` 中没有名称以 `libra_test_` 开头的工具（MC-01 发布时列表为空，后续卡新增生产工具不影响本门）。
- [x] AC-14 `tools/call` 未知工具名得到协议错误 `-32602`。
- [x] AC-15 `libra_test_echo` 传入 schema 未声明的字段得到工具错误 `LBR-AGENT-027`。
- [x] AC-16 一次会话中 stdout 的每一行都能解析为 JSON-RPC 消息。
- [x] AC-17 超过 256 KiB 的输入行得到协议错误 `-32600`。
- [x] AC-18 超长输入行之后的下一个 `ping` 得到 `{}`。
- [x] AC-19 stdin EOF 后进程退出码为 0。
- [x] AC-20 在非仓库目录启动时进程退出码非 0。
- [x] AC-21 在非仓库目录启动且 stdin 保持打开时，进程在 5 s 内自行退出。
- [x] AC-22 不带 `--stdio`/`--transport` 时以用法拒绝 `LBR-CLI-002` 结束。
- [x] AC-23 注册表中每个工具的 `outputSchema.anyOf` 都包含 `TOOL_ERROR_SCHEMA`。
- [x] AC-24 注册表中每个工具以其成功示例参数调用时，结果的 `isError` 不为 `true`。
- [x] AC-25 同一成功结果的 `structuredContent` 通过该工具 `outputSchema.anyOf` 的成功分支校验。
- [x] AC-26 注册表中每个工具以其工具错误示例参数调用时，结果的 `isError` 为 `true`。
- [x] AC-27 同一错误结果的 `structuredContent` 通过 `TOOL_ERROR_SCHEMA` 校验。
- [x] AC-28 `tools/list` 结果通过固定提交 `1155ca46` 的 `schema/2025-11-25/schema.json` 中 `ListToolsResult` 定义的 JSON Schema 校验（schema 副本存于测试 fixture）。
- [x] AC-29 同一结果通过同一提交 `schema/2025-06-18/schema.json` 中 `ListToolsResult` 的校验。
- [x] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**判据规范（非计数正文）:** 门族（29 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`agent_mcp_stdio_test::initialize_echoes_2025_11_25`；AC-2=`agent_mcp_stdio_test::initialize_echoes_2025_06_18`；AC-3=`agent_mcp_stdio_test::initialize_unknown_version_falls_back`；AC-4=`agent_mcp_stdio_test::initialize_server_name`；AC-5=`agent_mcp_stdio_test::initialize_server_version`；AC-6=`agent_mcp_stdio_test::initialize_capabilities_tools_only`；AC-7=`agent_mcp_stdio_test::request_before_initialize_is_error`；AC-8=`agent_mcp_stdio_test::ping_after_initialize`；AC-9=`agent_mcp_stdio_test::initialized_notification_is_silent`；AC-10=`agent_mcp_stdio_test::batch_is_rejected`；AC-11=`agent_mcp_stdio_test::unknown_method_is_32601`；AC-12=`agent_mcp_stdio_test::invalid_json_is_32700`；AC-13=`agent_mcp_stdio_test::test_tools_hidden_in_production`；AC-14=`agent_mcp_stdio_test::unknown_tool_is_32602`；AC-15=`agent_mcp_stdio_test::schema_violation_is_tool_error`；AC-16=`agent_mcp_stdio_test::stdout_is_only_jsonrpc`；AC-17=`agent_mcp_stdio_test::oversized_line_is_32600`；AC-18=`agent_mcp_stdio_test::server_survives_oversized_line`；AC-19=`agent_mcp_stdio_test::eof_exits_zero`；AC-20=`agent_mcp_stdio_test::outside_repository_fails`；AC-21=`agent_mcp_stdio_test::outside_repository_exits_without_eof`；AC-22=`agent_mcp_stdio_test::missing_transport_is_usage_error`；AC-23=`agent_mcp_conformance_test::every_tool_schema_includes_error_shape`；AC-24=`agent_mcp_conformance_test::every_tool_success_example_is_success`；AC-25=`agent_mcp_conformance_test::every_tool_success_matches_success_branch`；AC-26=`agent_mcp_conformance_test::every_tool_error_example_is_error`；AC-27=`agent_mcp_conformance_test::every_tool_error_matches_error_schema`；AC-28=`agent_mcp_conformance_test::tools_list_matches_2025_11_25_schema`；AC-29=`agent_mcp_conformance_test::tools_list_matches_2025_06_18_schema`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [x] AC-1：`source .env.test && cargo test --test agent_mcp_stdio_test initialize_echoes_2025_11_25 -- --exact`（new）
- [x] AC-2：`source .env.test && cargo test --test agent_mcp_stdio_test initialize_echoes_2025_06_18 -- --exact`（new）
- [x] AC-3：`source .env.test && cargo test --test agent_mcp_stdio_test initialize_unknown_version_falls_back -- --exact`（new）
- [x] AC-4：`source .env.test && cargo test --test agent_mcp_stdio_test initialize_server_name -- --exact`（new）
- [x] AC-5：`source .env.test && cargo test --test agent_mcp_stdio_test initialize_server_version -- --exact`（new）
- [x] AC-6：`source .env.test && cargo test --test agent_mcp_stdio_test initialize_capabilities_tools_only -- --exact`（new）
- [x] AC-7：`source .env.test && cargo test --test agent_mcp_stdio_test request_before_initialize_is_error -- --exact`（new）
- [x] AC-8：`source .env.test && cargo test --test agent_mcp_stdio_test ping_after_initialize -- --exact`（new）
- [x] AC-9：`source .env.test && cargo test --test agent_mcp_stdio_test initialized_notification_is_silent -- --exact`（new）
- [x] AC-10：`source .env.test && cargo test --test agent_mcp_stdio_test batch_is_rejected -- --exact`（new）
- [x] AC-11：`source .env.test && cargo test --test agent_mcp_stdio_test unknown_method_is_32601 -- --exact`（new）
- [x] AC-12：`source .env.test && cargo test --test agent_mcp_stdio_test invalid_json_is_32700 -- --exact`（new）
- [x] AC-13：`source .env.test && cargo test --test agent_mcp_stdio_test test_tools_hidden_in_production -- --exact`（new）
- [x] AC-14：`source .env.test && cargo test --test agent_mcp_stdio_test unknown_tool_is_32602 -- --exact`（new）
- [x] AC-15：`source .env.test && cargo test --test agent_mcp_stdio_test schema_violation_is_tool_error -- --exact`（new）
- [x] AC-16：`source .env.test && cargo test --test agent_mcp_stdio_test stdout_is_only_jsonrpc -- --exact`（new）
- [x] AC-17：`source .env.test && cargo test --test agent_mcp_stdio_test oversized_line_is_32600 -- --exact`（new）
- [x] AC-18：`source .env.test && cargo test --test agent_mcp_stdio_test server_survives_oversized_line -- --exact`（new）
- [x] AC-19：`source .env.test && cargo test --test agent_mcp_stdio_test eof_exits_zero -- --exact`（new）
- [x] AC-20：`source .env.test && cargo test --test agent_mcp_stdio_test outside_repository_fails -- --exact`（new）
- [x] AC-21：`source .env.test && cargo test --test agent_mcp_stdio_test outside_repository_exits_without_eof -- --exact`（new）
- [x] AC-22：`source .env.test && cargo test --test agent_mcp_stdio_test missing_transport_is_usage_error -- --exact`（new）
- [x] AC-23：`source .env.test && cargo test --test agent_mcp_conformance_test every_tool_schema_includes_error_shape -- --exact`（new）
- [x] AC-24：`source .env.test && cargo test --test agent_mcp_conformance_test every_tool_success_example_is_success -- --exact`（new）
- [x] AC-25：`source .env.test && cargo test --test agent_mcp_conformance_test every_tool_success_matches_success_branch -- --exact`（new）
- [x] AC-26：`source .env.test && cargo test --test agent_mcp_conformance_test every_tool_error_example_is_error -- --exact`（new）
- [x] AC-27：`source .env.test && cargo test --test agent_mcp_conformance_test every_tool_error_matches_error_schema -- --exact`（new）
- [x] AC-28：`source .env.test && cargo test --test agent_mcp_conformance_test tools_list_matches_2025_11_25_schema -- --exact`（new）
- [x] AC-29：`source .env.test && cargo test --test agent_mcp_conformance_test tools_list_matches_2025_06_18_schema -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [x] `docs/error-codes.md`：`rg -qF -- 'libra agent mcp' docs/error-codes.md`
- [x] `docs/commands/agent.md`：`rg -qF -- 'agent mcp --stdio' docs/commands/agent.md && rg -qF -- '2025-11-25' docs/commands/agent.md && rg -qF -- '2025-06-18' docs/commands/agent.md && rg -qF -- 'LBR-AGENT-027' docs/commands/agent.md && rg -qF -- '"cwd"' docs/commands/agent.md`
- [x] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'agent mcp --stdio' docs/commands/zh-CN/agent.md && rg -qF -- '2025-11-25' docs/commands/zh-CN/agent.md && rg -qF -- '2025-06-18' docs/commands/zh-CN/agent.md && rg -qF -- 'LBR-AGENT-027' docs/commands/zh-CN/agent.md && rg -qF -- '"cwd"' docs/commands/zh-CN/agent.md`
- [x] `docs/development/commands/agent.md`：`rg -qF -- 'mcp_server' docs/development/commands/agent.md && rg -qF -- 'BridgeRequest' docs/development/commands/agent.md`
- [x] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'agent mcp --stdio'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [x] 网站源文件 `agent`：`rg -qF -- 'agent mcp --stdio' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '2025-11-25' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '2025-06-18' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'LBR-AGENT-027' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '"cwd"' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'agent mcp --stdio' && printf '%s' "$PAGE" | rg -qF -- '2025-11-25' && printf '%s' "$PAGE" | rg -qF -- '2025-06-18' && printf '%s' "$PAGE" | rg -qF -- 'LBR-AGENT-027' && printf '%s' "$PAGE" | rg -qF -- '"cwd"'`（GC-MC-05 ⑥）
- [x] 测试索引：`rg -q '^\| `agent_mcp_stdio_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_stdio_test"' Cargo.toml`
- [x] 测试索引：`rg -q '^\| `agent_mcp_conformance_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_conformance_test"' Cargo.toml`
- [x] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: 新增 agent 子命令注册与 help EXAMPLES 合同，并在 Cargo.toml 新增 [[test]] 行`

**Dependencies:** `DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `tests/fixtures/mcp/schema-2025-11-25.json`、`tests/fixtures/mcp/schema-2025-06-18.json`（新，固定提交 `1155ca460e5097019f32769b38dcba194432e2fb` 的副本）、`src/command/agent/mcp.rs`（新）、`src/command/agent/mod.rs`、`src/internal/ai/mod.rs`、`src/internal/ai/mcp_server/mod.rs`（新）、`src/internal/ai/mcp_server/protocol.rs`（新）、`src/internal/ai/mcp_server/stdio.rs`（新）、`src/internal/ai/mcp_server/tools/mod.rs`（新）、`tests/agent_mcp_stdio_test.rs`（新）、`docs/error-codes.md`、`tests/agent_mcp_conformance_test.rs`（新）、`Cargo.toml`（`[dev-dependencies]` 新增 `jsonschema` 精确版本）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/error-codes.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：子命令表新增 `mcp` 行、EXAMPLES 新增 `libra agent mcp --stdio`、新增「MCP server」小节（协议版本、错误分层、stdio 客户端配置示例 `command`/`args`/`cwd`，GC-MC-06）；开发文档写 MCP 前端架构（ADR-MC-02/03/08）；`COMPATIBILITY.md` 第 252 行末尾追加一句：`agent mcp` 是新的仓库操作 MCP 面，与已删除的 `libra code` MCP 不同；`docs/error-codes.md` 在 `LBR-AGENT-022..038` 段落前加一句：MCP 前端（`libra agent mcp`）以工具错误形式复用这些码（英文措辞含 `libra agent mcp`），各码在 MCP 下的触发条件见 agent 命令文档。不受影响：——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** stdout 只写合法 JSON-RPC；scope 只来自 `libra.repoid` 与 cwd；测试工具在 release 构建中不存在（`cfg(debug_assertions)`）。

**Performance budget:** 单行输入 ≤ 256 KiB；stdio 请求按到达顺序串行处理。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=stdio 上的 legacy MCP 协议核心（握手、错误分层、帧）; recovery=immutable-release，发新版本修复，旧版本无 agent mcp 子命令; complete=yes; self-contained=yes; AC=29/8@EX-MC-01; VER=29/8@EX-MC-01; landing=4; prod-files=7; scope=M; deps=DEP-MC-03,DEP-MC-02; writeset=no-overlap; release=independent; split-from=N/A; exception=EX-MC-01`

### Task MC-02: `libra_status` 只读工具

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 新增首个生产工具 `libra_status`：无参数，`structuredContent = {status: <MC-33 的 collect_status_json_envelope_bounded(cwd, 1000) 的 data>}`（扫描路径预算 100 000、全部动态数组各至多 1000 条，只有发生省略时才出现 `omitted_count`，因此无省略时与 CLI `data` 形状相同），经结果信封返回；未出生 HEAD 的新仓库同样成功（与 `libra status` 一致）。一致快照字段 `head`/`index_tree`/`staged_changes` 由 MC-25 增加。唯一行为轴：「MCP 读取仓库状态」。

**Out of scope:** diff（由 MC-05 承接）；写工具（由 MC-06 起承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| status envelope 复用点 | `src/command/status.rs:477-521` |
| 结果信封（MC-16、MC-19 交付） | `src/internal/ai/mcp_server/output.rs` |

**Acceptance criteria:**

- [ ] AC-1 `tools/list` 中 `libra_status` 条目的 `annotations` 等于 `{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true}`。
- [ ] AC-2 `libra_status` 的 `inputSchema` 等于 `{"type":"object","properties":{},"additionalProperties":false}`。
- [ ] AC-3 对 secret-free fixture，`structuredContent.status` 等于 `libra --json status` 的 `data`。
- [ ] AC-4 `content[0].text` 解析后等于 `structuredContent`。
- [ ] AC-5 带任意参数调用得到工具错误 `LBR-AGENT-027`。
- [ ] AC-6 在未出生 HEAD 的新仓库（`libra init` 后无提交）中，`libra_status` 结果的 `structuredContent.status` 等于 `libra --json status` 的 `data`（调用成功是取得该值的前提，不单独计门）。
- [ ] AC-7 含 20 000 个未跟踪文件的仓库中 4 个并发 `libra_status` 全部成功。
- [ ] AC-8 同一并发调用中每个响应行的字节数 ≤ 262144。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_status_test status_tool_annotations -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_status_test status_tool_input_schema -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_status_test status_tool_matches_status_json -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_status_test status_text_mirrors_structured -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_status_test status_extra_argument_is_tool_error -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_status_test status_tool_works_on_unborn_head -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_status_test status_concurrent_large_repo_succeeds -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_status_test status_concurrent_large_repo_response_bounded -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_status' docs/commands/agent.md && rg -qF -- 'omitted_count' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_status' docs/commands/zh-CN/agent.md && rg -qF -- 'omitted_count' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_status'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'libra_status' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'omitted_count' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'libra_status' && printf '%s' "$PAGE" | rg -qF -- 'omitted_count'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_status_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_status_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-21`（消费结果信封与限流器；MC-01、MC-16、MC-19 已先发布）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/tools/mod.rs`、`src/internal/ai/mcp_server/tools/repo.rs`（新）、`tests/agent_mcp_status_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：工具表新增 `libra_status`（扫描路径预算 100 000、全部动态数组各至多 1000 条，只有发生省略时才出现 `omitted_count`，因此无省略时与 CLI `data` 形状相同）；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A（无新架构）。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 只返回 `libra status --json` 已公开字段，经 GC-MC-03 信封。

**Performance budget:** 扫描成本同 `libra status`；结果每列表 ≤ 1000 条；读 deadline 30 s（读子进程强制终止）。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=libra_status 只读工具; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=2; scope=M; deps=MC-21,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-21; release=independent; split-from=N/A; exception=N/A`

### Task MC-03: Repository migration：新建 `agent_mcp_*` 表集

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 新增 Repository 角色 migration，创建与 bridge 表同构的 `agent_mcp_session`（`source CHECK = 'mcp'`）、`agent_mcp_operation`、`agent_mcp_checkpoint`、`agent_mcp_link`（列与约束照抄 `agent_bridge_*` 当前定义，`link` 取 `2026082401` 后的形状），以及 MCP 意图表 `agent_mcp_intent`（`operation_id TEXT PRIMARY KEY`——存 actor 限定的 durable id `<actor>~<client operation_id>`，与 `agent_mcp_operation.operation_id` 同值；`tool TEXT NOT NULL`、`request_json TEXT NOT NULL CHECK(length(CAST(request_json AS BLOB)) <= 262144)`、`created_at INTEGER NOT NULL`、`result_json TEXT NULL CHECK(result_json IS NULL OR length(CAST(result_json AS BLOB)) <= 262144)`——工具结果的规范化 JSON，在 operation 记为 `applied` 的同一事务内写入，供写会话在重放时复用首次请求并原样返回首次结果，见 ADR-MC-06），、MCP 用量计数表 `agent_mcp_usage`（单行：`id INTEGER PRIMARY KEY CHECK(id = 1)`、`groups INTEGER NOT NULL`、`payload_bytes INTEGER NOT NULL`，供 MC-06 在写事务内增量维护并强制上限，配额检查只读这一行），以及 MCP lease 记录表 `agent_mcp_lease`（`workspace_id TEXT PRIMARY KEY`、`actor TEXT NOT NULL`、`claim_operation_id TEXT NOT NULL`、`lease_ttl_ms INTEGER NOT NULL CHECK(lease_ttl_ms BETWEEN 1000 AND 3600000)`、`created_at INTEGER NOT NULL`；MC-11 claim 在同一事务内写入，MC-38 renew 按 `workspace_id` 读取 claim 时的 TTL）；不建 event 表（MCP 无 ingress）。同卡把七张新表登记到两个表归属注册表（`CODE_AGENT_TABLE_OWNERSHIP` 与 `MUTABLE_STATE_OWNERSHIP`，与 `agent_bridge_*` 同为 Repository 所有）。本卡只建表，不改任何读写代码。

**Out of scope:** bridge 代码按 source 选择表集（由 MC-04 承接）；MCP 前端使用（由 MC-06 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge 表定义 | `sql/migrations/2026081801_agent_bridge_capture.sql:13-75`；`2026082401_agent_bridge_link_relations.sql:37-60` |
| Repository-only 注册与当前最大版本 | `src/internal/db/migration.rs:1738`（`repository_migrations`）、`:2438`（`2026101902_worktree_layout_capability`）；`src/internal/db/schema.rs:239-258` |
| 表归属注册表（穷举守卫） | `src/internal/config_ownership.rs:180-260`（`CODE_AGENT_TABLE_OWNERSHIP`，`agent_bridge_operation` 在 `:247`）；`src/internal/mutable_state_ownership.rs:264`、`:1223-1240`（`MUTABLE_STATE_OWNERSHIP` 穷举断言） |
| 未来版本拒绝 | `src/internal/db/schema.rs:368-375`、`:389-400` |

**Acceptance criteria:**

- [ ] AC-1 升级后 `sqlite_master` 中名称以 `agent_mcp_` 开头的表集合等于 `{agent_mcp_session, agent_mcp_operation, agent_mcp_checkpoint, agent_mcp_link, agent_mcp_intent, agent_mcp_lease, agent_mcp_usage}`。
- [ ] AC-2 升级前后 `agent_bridge_*` 的 `(name, sql)` 列表相等。
- [ ] AC-3 升级前后 `agent_bridge_*` 的 `(name, count(*))` 列表相等。
- [ ] AC-4 向 `agent_mcp_session` 插入 `source='deepseek-harness'` 被 CHECK 拒绝。
- [ ] AC-5 空库全量 migration 与旧库升级得到的 `agent_mcp_*` 的 `(name, sql)` 列表相等。
- [ ] AC-6 在建表中途注入失败（`LIBRA_TEST=1` failpoint）后，`(sqlite_master, schema_versions)` 快照等于升级前快照。
- [ ] AC-7 `config_ownership` 的表归属守卫在新增七表后通过。
- [ ] AC-8 `mutable_state_ownership` 的穷举守卫在新增七表后通过。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_bridge_migration_test mcp_tables_created -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_bridge_migration_test harness_table_sql_unchanged -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_bridge_migration_test harness_rows_unchanged -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_bridge_migration_test mcp_session_source_check -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_bridge_migration_test mcp_tables_fresh_matches_upgrade -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_bridge_migration_test mcp_migration_interrupted_rolls_back -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --lib internal::config_ownership::tests::every_code_agent_table_and_cache_is_classified -- --exact`（既有）
- [ ] AC-8：`source .env.test && cargo test --lib internal::mutable_state_ownership::tests::mutable_state_scope_registration_guard -- --exact`（既有）

**ER-04 A 组 migration 强制门与 GC-13 隔离门（全局强制，不计入 G-03）:**

- [ ] forward 幂等重放：同一库连续两次运行全部 migration，第二次不改变 `(sqlite_master, schema_versions)` 快照。 `source .env.test && cargo test --test agent_bridge_migration_test mcp_store_migration_replay_is_idempotent -- --exact`（new）
- [ ] down 回滚（有行）：存在 `agent_mcp_*` 行时 down 返回错误。 `source .env.test && cargo test --test agent_bridge_migration_test mcp_down_refuses_with_rows -- --exact`（new）
- [ ] down 回滚（有行）：拒绝后数据库快照不变。 `source .env.test && cargo test --test agent_bridge_migration_test mcp_down_keeps_rows -- --exact`（new）
- [ ] down 回滚（无行）：down 后 `sqlite_master` 等于升级前。 `source .env.test && cargo test --test agent_bridge_migration_test mcp_down_drops_empty_tables -- --exact`（new）
- [ ] 旧二进制读新库：上一发布版（本卡 bump 前 `Cargo.toml` 的版本）对本卡二进制初始化的仓库执行 `status`，以非 0 退出且 stderr 含 `install a newer Libra binary`；前置断言：下载的二进制版本等于 `PREV`，新库 `schema_versions` 最大值等于本卡 migration 版本；临时目录由 `trap` 清理，不保存为证据。 以 `bash` 执行下列脚本，退出码 0 为通过：

  ```bash
  set -euo pipefail
  PREV="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"   # 在本卡 bump 之前执行
  NEW="$(ls sql/migrations | sed -n 's/^\([0-9]*\)_agent_mcp_store\.sql$/\1/p')"
  test -n "$NEW"
  case "$(uname -s)/$(uname -m)" in
    Darwin/arm64) ART=libra-darwin-arm64 ;;
    Linux/x86_64) ART=libra-linux-amd64 ;;
    Linux/aarch64) ART=libra-linux-arm64 ;;
    *) echo "ERROR: no released artifact for this executor platform"; exit 2 ;;
  esac
  D="$(mktemp -d)"; trap 'rm -rf "$D"' EXIT
  curl -fsSLo "$D/libra-prev" "https://download.libra.tools/libra/releases/v$PREV/$ART"
  chmod +x "$D/libra-prev"
  test "$("$D/libra-prev" --version)" = "libra $PREV"
  cargo build --quiet
  NEWBIN="$PWD/target/debug/libra"
  mkdir "$D/repo"; (cd "$D/repo" && "$NEWBIN" init >/dev/null)
  test "$(sqlite3 "$D/repo/.libra/libra.db" 'SELECT max(version) FROM schema_versions')" = "$NEW"
  if (cd "$D/repo" && "$D/libra-prev" status) 2>"$D/err"; then echo "FAIL: previous binary opened the new database"; exit 1; fi
  grep -qF 'install a newer Libra binary' "$D/err"
  echo OK
  ```
- [ ] GC-13：升级后临时 `LIBRA_CONFIG_GLOBAL_DB` 的 `(schema receipt, 表集合, config_kv 摘要)` 快照不变。 `source .env.test && cargo test --test agent_bridge_migration_test mcp_migration_leaves_global_config -- --exact`（new）
- [ ] GC-13：升级后临时 `LIBRA_CONFIG_SYSTEM_DB` 的同一快照不变。 `source .env.test && cargo test --test agent_bridge_migration_test mcp_migration_leaves_system_config -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `sql/migrations/README.md`：`rg -qF -- 'agent_mcp_store' sql/migrations/README.md`
- [ ] `docs/development/tracing/deepseek-harness.md`：`rg -qF -- 'agent_mcp_' docs/development/tracing/deepseek-harness.md`
- [ ] `docs/commands/agent.md`：`rg -qF -- 'install a newer Libra binary' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'install a newer Libra binary' docs/commands/zh-CN/agent.md`
- [ ] 网站源文件 `agent`：`rg -qF -- 'install a newer Libra binary' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'install a newer Libra binary'`（GC-MC-05 ⑥）
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: sql/migrations/** 与共享表归属注册表（config_ownership、mutable_state_ownership）；T-3: Task type = migration`

**Dependencies:** `MC-02`（写集串行：`tests/INDEX.md`）；`DEP-MC-01`；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `sql/migrations/<next>_agent_mcp_store.sql`（新；版本号取开工时 `sql/migrations` 最大版本之后的下一个空闲值；2026-10-19 11:01 UTC 最大为 `2026101902`，故不小于 `2026101903`）、`sql/migrations/<next>_agent_mcp_store_down.sql`（新）、`sql/migrations/README.md`、`src/internal/db/migration.rs`、`src/internal/config_ownership.rs`、`src/internal/mutable_state_ownership.rs`、`tests/agent_bridge_migration_test.rs`、`tests/INDEX.md`、`docs/development/tracing/deepseek-harness.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`sql/migrations/README.md`、`docs/development/tracing/deepseek-harness.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：`sql/migrations/README.md` 新增 migration 行；`docs/development/tracing/deepseek-harness.md` 追加：MCP 专用表集、降级屏障（新二进制打开仓库后旧二进制以 `UnsupportedFuture` 拒绝打开）与「升级前停止正在运行的 `agent bridge` 进程」；`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md` 与网站页新增「升级说明」小节：本版本起仓库 schema 前进，旧版 `libra` 打开该仓库会报 `install a newer Libra binary`，升级前须停止正在运行的 `libra agent bridge` 进程，升级后不要回退二进制；`COMPATIBILITY.md`、`docs/error-codes.md`：N/A（无新命令表面、无新码）。不受影响：`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `forward-only`

**Migration and rollback:** GC-13 清单——数据库角色：只 `Repository`；migration allowlist：只本卡新增的一个版本（登记在 `repository_migrations()`，`schema.rs` 不改）；bootstrap：无改动（`rg -n agent_bridge_session sql/*.sql` 零命中，bridge 表只由 migration 创建）；top-up：无改动；schema ledger：Repository 的 `schema_versions`；`GlobalConfig`/`SystemConfig`：不得写入、不推进 receipt（强制门 GC-13 两项）。up：在 migration 事务内 `CREATE TABLE IF NOT EXISTS` 七张表，不触碰 `agent_bridge_*`。数据不变量：`agent_bridge_*` 结构与行数不变（AC-2/3）。down：存在任何 `agent_mcp_*` 行则以可操作信息拒绝并保持原状，否则删除七张表。恢复验证命令：`source .env.test && cargo test --test agent_bridge_migration_test`。旧/新二进制：新二进制打开仓库后 schema 版本前进，旧二进制以 `UnsupportedFuture` 拒绝（强制门「旧二进制读新库」）；升级时已在运行的旧 `agent bridge` 进程不在支持范围内，文档要求先停止。用户影响：升级后不要回退二进制。

**Security and privacy:** 新表只由服务端常量选择；不影响 Harness 数据。

**Performance budget:** 只建空表，O(1)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=Repository 角色新建 MCP 专用表集（不触碰 agent_bridge_*）; recovery=forward-only，down 在有 agent_mcp 行时拒绝，前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=3; prod-files=5; scope=M; deps=MC-02,DEP-MC-01,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-02; release=independent; split-from=N/A; exception=N/A`

### Task MC-04: bridge 代码按 source 选择表集

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** `BridgeContext` 增加 `source: BridgeSource`（`DeepseekHarness`/`Mcp`），`BridgeSource::tables()` 返回固定表名集合（Harness 恰为现有 `agent_bridge_*` 名，MCP 为 `agent_mcp_*`，event 表为 `None`：MCP 上下文的 event 计数恒为 0，ingress 方法以 `LBR-AGENT-032` 拒绝）；storage/methods/mutations/provenance/ingress/workspace 中的 SQL 由该集合构造，actor 绑定、provenance 与 workspace owner 使用上下文 source；`bridge --stdio` 固定 `DeepseekHarness`。两个 source 因此拥有完全独立的主键空间，Harness 的表、SQL 文本与标识语义不变。唯一行为轴：「bridge 读写按 source 隔离」。

**Out of scope:** MCP 前端（由 MC-06 承接）；表结构（MC-03 已交付）；第三个 source（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| source/actor/owner 写死 | `storage.rs:85-142`；`mutations.rs:146`、`:333`、`:574`；`agent_bridge/workspace.rs:79-104` |
| 读查询与 scope 校验 | `methods.rs:342-552`；`provenance.rs:159-179`；`ingress.rs:46-63` |
| 直接构造 `BridgeContext` 的既有测试 | `tests/agent_bridge_{approval,checkpoint,ingress,mutation,read_methods,redaction,security,span,vcs,workspace}_test.rs` |
| MCP 表集（MC-03 交付） | `agent_mcp_session/operation/checkpoint/link` |

**Acceptance criteria:**

- [ ] AC-1 `BridgeSource::DeepseekHarness.tables()` 的表名等于现有 `agent_bridge_*` 名。
- [ ] AC-2 存在 MCP session 时，Harness 上下文 `context.get` 的结果不含该 session id。
- [ ] AC-3 存在 MCP checkpoint 时，Harness 上下文 `checkpoint.list` 的结果不含该 checkpoint id。
- [ ] AC-4 两个表集都有数据时，Harness 上下文 `status.get` 的 `data` 等于只有 Harness 数据时的值。
- [ ] AC-5 两个表集都有数据时，Harness 上下文 `history.search`（空查询）的 session 集合等于只有 Harness 数据时的集合。
- [ ] AC-6 Harness 上下文以 MCP checkpoint id 调用 `checkpoint.show` 得到 `LBR-AGENT-032`。
- [ ] AC-7 Harness 上下文以 MCP checkpoint id 调用 `diff.get`（`mode = "checkpoint"`）得到 `LBR-AGENT-032`。
- [ ] AC-8 Harness 已以 `operation_id = "op-1"` 调用 `checkpoint.create` 后，MCP 上下文以同一 id 调用成功。
- [ ] AC-9 MCP 上下文已以 `"op-1"` 调用后，Harness 以同一 id 调用成功。
- [ ] AC-10 Harness 先、MCP 后的顺序下，Harness operation 行的 `params_digest` 等于单独运行 Harness 时的值。
- [ ] AC-11 MCP 先、Harness 后的顺序下，Harness operation 行的 `params_digest` 等于单独运行 Harness 时的值。
- [ ] AC-12 MCP 上下文以 Harness session id 调用 mutation 得到 `LBR-AGENT-032`。
- [ ] AC-13 Harness 上下文以 MCP session id 调用 mutation 得到 `LBR-AGENT-032`。
- [ ] AC-14 MCP 上下文 workspace claim 记录的 owner 等于 `mcp:<bridge_session_id>`。
- [ ] AC-15 MCP 上下文 `status.get` 的 `data.bridge_events` 等于 0（MCP 表集无 event 表）。
- [ ] AC-16 MCP 上下文调用 `event.append` 得到 `LBR-AGENT-032`（ingress 只属 Harness）。
- [ ] AC-17 10 个既有 bridge 测试文件的改动只有在 `BridgeContext` 字面量中新增的全限定行 `source: libra::internal::ai::agent_bridge::ingress::BridgeSource::DeepseekHarness,`（无需改 `use`；`libra diff` 或过滤器自身失败判为 ERROR，不判为通过）。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**判据规范（非计数正文）:** 门族（17 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`--lib internal::ai::agent_bridge::ingress::tests::harness_table_names_unchanged`；AC-2=`agent_bridge_source_isolation_test::harness_context_excludes_mcp`；AC-3=`agent_bridge_source_isolation_test::harness_checkpoint_list_excludes_mcp`；AC-4=`agent_bridge_source_isolation_test::harness_status_unaffected_by_mcp_rows`；AC-5=`agent_bridge_source_isolation_test::harness_history_unaffected_by_mcp_rows`；AC-6=`agent_bridge_source_isolation_test::harness_checkpoint_show_cannot_see_mcp`；AC-7=`agent_bridge_source_isolation_test::harness_checkpoint_diff_cannot_see_mcp`；AC-8=`agent_bridge_source_isolation_test::same_operation_id_harness_first`；AC-9=`agent_bridge_source_isolation_test::same_operation_id_mcp_first`；AC-10=`agent_bridge_source_isolation_test::harness_digest_unaffected_harness_first`；AC-11=`agent_bridge_source_isolation_test::harness_digest_unaffected_mcp_first`；AC-12=`agent_bridge_source_isolation_test::mcp_cannot_use_harness_session`；AC-13=`agent_bridge_source_isolation_test::harness_cannot_use_mcp_session`；AC-14=`agent_bridge_source_isolation_test::workspace_owner_uses_context_source`；AC-15=`agent_bridge_source_isolation_test::mcp_status_reports_zero_events`；AC-16=`agent_bridge_source_isolation_test::mcp_context_rejects_ingress`；AC-17=Verification 第 17 项的命令/脚本。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib internal::ai::agent_bridge::ingress::tests::harness_table_names_unchanged -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_bridge_source_isolation_test harness_context_excludes_mcp -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_bridge_source_isolation_test harness_checkpoint_list_excludes_mcp -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_bridge_source_isolation_test harness_status_unaffected_by_mcp_rows -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_bridge_source_isolation_test harness_history_unaffected_by_mcp_rows -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_bridge_source_isolation_test harness_checkpoint_show_cannot_see_mcp -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_bridge_source_isolation_test harness_checkpoint_diff_cannot_see_mcp -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_bridge_source_isolation_test same_operation_id_harness_first -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_bridge_source_isolation_test same_operation_id_mcp_first -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_bridge_source_isolation_test harness_digest_unaffected_harness_first -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_bridge_source_isolation_test harness_digest_unaffected_mcp_first -- --exact`（new）
- [ ] AC-12：`source .env.test && cargo test --test agent_bridge_source_isolation_test mcp_cannot_use_harness_session -- --exact`（new）
- [ ] AC-13：`source .env.test && cargo test --test agent_bridge_source_isolation_test harness_cannot_use_mcp_session -- --exact`（new）
- [ ] AC-14：`source .env.test && cargo test --test agent_bridge_source_isolation_test workspace_owner_uses_context_source -- --exact`（new）
- [ ] AC-15：`source .env.test && cargo test --test agent_bridge_source_isolation_test mcp_status_reports_zero_events -- --exact`（new）
- [ ] AC-16：`source .env.test && cargo test --test agent_bridge_source_isolation_test mcp_context_rejects_ingress -- --exact`（new）
- [ ] AC-17：以 `bash` 执行下列脚本，退出码 0 为通过：

  ```bash
  set -euo pipefail
  FILES="tests/agent_bridge_approval_test.rs tests/agent_bridge_checkpoint_test.rs tests/agent_bridge_ingress_test.rs tests/agent_bridge_mutation_test.rs tests/agent_bridge_read_methods_test.rs tests/agent_bridge_redaction_test.rs tests/agent_bridge_security_test.rs tests/agent_bridge_span_test.rs tests/agent_bridge_vcs_test.rs tests/agent_bridge_workspace_test.rs"
  OUT="$(libra diff -- $FILES)"
  test -n "$OUT" || { echo "ERROR: expected changes in the 10 files"; exit 2; }
  printf '%s\n' "$OUT" | awk '
    /^(\+\+\+|---) / { next }
    /^[+-]/ {
      if ($0 ~ /^\+[[:space:]]*source: libra::internal::ai::agent_bridge::ingress::BridgeSource::DeepseekHarness,$/) next
      print "FAIL: unexpected changed line: " $0; bad = 1
    }
    END { exit bad }'
  echo OK
  ```

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/development/tracing/deepseek-harness.md`：`rg -qF -- 'BridgeSource' docs/development/tracing/deepseek-harness.md`
- [ ] 测试索引：`rg -q '^\| `agent_bridge_source_isolation_test` ' tests/INDEX.md && rg -q 'name = "agent_bridge_source_isolation_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 bridge 存储层与 Cargo.toml [[test]] 行`

**Dependencies:** `MC-03`（消费 `agent_mcp_*` 表）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/agent_bridge/protocol.rs`（只新增 `SOURCE_MCP` 常量）、`src/internal/ai/agent_bridge/ingress.rs`、`src/internal/ai/agent_bridge/storage.rs`、`src/internal/ai/agent_bridge/provenance.rs`、`src/internal/ai/agent_bridge/mutations.rs`、`src/internal/ai/agent_bridge/methods.rs`、`src/internal/ai/agent_bridge/workspace.rs`、`src/command/agent/bridge.rs`、`tests/agent_bridge_source_isolation_test.rs`（新）、`tests/agent_bridge_approval_test.rs`、`tests/agent_bridge_checkpoint_test.rs`、`tests/agent_bridge_ingress_test.rs`、`tests/agent_bridge_mutation_test.rs`、`tests/agent_bridge_read_methods_test.rs`、`tests/agent_bridge_redaction_test.rs`、`tests/agent_bridge_security_test.rs`、`tests/agent_bridge_span_test.rs`、`tests/agent_bridge_vcs_test.rs`、`tests/agent_bridge_workspace_test.rs`、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/development/tracing/deepseek-harness.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/development/tracing/deepseek-harness.md`。内容：`docs/development/tracing/deepseek-harness.md` 追加 `BridgeSource` 分表说明；用户命令文档、网站页、`COMPATIBILITY.md`、`docs/error-codes.md`：N/A——Harness 命令行为与输出不变，MCP 尚无用户入口。不受影响：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** source 只来自服务端常量；表名来自固定枚举，不拼接请求输入；两个 source 读写互不可见。

**Performance budget:** Harness 查询文本不变；MCP 查询与 Harness 同复杂度。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=bridge 读写按 source 选择独立表集（Harness SQL 不变）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=17/8@EX-MC-02; VER=17/8@EX-MC-02; landing=2; prod-files=8; scope=M; deps=MC-03,DEP-MC-03; writeset=序列化于 MC-03; release=independent; split-from=N/A; exception=EX-MC-02`

### Task MC-05: `libra_diff` 只读工具（worktree/staged）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 新增只读工具 `libra_diff`：参数 `mode`（本卡只开放 `worktree`/`staged`；`checkpoint` 模式由 MC-07 开放）与可选 `paths`（仓库相对），复用 bridge `vcs::parse_diff_params` 与 MC-31 的有界 `vcs::diff_data_bounded`（在 MC-29 的读子进程内运行），结果经结果信封（脱敏；patch 超 256 KiB 截断并置 `truncated: true`）。唯一行为轴：「MCP 读取工作树/暂存区 diff」。

**Out of scope:** checkpoint 模式（由 MC-07 承接）；任意 revision（永久非目标，bridge 封闭 mode 设计）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| diff 复用点与原始 patch 输出 | `src/internal/ai/agent_bridge/vcs.rs:212-302`、`:303-313` |

**Acceptance criteria:**

- [ ] AC-1 `tools/list` 中 `libra_diff` 条目的 `annotations.readOnlyHint` 为 `true`。
- [ ] AC-2 修改一个已跟踪文件后，`mode = "worktree"` 结果的 patch 含该文件的新增行。
- [ ] AC-3 暂存一个改动后，`mode = "staged"` 结果的 patch 含该改动的新增行。
- [ ] AC-4 `mode = "bogus"` 得到工具错误 `LBR-AGENT-027`。
- [ ] AC-5 `paths = ["../x"]` 得到工具错误 `LBR-AGENT-027`。
- [ ] AC-6 改动内容含 fixture secret 时，响应全文不含该 secret 字面量。
- [ ] AC-7 改动超过 256 KiB 时结果的 `truncated` 为 `true`。
- [ ] AC-8 同一情形下完整 JSON-RPC 响应行的字节数不超过 262144。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_diff_test diff_tool_is_read_only -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_diff_test diff_worktree_shows_change -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_diff_test diff_staged_shows_change -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_diff_test diff_invalid_mode_is_tool_error -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_diff_test diff_escaping_path_is_tool_error -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_diff_test diff_redacts_secret -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_diff_test diff_marks_truncated -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_diff_test diff_result_within_cap -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_diff' docs/commands/agent.md && rg -qF -- 'truncated' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_diff' docs/commands/zh-CN/agent.md && rg -qF -- 'truncated' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_diff'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'libra_diff' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'truncated' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'libra_diff' && printf '%s' "$PAGE" | rg -qF -- 'truncated'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_diff_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_diff_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-04`（写集串行：`Cargo.toml`、`tests/INDEX.md`）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/tools/mod.rs`、`src/internal/ai/mcp_server/tools/repo.rs`、`tests/agent_mcp_diff_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：工具表新增 `libra_diff`、模式、脱敏与截断说明、`patch_omitted`/`omitted_reason`（单文件新旧字节数之和超过 1 MiB 或行数之和超过 10 000 时省略 patch）与「MCP diff 不做 rename 检测、改名显示为一删一增」（MC-31）、全局预算下的 `omitted_reason = "budget"` 与 `files_omitted`（MC-37）；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 继承 bridge 的 `--no-ext-diff`/`--no-textconv`；GC-MC-03 脱敏。

**Performance budget:** patch ≤ 256 KiB（截断）；读 deadline 30 s。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=libra_diff 只读工具（worktree/staged 模式）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=2; scope=M; deps=MC-04,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-04; release=independent; split-from=N/A; exception=N/A`

### Task MC-06: MCP 可恢复写入：写会话与 `libra_checkpoint_create`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 写会话在写入意图与 `pending` operation 的**同一事务**内维护 MC-03 的单行用量表 `agent_mcp_usage`（组数 +1、载荷字节预留 `len(request_json) + 256 KiB`，`applied` 时调整为实际字节；`failed`/`quarantined` 组保持计入），因此自第一个可写版本起所有 MCP 写入都被计数并受上限约束。**容量上限（R48 自 MC-42 并入，计数与强制必须在第一个可写版本同时生效）**：为本仓库中 MCP 写入产生的**全部**持久化数据设仓库级聚合上限（不分 actor）：以「operation 组」计（一条 MCP operation 及其意图、checkpoint、关联链接、`agent_mcp_lease` 行、该 lease 之后的 renew/release 记录，以及 MCP claim 在 workspace registry 中创建的 `workspace_record` 行），全仓库至多 10000 组，载荷字节至多 512 MiB（两项仅在 debug 且 `LIBRA_TEST=1` 时可覆盖）。**计数与强制在同一版本**：上述单行用量表由本卡维护并强制上限，第一个可写版本即有上限，不存在未受限的旧写入者。claim 另为该 lease 预留结算额度：16 次 renew 与 1 次 release，每次的请求 JSON 与结果 JSON **合计** ≤ 2 KiB（renew/release 在记录意图之前校验请求 JSON ≤ 1 KiB，否则 `LBR-AGENT-027`；二者结果 schema 固定且序列化 ≤ 1 KiB），合计 34 KiB，因此对已存在 lease 的 renew/release 不新增组、不再检查配额，达到上限时仍可结算。**接入点**：写会话入口 `src/internal/ai/mcp_server/session.rs`（所有写工具经它取得写锁与记录意图）。超出即工具错误 `LBR-AGENT-035`（`structuredContent.reason = "write_quota"`）且不写任何行。**不做自动删除**；达到上限后的清理见 DEFER-MC-10。记录载荷上限：≤ 10000 组、`request_json`/`result_json` 载荷 ≤ 512 MiB（含预留的结算额度）；SQLite 页、索引、WAL 与对象库不计入该数值（文档写明）。写入意图前按 **UTF-8 字节数**（而非字符数）校验规范化后的 `request_json` ≤ 256 KiB，超出即工具错误 `LBR-AGENT-034`（payload 超限，不写任何行；MC-03 的 CHECK 以 `length(CAST(… AS BLOB))` 作为最后防线）。所有写工具在任何写入之前校验客户端 `operation_id`：1..=128 字节、只含 `[A-Za-z0-9._:-]`（不含 actor 限定 id 的分隔符 `~`），否则工具错误 `LBR-AGENT-027`（因此由它派生的 checkpoint id 与结果有界）；写记录的容量上限见本卡下文。新增 `--allow-write`、`--actor`（写权限时必填，文档示例 WorkBuddy 用 `workbuddy`、Grok Bot 所连服务用 `grok`）与 MCP 写会话（ADR-MC-05/06；`--allow-write` 只在 Unix 上可用，因为本计划的写路径端到端门（写锁、写入期限退出、hook 拒绝）只在 Unix 上验证，非 Unix 以 `LBR-CLI-002` 拒绝，见 DEFER-MC-08）：actor 写锁（位于共享存储目录的 `mcp/actors/<actor>.write.lock`，跨 linked worktree 生效；与仓库锁 `mcp/repo.write.lock` 均经新增的 `acquire_private_lock`：私有锁目录、`O_NOFOLLOW` 打开、`fstat` 校验）、稳定 session `mcp~<actor>`（仓库级：`worktree_id` 恒为 NULL，不绑定首次使用的工作树；每次写入作用于发起进程所在的工作树）、operation 标识 `<actor>~<client operation_id>`、意图表（首次请求写入 `agent_mcp_intent`，重放一律复用其 `request_json`）、写工具串行、写入硬期限（每次写工具执行超过 120 s 时，进程记录日志后以非零退出码终止以释放全部锁，遗留的 `pending` 由下次启动收敛；测试可经 debug-only 环境变量把期限缩短）、跨 actor 的 MCP 仓库写锁（共享存储下的 `mcp/repo.write.lock`，包住 fence 校验与效果执行，阻塞等待上限 30 s，超时为工具错误 `LBR-AGENT-036`；只在 MCP 写入者之间串行化，对 CLI/Harness 的保证见 ADR-MC-06）、仅在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时存在的测试写工具 `libra_test_repo_write`（持仓库写锁、校验 `expected_head` 后创建空提交，用于验证跨 actor 互斥；可选 `stall = true` 使其在持锁后停住，用于验证写入期限）、启动时 pending 收敛框架（有收敛器的方法按收敛器判定；没有收敛器的方法置为 `quarantined`），以及 quarantine 拦截（对 `quarantined` 的 operation 重放返回工具错误 `LBR-AGENT-035`）。首个写工具 `libra_checkpoint_create`（参数：只有必填 `operation_id`；首次调用时服务端读取当前 HEAD 作为 `target_oid` 并写入意图，checkpoint id 为 `<actor>~ck~<operation_id>`），及其确定性收敛器：checkpoint 行存在时先幂等补写全部关联链接（`record_association_links` 以唯一键幂等），再记为 `applied`；行不存在则记为 `failed`。写会话没有不经写工具即可观察的行为，拆成单独一卡会产生无法验收的增量（G-02），因此「会话 + 首个写工具」是一个可恢复的行为轴。

**Out of scope:** checkpoint 读工具（由 MC-07 承接）；commit（由 MC-08 承接）；restore 工具（DEFER-MC-07）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| mutation 管线与重放语义（先写效果、后记 applied；摘要覆盖全部参数） | `src/internal/ai/agent_bridge/mutations.rs:39-56`、`:65-75`、`:77-190`、`:200-225` |
| checkpoint 行先于关联链接写入 | `mutations.rs:515-583`（`insert_checkpoint` 在 `:540-550`，`record_association_links` 在其后） |
| operation 状态读写 | `storage.rs:405-497` |
| 锁原语与共享存储路径 | `src/command/control_lock.rs:624-674`；`src/utils/util.rs:1233`（`try_get_storage_path`，工作区当前行号；基线提交中为 `:1177`） |
| MCP 表集与 source 隔离（MC-03/04 交付） | `agent_mcp_*`、`agent_mcp_intent`、`BridgeSource::Mcp` |

**Acceptance criteria:**

- [ ] AC-1 不带 `--allow-write` 时 `tools/list` 不含 `libra_checkpoint_create`。
- [ ] AC-2 不带 `--allow-write` 时调用 `libra_checkpoint_create` 得到协议错误 `-32602`。
- [ ] AC-3 同一情形下 `agent_mcp_operation` 行数为 0。
- [ ] AC-4 带 `--allow-write` 时 `libra_checkpoint_create` 的 `annotations` 等于 `{"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true}`。
- [ ] AC-5 首次写入后存在 `bridge_session_id = "mcp~<actor>"` 的 `agent_mcp_session` 行。
- [ ] AC-6 该 session 行的 `agent_id` 等于 `--actor` 值。
- [ ] AC-7 该 session 行的 `worktree_id` 为 NULL。
- [ ] AC-8 同一 actor 先在工作树 A、退出后再在 linked 工作树 B 调用 `libra_checkpoint_create`，B 中创建的 checkpoint `target_oid` 等于 B 的 HEAD。
- [ ] AC-9 在工作树 B 中以 A 中用过的 `operation_id` 重放，返回 A 中创建的 `checkpoint_id`。
- [ ] AC-10 创建的 checkpoint 行 `target_oid` 等于调用时的 HEAD。
- [ ] AC-11 unborn HEAD 仓库中调用得到工具错误 `LBR-AGENT-035`。
- [ ] AC-12 缺少 `operation_id` 得到工具错误 `LBR-AGENT-027`。
- [ ] AC-13 `operation_id` 含 `~`（`[A-Za-z0-9._:-]` 以外的字符）时得到工具错误 `LBR-AGENT-027`。
- [ ] AC-14 `operation_id` 为 `op:1` 的 `libra_checkpoint_create` 调用成功（`:` 被接受）。
- [ ] AC-15 同一进程内以同一 `operation_id` 重放，返回的 `checkpoint_id` 与首次相同。
- [ ] AC-16 同一重放后 `agent_mcp_checkpoint` 行数不变。
- [ ] AC-17 进程退出并重启后，以同一 `operation_id` 重放返回的 `checkpoint_id` 与首次相同。
- [ ] AC-18 HEAD 前进一个提交后，以同一 `operation_id` 重放返回的 `checkpoint_id` 与首次相同。
- [ ] AC-19 同一 `operation_id` 携带 schema 未声明的参数得到工具错误 `LBR-AGENT-027`。
- [ ] AC-20 同一拒绝后该 `operation_id` 的意图行 `request_json` 不变。
- [ ] AC-21 两个不同 actor 以同一客户端 `operation_id` 各调用一次 `libra_checkpoint_create`，两次返回的 `checkpoint_id` 不同（意图与 operation 均以 actor 限定 id 为键）。
- [ ] AC-22 同一 actor 在同一工作树已有存活的 `--allow-write` 进程时，第二个进程以用法拒绝 `LBR-CLI-002` 结束。
- [ ] AC-23 同一 actor 在另一 linked worktree 已有存活的 `--allow-write` 进程时，第二个进程以用法拒绝 `LBR-CLI-002` 结束。
- [ ] AC-24 `--actor` 不满足 `[A-Za-z0-9._-]{1,64}` 时以用法拒绝 `LBR-CLI-002` 结束。
- [ ] AC-25 两个不同 actor 的 `--allow-write` 进程以同一 `expected_head` 并发调用 `libra_test_repo_write`，恰有一个成功。
- [ ] AC-26 同一负载下另一个得到工具错误 `LBR-AGENT-038`。
- [ ] AC-27 `libra_test_repo_write` 以 `stall = true` 停住、写入期限缩短为 2 s 时，服务进程在 10 s 内退出。
- [ ] AC-28 同一进程的退出码非 0。
- [ ] AC-29 写入平台判定函数对非 Unix 输入返回「不支持」（非 Unix 上 `--allow-write` 以 `LBR-CLI-002` 拒绝，DEFER-MC-08）。
- [ ] AC-30 写入平台判定函数对 Unix 输入返回「支持」。
- [ ] AC-31 该进程结束后，同一 actor 的新 `--allow-write` 进程能取得写锁并成功执行 `libra_test_repo_write`。
- [ ] AC-32 同一新进程启动后，停住的那次写入的 operation 状态为 `quarantined`。
- [ ] AC-33 在 checkpoint 行写入后、关联链接写入前注入崩溃（`LIBRA_TEST=1` failpoint）并重启，该 operation 状态为 `applied`。
- [ ] AC-34 同一情形下该 checkpoint 的 `agent_mcp_link` 行集合等于未崩溃创建时的集合。
- [ ] AC-35 在 checkpoint 行写入前注入崩溃并重启，该 operation 状态为 `failed`。
- [ ] AC-36 同一情形下重放成功创建 checkpoint。
- [ ] AC-37 注入一条 `method = 'commit.create'` 的 `pending` 行后重启，其状态为 `quarantined`。
- [ ] AC-38 对 `quarantined` 的 operation 重放得到工具错误 `LBR-AGENT-035`。
- [ ] AC-39 `--allow-write` 未给 `--actor` 时以用法拒绝 `LBR-CLI-002` 结束。
- [ ] AC-40 共享存储目录下的 `mcp/` 被设为 0777 时，`--allow-write` 进程退出码非 0。
- [ ] AC-41 `mcp/actors/<actor>.write.lock` 预先被创建为指向另一文件的符号链接时，`--allow-write` 进程退出码非 0。
- [ ] AC-42 同一情形下该符号链接指向的文件内容不变。
- [ ] AC-43 `--actor repo --allow-write` 进程正常启动并成功完成一次 `libra_checkpoint_create`。
- [ ] AC-44 `operation_id` 长度为 129 字节的 `libra_checkpoint_create` 调用得到工具错误 `LBR-AGENT-027`。
- [ ] AC-45 两次成功的 `libra_checkpoint_create` 之后 `agent_mcp_usage.groups` 等于 2。
- [ ] AC-46 同一时刻 `agent_mcp_usage.payload_bytes` 等于这两组 `request_json` 与 `result_json` 的实际字节之和。
- [ ] AC-47 组配额覆盖为 3 时，actor `a` 写 2 次、actor `b` 写 1 次后，actor `c` 的写调用得到工具错误 `LBR-AGENT-035`。
- [ ] AC-48 同一结果的 `structuredContent.reason` 等于 `"write_quota"`。
- [ ] AC-49 同一状态下以 actor `a` 第 1 次的 `operation_id` 与参数重放，返回首次结果。
- [ ] AC-50 同一被拒调用前后，`agent_mcp_*` 各表与 `workspace_record` 的行数都不变。
- [ ] AC-51 字节配额覆盖为「当前用量 + 本次请求字节 + 100 KiB」时，该写调用得到工具错误 `LBR-AGENT-035`（预留 256 KiB 结果上界）。
- [ ] AC-52 预置 10000 组用量时，一次写调用的配额检查所读取的行数（debug 计数器）等于 1。
- [ ] AC-53 组配额覆盖为 3 时，以 3 个不同 `operation_id` 触发失败的写调用（`libra_test_repo_write` 的 `expected_head` 不匹配）后，第 4 个写调用得到工具错误 `LBR-AGENT-035`（失败的组同样计入）。
- [ ] AC-54 规范化后字符数 < 262144、但 UTF-8 字节数 > 262144（多字节字符）的写请求得到工具错误 `LBR-AGENT-034`。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**判据规范（非计数正文）:** 门族（54 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`agent_mcp_write_session_test::checkpoint_create_hidden_without_allow_write`；AC-2=`agent_mcp_write_session_test::checkpoint_create_call_without_allow_write`；AC-3=`agent_mcp_write_session_test::no_rows_without_allow_write`；AC-4=`agent_mcp_write_session_test::checkpoint_create_annotations`；AC-5=`agent_mcp_write_session_test::write_opens_stable_actor_session`；AC-6=`agent_mcp_write_session_test::session_agent_id_is_actor`；AC-7=`agent_mcp_write_session_test::session_is_repository_scoped`；AC-8=`agent_mcp_write_session_test::sequential_worktrees_use_own_head`；AC-9=`agent_mcp_write_session_test::replay_across_worktrees_returns_original`；AC-10=`agent_mcp_write_session_test::checkpoint_targets_current_head`；AC-11=`agent_mcp_write_session_test::checkpoint_unborn_head_denied`；AC-12=`agent_mcp_write_session_test::missing_operation_id_is_tool_error`；AC-13=`agent_mcp_write_session_test::invalid_operation_id_is_tool_error`；AC-14=`agent_mcp_write_session_test::operation_id_colon_accepted`；AC-15=`agent_mcp_write_session_test::checkpoint_replay_same_id`；AC-16=`agent_mcp_write_session_test::checkpoint_replay_no_new_row`；AC-17=`agent_mcp_write_session_test::checkpoint_replay_after_restart`；AC-18=`agent_mcp_write_session_test::checkpoint_replay_after_head_moves`；AC-19=`agent_mcp_write_session_test::checkpoint_replay_with_extra_args_is_rejected`；AC-20=`agent_mcp_write_session_test::checkpoint_replay_rejection_keeps_intent`；AC-21=`agent_mcp_write_session_test::same_client_operation_id_two_actors`；AC-22=`agent_mcp_write_session_test::second_writer_same_worktree_refused`；AC-23=`agent_mcp_write_session_test::second_writer_other_worktree_refused`；AC-24=`agent_mcp_write_session_test::invalid_actor_is_usage_error`；AC-25=`agent_mcp_write_session_test::cross_actor_writes_one_succeeds`；AC-26=`agent_mcp_write_session_test::cross_actor_writes_other_is_stale`；AC-27=`agent_mcp_write_session_test::stalled_write_terminates_process`；AC-28=`agent_mcp_write_session_test::stalled_write_exit_code_nonzero`；AC-29=`--lib internal::ai::mcp_server::session::tests::writes_unsupported_off_unix`；AC-30=`--lib internal::ai::mcp_server::session::tests::writes_supported_on_unix`；AC-31=`agent_mcp_write_session_test::stalled_write_releases_locks`；AC-32=`agent_mcp_write_session_test::stalled_write_is_quarantined`；AC-33=`agent_mcp_write_session_test::crash_before_links_reconciles_applied`；AC-34=`agent_mcp_write_session_test::crash_before_links_restores_links`；AC-35=`agent_mcp_write_session_test::crash_before_insert_reconciles_failed`；AC-36=`agent_mcp_write_session_test::crash_before_insert_replay_creates`；AC-37=`agent_mcp_write_session_test::pending_commit_becomes_quarantined`；AC-38=`agent_mcp_write_session_test::quarantined_replay_is_tool_error`；AC-39=`agent_mcp_write_session_test::allow_write_requires_explicit_actor`；AC-40=`agent_mcp_write_session_test::shared_lock_dir_refused`；AC-41=`agent_mcp_write_session_test::actor_lock_symlink_refused`；AC-42=`agent_mcp_write_session_test::actor_lock_symlink_target_untouched`；AC-43=`agent_mcp_write_session_test::actor_named_repo_does_not_collide`；AC-44=`agent_mcp_write_session_test::operation_id_too_long_is_027`；AC-45=`agent_mcp_write_session_test::usage_counts_groups_from_first_write`；AC-46=`agent_mcp_write_session_test::usage_counts_actual_payload_bytes`；AC-47=`agent_mcp_write_quota_test::aggregate_quota_spans_actors`；AC-48=`agent_mcp_write_quota_test::quota_reason_is_write_quota`；AC-49=`agent_mcp_write_quota_test::replay_at_quota_returns_original`；AC-50=`agent_mcp_write_quota_test::refused_write_adds_no_rows`；AC-51=`agent_mcp_write_quota_test::byte_quota_reserves_result_bound`；AC-52=`agent_mcp_write_quota_test::quota_check_reads_single_counter_row`；AC-53=`agent_mcp_write_quota_test::failed_groups_count_toward_quota`；AC-54=`agent_mcp_write_session_test::multibyte_request_over_byte_limit_is_034`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_create_hidden_without_allow_write -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_create_call_without_allow_write -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_write_session_test no_rows_without_allow_write -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_create_annotations -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_write_session_test write_opens_stable_actor_session -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_write_session_test session_agent_id_is_actor -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_write_session_test session_is_repository_scoped -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_write_session_test sequential_worktrees_use_own_head -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_mcp_write_session_test replay_across_worktrees_returns_original -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_targets_current_head -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_unborn_head_denied -- --exact`（new）
- [ ] AC-12：`source .env.test && cargo test --test agent_mcp_write_session_test missing_operation_id_is_tool_error -- --exact`（new）
- [ ] AC-13：`source .env.test && cargo test --test agent_mcp_write_session_test invalid_operation_id_is_tool_error -- --exact`（new）
- [ ] AC-14：`source .env.test && cargo test --test agent_mcp_write_session_test operation_id_colon_accepted -- --exact`（new）
- [ ] AC-15：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_replay_same_id -- --exact`（new）
- [ ] AC-16：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_replay_no_new_row -- --exact`（new）
- [ ] AC-17：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_replay_after_restart -- --exact`（new）
- [ ] AC-18：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_replay_after_head_moves -- --exact`（new）
- [ ] AC-19：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_replay_with_extra_args_is_rejected -- --exact`（new）
- [ ] AC-20：`source .env.test && cargo test --test agent_mcp_write_session_test checkpoint_replay_rejection_keeps_intent -- --exact`（new）
- [ ] AC-21：`source .env.test && cargo test --test agent_mcp_write_session_test same_client_operation_id_two_actors -- --exact`（new）
- [ ] AC-22：`source .env.test && cargo test --test agent_mcp_write_session_test second_writer_same_worktree_refused -- --exact`（new）
- [ ] AC-23：`source .env.test && cargo test --test agent_mcp_write_session_test second_writer_other_worktree_refused -- --exact`（new）
- [ ] AC-24：`source .env.test && cargo test --test agent_mcp_write_session_test invalid_actor_is_usage_error -- --exact`（new）
- [ ] AC-25：`source .env.test && cargo test --test agent_mcp_write_session_test cross_actor_writes_one_succeeds -- --exact`（new）
- [ ] AC-26：`source .env.test && cargo test --test agent_mcp_write_session_test cross_actor_writes_other_is_stale -- --exact`（new）
- [ ] AC-27：`source .env.test && cargo test --test agent_mcp_write_session_test stalled_write_terminates_process -- --exact`（new）
- [ ] AC-28：`source .env.test && cargo test --test agent_mcp_write_session_test stalled_write_exit_code_nonzero -- --exact`（new）
- [ ] AC-29：`source .env.test && cargo test --lib internal::ai::mcp_server::session::tests::writes_unsupported_off_unix -- --exact`（new）
- [ ] AC-30：`source .env.test && cargo test --lib internal::ai::mcp_server::session::tests::writes_supported_on_unix -- --exact`（new）
- [ ] AC-31：`source .env.test && cargo test --test agent_mcp_write_session_test stalled_write_releases_locks -- --exact`（new）
- [ ] AC-32：`source .env.test && cargo test --test agent_mcp_write_session_test stalled_write_is_quarantined -- --exact`（new）
- [ ] AC-33：`source .env.test && cargo test --test agent_mcp_write_session_test crash_before_links_reconciles_applied -- --exact`（new）
- [ ] AC-34：`source .env.test && cargo test --test agent_mcp_write_session_test crash_before_links_restores_links -- --exact`（new）
- [ ] AC-35：`source .env.test && cargo test --test agent_mcp_write_session_test crash_before_insert_reconciles_failed -- --exact`（new）
- [ ] AC-36：`source .env.test && cargo test --test agent_mcp_write_session_test crash_before_insert_replay_creates -- --exact`（new）
- [ ] AC-37：`source .env.test && cargo test --test agent_mcp_write_session_test pending_commit_becomes_quarantined -- --exact`（new）
- [ ] AC-38：`source .env.test && cargo test --test agent_mcp_write_session_test quarantined_replay_is_tool_error -- --exact`（new）
- [ ] AC-39：`source .env.test && cargo test --test agent_mcp_write_session_test allow_write_requires_explicit_actor -- --exact`（new）
- [ ] AC-40：`source .env.test && cargo test --test agent_mcp_write_session_test shared_lock_dir_refused -- --exact`（new）
- [ ] AC-41：`source .env.test && cargo test --test agent_mcp_write_session_test actor_lock_symlink_refused -- --exact`（new）
- [ ] AC-42：`source .env.test && cargo test --test agent_mcp_write_session_test actor_lock_symlink_target_untouched -- --exact`（new）
- [ ] AC-43：`source .env.test && cargo test --test agent_mcp_write_session_test actor_named_repo_does_not_collide -- --exact`（new）
- [ ] AC-44：`source .env.test && cargo test --test agent_mcp_write_session_test operation_id_too_long_is_027 -- --exact`（new）
- [ ] AC-45：`source .env.test && cargo test --test agent_mcp_write_session_test usage_counts_groups_from_first_write -- --exact`（new）
- [ ] AC-46：`source .env.test && cargo test --test agent_mcp_write_session_test usage_counts_actual_payload_bytes -- --exact`（new）
- [ ] AC-47：`source .env.test && cargo test --test agent_mcp_write_quota_test aggregate_quota_spans_actors -- --exact`（new）
- [ ] AC-48：`source .env.test && cargo test --test agent_mcp_write_quota_test quota_reason_is_write_quota -- --exact`（new）
- [ ] AC-49：`source .env.test && cargo test --test agent_mcp_write_quota_test replay_at_quota_returns_original -- --exact`（new）
- [ ] AC-50：`source .env.test && cargo test --test agent_mcp_write_quota_test refused_write_adds_no_rows -- --exact`（new）
- [ ] AC-51：`source .env.test && cargo test --test agent_mcp_write_quota_test byte_quota_reserves_result_bound -- --exact`（new）
- [ ] AC-52：`source .env.test && cargo test --test agent_mcp_write_quota_test quota_check_reads_single_counter_row -- --exact`（new）
- [ ] AC-53：`source .env.test && cargo test --test agent_mcp_write_quota_test failed_groups_count_toward_quota -- --exact`（new）
- [ ] AC-54：`source .env.test && cargo test --test agent_mcp_write_session_test multibyte_request_over_byte_limit_is_034 -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- '--allow-write' docs/commands/agent.md && rg -qF -- 'write_quota' docs/commands/agent.md && rg -qF -- '10000' docs/commands/agent.md && rg -qF -- '512 MiB' docs/commands/agent.md && rg -qF -- '--actor' docs/commands/agent.md && rg -qF -- 'operation_id' docs/commands/agent.md && rg -qF -- 'quarantined' docs/commands/agent.md && rg -qF -- 'LBR-AGENT-035' docs/commands/agent.md && rg -qF -- 'libra_checkpoint_create' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- '--allow-write' docs/commands/zh-CN/agent.md && rg -qF -- 'write_quota' docs/commands/zh-CN/agent.md && rg -qF -- '10000' docs/commands/zh-CN/agent.md && rg -qF -- '512 MiB' docs/commands/zh-CN/agent.md && rg -qF -- '--actor' docs/commands/zh-CN/agent.md && rg -qF -- 'operation_id' docs/commands/zh-CN/agent.md && rg -qF -- 'quarantined' docs/commands/zh-CN/agent.md && rg -qF -- 'LBR-AGENT-035' docs/commands/zh-CN/agent.md && rg -qF -- 'libra_checkpoint_create' docs/commands/zh-CN/agent.md`
- [ ] `docs/development/commands/agent.md`：`rg -qF -- 'agent_mcp_intent' docs/development/commands/agent.md && rg -qF -- 'repo.write.lock' docs/development/commands/agent.md && rg -qF -- 'quarantined' docs/development/commands/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- '--allow-write'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- '--allow-write' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'write_quota' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '10000' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '512 MiB' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '--actor' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'operation_id' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'quarantined' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'LBR-AGENT-035' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'libra_checkpoint_create' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- '--allow-write' && printf '%s' "$PAGE" | rg -qF -- '--actor' && printf '%s' "$PAGE" | rg -qF -- 'operation_id' && printf '%s' "$PAGE" | rg -qF -- 'quarantined' && printf '%s' "$PAGE" | rg -qF -- 'LBR-AGENT-035' && printf '%s' "$PAGE" | rg -qF -- 'libra_checkpoint_create' && printf '%s' "$PAGE" | rg -qF -- 'write_quota' && printf '%s' "$PAGE" | rg -qF -- '10000' && printf '%s' "$PAGE" | rg -qF -- '512 MiB'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_write_session_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_write_session_test"' Cargo.toml`
- [ ] `docs/error-codes.md`：`rg -qF -- 'write_quota' docs/error-codes.md`
- [ ] 测试索引：`rg -q '^\| `agent_mcp_write_quota_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_write_quota_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-05`（写集串行：`tools/mod.rs`、agent 文档）；`DEP-MC-03`。本卡消费 MC-01 的协议核心与 MC-03/MC-04 的表集与 `BridgeSource::Mcp`（均已在 MC-05 之前发布）。；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `tests/agent_mcp_write_quota_test.rs`（新）、`docs/error-codes.md`（`LBR-AGENT-035` 补充 `write_quota`/`renew_limit`）、`src/internal/ai/mcp_server/lock.rs`（新，`acquire_private_lock`）、`src/command/agent/mcp.rs`、`src/internal/ai/mcp_server/session.rs`（新）、`src/internal/ai/mcp_server/tools/mod.rs`、`src/internal/ai/mcp_server/tools/checkpoint.rs`（新）、`src/internal/ai/mcp_server/stdio.rs`、`tests/agent_mcp_write_session_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/error-codes.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：「MCP server」小节写明仓库级写记录上限（10000 组、记录载荷 512 MiB，全部 actor 合计，失败的写同样计入，不含 SQLite 页/索引/WAL）、超限 `LBR-AGENT-035`（`write_quota`）、不自动删除（清理见 DEFER-MC-10）；`docs/error-codes.md` 的 `LBR-AGENT-035` 行补充 `write_quota`；`--allow-write`、`--actor`、必填 `operation_id` 与重放语义（重放复用首次请求）、actor 锁（跨 linked worktree）、`quarantined` 的含义与处理（换新 `operation_id`）、`libra_checkpoint_create` 与 EXAMPLES `libra agent mcp --stdio --allow-write --actor workbuddy`；开发文档写写会话、意图表、收敛框架（ADR-MC-05/06）；`COMPATIBILITY.md` 第 252 行追加一句。不受影响：无（`docs/error-codes.md` 列为受影响：`LBR-AGENT-035` 补充 `write_quota`，未新增错误码）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 审批只来自启动参数；`inputSchema` 拒绝 `approval`/`actor`/`session_id` 等字段（多余字段 → 工具错误 `LBR-AGENT-027`）；failpoint 只在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时存在。

**Performance budget:** 写工具串行；每次写调用一次 session 幂等打开与一次意图读取；启动收敛为 O(本 actor 的 pending 行数)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP 可恢复写入（写会话 + 首个写工具 libra_checkpoint_create）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=54/8@EX-MC-03; VER=54/8@EX-MC-03; landing=4; prod-files=6; scope=M; deps=MC-05,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-05; release=independent; split-from=N/A; exception=EX-MC-03`

### Task MC-07: MCP checkpoint 读取：list/show 与 diff checkpoint 模式

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 新增只读工具 `libra_checkpoint_list`、`libra_checkpoint_show`，并为 `libra_diff` 开放 `mode = "checkpoint"`（参数 `checkpoint_id`，比较该 checkpoint 固定的提交与工作树）。三处查找都先确认 checkpoint 行的 `bridge_session_id` 等于服务端派生的 `mcp~<actor>`，否则工具错误 `LBR-AGENT-032`。唯一行为轴：「MCP 读取本 actor 的 checkpoint」。

**Out of scope:** restore 工具（DEFER-MC-07）；其它 actor 或 Harness checkpoint 的读取（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge checkpoint 读方法只按仓库查找，缺失返回 `LBR-AGENT-032` | `src/internal/ai/agent_bridge/methods.rs:201-245`、`:542-560` |
| diff checkpoint 模式解析 | `methods.rs:247-315` |

**Acceptance criteria:**

- [ ] AC-1 `libra_checkpoint_list` 返回的 id 集合等于本 actor 已创建的 checkpoint id 集合。
- [ ] AC-2 另一 actor 创建 checkpoint 后，`libra_checkpoint_list` 结果与其 id 集合交集为空（Harness 隔离由 MC-04 的门覆盖）。
- [ ] AC-3 `libra_checkpoint_show` 返回的 `target_oid` 等于创建时的 HEAD。
- [ ] AC-4 `libra_checkpoint_show` 以另一 actor 的 checkpoint id 调用得到工具错误 `LBR-AGENT-032`。
- [ ] AC-5 `libra_checkpoint_list` 的 `annotations.readOnlyHint` 为 `true`。
- [ ] AC-6 `libra_checkpoint_show` 的 `annotations.readOnlyHint` 为 `true`。
- [ ] AC-7 在 checkpoint 之后修改已跟踪文件，`mode = "checkpoint"` 结果的 patch 含该新增行。
- [ ] AC-8 `mode = "checkpoint"` 以另一 actor 的 checkpoint id 调用得到工具错误 `LBR-AGENT-032`。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_checkpoint_read_test list_returns_actor_checkpoints -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_checkpoint_read_test list_excludes_other_actor_checkpoints -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_checkpoint_read_test show_returns_target -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_checkpoint_read_test show_foreign_actor_refused -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_checkpoint_read_test list_is_read_only -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_checkpoint_read_test show_is_read_only -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_checkpoint_read_test diff_checkpoint_mode_shows_change -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_checkpoint_read_test diff_checkpoint_foreign_actor_refused -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_checkpoint_list' docs/commands/agent.md && rg -qF -- 'libra_checkpoint_show' docs/commands/agent.md && rg -qF -- 'checkpoint' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_checkpoint_list' docs/commands/zh-CN/agent.md && rg -qF -- 'libra_checkpoint_show' docs/commands/zh-CN/agent.md && rg -qF -- 'checkpoint' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_checkpoint_list'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'libra_checkpoint_list' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'libra_checkpoint_show' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'checkpoint' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'libra_checkpoint_list' && printf '%s' "$PAGE" | rg -qF -- 'libra_checkpoint_show' && printf '%s' "$PAGE" | rg -qF -- 'checkpoint'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_checkpoint_read_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_checkpoint_read_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-06`（消费 `libra_checkpoint_create` 与写会话）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/tools/mod.rs`、`src/internal/ai/mcp_server/tools/checkpoint.rs`、`src/internal/ai/mcp_server/tools/repo.rs`、`tests/agent_mcp_checkpoint_read_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：checkpoint 读工具、actor 作用域与 diff `checkpoint` 模式；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 只读本 actor 的 MCP checkpoint。

**Performance budget:** 分页沿用 bridge 上限（100）；读 deadline 30 s。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP 读取本 actor 的 checkpoint; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=3; scope=M; deps=MC-06,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-06; release=independent; split-from=N/A; exception=N/A`

### Task MC-08: `libra_commit` 写工具

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 新增写工具 `libra_commit`（需 `--allow-write`）：参数 `message`、`expected_head`（提交 OID，或 `null` 表示未出生分支，映射为 MC-28 的 `ExpectedParent::Unborn`）、`expected_head_ref`（分支名或 `null` 表示 detached）、`expected_tree`（三者取自 `libra_status` 的 `head`/`head_ref`/`index_tree`）、`operation_id`（均必填）与可选 `allow_empty`；经写会话与 bridge mutation 管线执行，注入运维审批（ADR-MC-06），并修改 bridge mutation 分发器：上下文 source 为 `Mcp` 时 `commit.create` 先以 MC-23 的 `runnable_commit_hooks()` 预检：存在任一会执行的提交 hook（pre-commit、prepare-commit-msg、commit-msg、post-commit）即返回工具错误 `LBR-AGENT-035`（`structuredContent.reason = "hooks_present"`，不写仓库）；否则在 MC-23 的 `HookPolicy::Refuse` 作用域内调用 MC-18 的 `vcs::commit_create_fenced`（以 `expected_head` 与 `expected_head_ref` 为 fence 入口与 HEAD 更新事务内的 CAS 前提，`expected_tree` 为写提交对象前的暂存树前提），因此 MCP 提交在任何情况下都不启动 hook 进程（预检之后才出现的 `pre-commit`/`prepare-commit-msg`/`commit-msg` 由作用域在执行点拒绝，提交不发生；预检之后才出现的 `post-commit` 在提交效果之后不被执行，**不静默**：`run_commit` 不再丢弃 `run_advisory_repo_hook` 的返回值，而是在内部结果 `CommitOutcome` 中携带 `post_commit: AdvisoryOutcome`，`vcs::commit_create_fenced` 把它放入返回值（CLI 与 Harness 不读取该字段，输出不变），MCP 据此在结果中给出 `structuredContent.post_commit_skipped = true`，文档写明该竞态下提交已生效而 post-commit 未运行），写入期限到期退出时不存在 hook 派生进程，Harness 上下文仍调用原 `vcs::commit_create`（行为不变），因此 CLI/Harness 在 fence 与效果之间移动 HEAD 时，MCP 提交同样以 `LBR-AGENT-038` 拒绝。`commit.create` 没有收敛器：效果后、回执前崩溃遗留的 pending 在重启后成为 `quarantined`，重放被拒（本卡以 failpoint 端到端验证）。唯一行为轴：「MCP 提交当前 index」。

**Out of scope:** pathspec/`-a`/amend/author 覆盖（永久非目标，bridge 设计）；精确收敛（DEFER-MC-06）；fence 与 ref 写入之间的竞态门（MC-18 已交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| commit 语义与参数 | `src/internal/ai/agent_bridge/vcs.rs:404-458`、`:459-499` |
| 审批归类 | `src/internal/ai/agent_bridge/authorization.rs:32-43`、`:86-100` |
| 先写效果后记 applied | `src/internal/ai/agent_bridge/mutations.rs:200-225` |
| 分发器当前调用未加 fence 的 `vcs::commit_create` | `src/internal/ai/agent_bridge/mutations.rs:288` |

**Acceptance criteria:**

- [ ] AC-1 不带 `--allow-write` 时 `tools/list` 不含 `libra_commit`。
- [ ] AC-2 `libra_commit` 的 `annotations` 等于 `{"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true}`。
- [ ] AC-3 暂存改动后调用，新 HEAD 的父提交等于 `expected_head`。
- [ ] AC-4 同一 `operation_id`、同参数重放，返回的 commit id 与首次相同。
- [ ] AC-5 同一 `operation_id`、不同 `message` 得到工具错误 `LBR-AGENT-033`。
- [ ] AC-6 读取 `libra_status` 的 `index_tree` 之后另一进程执行 `libra add` 新文件，再以该 `index_tree` 调用 `libra_commit` 得到工具错误 `LBR-AGENT-038`。
- [ ] AC-7 读取 `libra_status`（`head_ref = main`）之后另一进程执行 `libra switch -c other`（同一 OID），再以该快照调用 `libra_commit` 得到工具错误 `LBR-AGENT-038`。
- [ ] AC-8 同一情形下 `other` 分支的 ref 不变。
- [ ] AC-9 未出生仓库中暂存 `a.txt` 后以 `expected_head = null` 调用成功，新提交没有父提交。
- [ ] AC-10 竞争首个提交：`expected_head = null` 的调用停在 MC-18 的 `LIBRA_TEST=1` 暂停点时，另一进程执行 `libra commit -m first`，放行后 MCP 调用得到工具错误 `LBR-AGENT-038`。
- [ ] AC-11 同一竞争后分支 ref 等于另一进程创建的首个提交。
- [ ] AC-12 同一路径换内容：取得 staged diff（MC-27）后把 `a.txt` 改为其它内容并重新 `libra add`，再以该 diff 的 `index_tree` 作 `expected_tree` 调用 `libra_commit`，得到工具错误 `LBR-AGENT-038`。
- [ ] AC-13 端到端竞态：MCP `libra_commit` 停在 MC-18 的 `LIBRA_TEST=1` 暂停点时，另一进程执行 `libra commit --allow-empty -m race`，放行后 MCP 调用得到工具错误 `LBR-AGENT-038`（ref 结果由 MC-18 AC-7 在写入层判定）。
- [ ] AC-14 operation 行的 `params_digest` 等于「意图中记录的请求参数 + 注入审批 `{decision:"approved",approver:"operator:--allow-write:<actor>"}` + session id」的摘要。
- [ ] AC-15 在提交写入后、回执前注入崩溃（`LIBRA_TEST=1` failpoint）并重启，以同一 `operation_id` 重放得到工具错误 `LBR-AGENT-035`。
- [ ] AC-16 仓库存在自定义 `pre-commit`（写标记文件）时调用得到工具错误 `LBR-AGENT-035`。
- [ ] AC-17 同一调用后标记文件不存在（hook 未执行）。
- [ ] AC-18 同一调用后 HEAD 不变。
- [ ] AC-19 仓库只存在自定义 `post-commit` 时调用同样得到工具错误 `LBR-AGENT-035`。
- [ ] AC-20 仓库只存在自定义 `commit-msg` 时调用同样得到工具错误 `LBR-AGENT-035`。
- [ ] AC-21 仓库只存在 `libra init` 内置的空 `pre-commit` 时调用成功（新 HEAD 的父提交等于 `expected_head`）。
- [ ] AC-22 竞态门：`libra_commit` 在 hook 预检之后的 `LIBRA_TEST=1` 暂停点停住时安装自定义 `post-commit`，放行后结果的 `structuredContent.post_commit_skipped` 为 `true`（调用成功是取得该字段的前提）。
- [ ] AC-23 同一竞态后该 `post-commit` 的标记文件不存在。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**判据规范（非计数正文）:** 门族（23 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`agent_mcp_commit_test::commit_hidden_without_allow_write`；AC-2=`agent_mcp_commit_test::commit_annotations`；AC-3=`agent_mcp_commit_test::commit_parent_is_expected_head`；AC-4=`agent_mcp_commit_test::commit_replay_returns_original`；AC-5=`agent_mcp_commit_test::commit_digest_conflict`；AC-6=`agent_mcp_commit_test::commit_refuses_changed_index`；AC-7=`agent_mcp_commit_test::commit_refuses_branch_switched_after_status`；AC-8=`agent_mcp_commit_test::commit_branch_switch_keeps_other_ref`；AC-9=`agent_mcp_commit_test::commit_first_commit_on_unborn_branch`；AC-10=`agent_mcp_commit_test::commit_unborn_race_is_fenced`；AC-11=`agent_mcp_commit_test::commit_unborn_race_keeps_other_ref`；AC-12=`agent_mcp_commit_test::commit_rejects_restaged_same_path_after_review`；AC-13=`agent_mcp_commit_test::commit_race_with_cli_is_fenced`；AC-14=`agent_mcp_commit_test::commit_records_operator_approval`；AC-15=`agent_mcp_commit_test::commit_crash_after_effect_replay_quarantined`；AC-16=`agent_mcp_commit_test::commit_refused_when_pre_commit_hook_present`；AC-17=`agent_mcp_commit_test::commit_refused_hook_not_executed`；AC-18=`agent_mcp_commit_test::commit_refused_hook_head_unchanged`；AC-19=`agent_mcp_commit_test::commit_refused_when_post_commit_hook_present`；AC-20=`agent_mcp_commit_test::commit_refused_when_commit_msg_hook_present`；AC-21=`agent_mcp_commit_test::commit_allowed_with_shipped_noop_pre_commit`；AC-22=`agent_mcp_commit_test::late_post_commit_reported_skipped`；AC-23=`agent_mcp_commit_test::late_post_commit_not_executed`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_commit_test commit_hidden_without_allow_write -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_commit_test commit_annotations -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_commit_test commit_parent_is_expected_head -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_commit_test commit_replay_returns_original -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_commit_test commit_digest_conflict -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_commit_test commit_refuses_changed_index -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_commit_test commit_refuses_branch_switched_after_status -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_commit_test commit_branch_switch_keeps_other_ref -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_mcp_commit_test commit_first_commit_on_unborn_branch -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_mcp_commit_test commit_unborn_race_is_fenced -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_mcp_commit_test commit_unborn_race_keeps_other_ref -- --exact`（new）
- [ ] AC-12：`source .env.test && cargo test --test agent_mcp_commit_test commit_rejects_restaged_same_path_after_review -- --exact`（new）
- [ ] AC-13：`source .env.test && cargo test --test agent_mcp_commit_test commit_race_with_cli_is_fenced -- --exact`（new）
- [ ] AC-14：`source .env.test && cargo test --test agent_mcp_commit_test commit_records_operator_approval -- --exact`（new）
- [ ] AC-15：`source .env.test && cargo test --test agent_mcp_commit_test commit_crash_after_effect_replay_quarantined -- --exact`（new）
- [ ] AC-16：`source .env.test && cargo test --test agent_mcp_commit_test commit_refused_when_pre_commit_hook_present -- --exact`（new）
- [ ] AC-17：`source .env.test && cargo test --test agent_mcp_commit_test commit_refused_hook_not_executed -- --exact`（new）
- [ ] AC-18：`source .env.test && cargo test --test agent_mcp_commit_test commit_refused_hook_head_unchanged -- --exact`（new）
- [ ] AC-19：`source .env.test && cargo test --test agent_mcp_commit_test commit_refused_when_post_commit_hook_present -- --exact`（new）
- [ ] AC-20：`source .env.test && cargo test --test agent_mcp_commit_test commit_refused_when_commit_msg_hook_present -- --exact`（new）
- [ ] AC-21：`source .env.test && cargo test --test agent_mcp_commit_test commit_allowed_with_shipped_noop_pre_commit -- --exact`（new）
- [ ] AC-22：`source .env.test && cargo test --test agent_mcp_commit_test late_post_commit_reported_skipped -- --exact`（new）
- [ ] AC-23：`source .env.test && cargo test --test agent_mcp_commit_test late_post_commit_not_executed -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_commit' docs/commands/agent.md && rg -qF -- 'expected_head' docs/commands/agent.md && rg -qF -- 'expected_head_ref' docs/commands/agent.md && rg -qF -- 'expected_tree' docs/commands/agent.md && rg -qF -- 'quarantined' docs/commands/agent.md && rg -qF -- 'hooks_present' docs/commands/agent.md && rg -qF -- 'post_commit_skipped' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_commit' docs/commands/zh-CN/agent.md && rg -qF -- 'expected_head' docs/commands/zh-CN/agent.md && rg -qF -- 'expected_head_ref' docs/commands/zh-CN/agent.md && rg -qF -- 'expected_tree' docs/commands/zh-CN/agent.md && rg -qF -- 'quarantined' docs/commands/zh-CN/agent.md && rg -qF -- 'hooks_present' docs/commands/zh-CN/agent.md && rg -qF -- 'post_commit_skipped' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_commit'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'libra_commit' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'expected_head' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'expected_head_ref' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'expected_tree' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'quarantined' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'hooks_present' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'post_commit_skipped' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'libra_commit' && printf '%s' "$PAGE" | rg -qF -- 'expected_head' && printf '%s' "$PAGE" | rg -qF -- 'expected_head_ref' && printf '%s' "$PAGE" | rg -qF -- 'expected_tree' && printf '%s' "$PAGE" | rg -qF -- 'quarantined' && printf '%s' "$PAGE" | rg -qF -- 'hooks_present' && printf '%s' "$PAGE" | rg -qF -- 'post_commit_skipped'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_commit_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_commit_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-30`（消费 staged diff 的快照身份与审核完整性标志）；`DEP-MC-03`。本卡另消费 MC-25/MC-26 的 `libra_status` 快照、MC-24 的符号引用前提、MC-43 的暂存树前提与 MC-28 的未出生前提（均已先发布）。本卡另消费 MC-23 的 hook 拒绝策略、MC-18 的 `vcs::commit_create_fenced` 与 MC-06 的写会话（均已先发布）。；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/tools/mod.rs`、`src/internal/ai/mcp_server/tools/commit.rs`（新）、`src/internal/ai/agent_bridge/mutations.rs`（只改 `commit.create` 分支的按 source 路由）、`src/command/commit.rs`（`run_commit` 返回 `CommitOutcome`，携带 post-commit 的 `AdvisoryOutcome`）、`src/internal/ai/agent_bridge/vcs.rs`（`commit_create_fenced` 返回值携带该结果）、`tests/agent_mcp_commit_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：`libra_commit` 参数（`expected_head`/`expected_head_ref`/`expected_tree` 取自 `libra_status` 的 `head`/`head_ref`/`index_tree`；`expected_head = null` 表示在未出生分支上创建首个提交）、审批、重放与崩溃后 `quarantined` 语义；存在提交 hook 时拒绝（`hooks_present`，MCP 不运行 hook，原因见 DEFER-MC-09），预检后才出现的 `post-commit` 未运行时以 `post_commit_skipped` 报告；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 只提交 index；审批来自启动参数；MCP 提交从不启动 hook 进程；failpoint 只在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时存在。

**Performance budget:** 单次 commit 成本同 `libra commit`；写串行。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=libra_commit 写工具（含崩溃不确定态）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=23/8@EX-MC-08; VER=23/8@EX-MC-08; landing=4; prod-files=5; scope=M; deps=MC-30,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-30; release=independent; split-from=N/A; exception=EX-MC-08`

### Task MC-10: workspace 只读工具与查询共享

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 把 `agent workspace list/show` 的查询与 `record_json` 抽为同文件内返回 `serde_json::Value` 的公开函数（CLI 输出逐字节不变），新增只读工具 `libra_workspace_list`（参数 `state`、`limit`、`cursor`，语义同 CLI）与 `libra_workspace_show`（参数 `workspace_id`）。唯一行为轴：「MCP 读取 workspace registry」。

**Out of scope:** lease 工具（由 MC-11 承接）；workspace 创建/删除（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| CLI 查询与 JSON 形状 | `src/command/agent/workspace.rs:76-96`、`:97-147`、`:148-202` |
| 既有 CLI 测试模块 | `tests/command/agent_workspace_test.rs` |

**Acceptance criteria:**

- [ ] AC-1 `libra --json agent workspace list` 对固定 fixture 的输出等于先于重构写入的内联期望值。
- [ ] AC-2 `libra --json agent workspace show <id>` 对固定 fixture 的输出等于先于重构写入的内联期望值。
- [ ] AC-3 `libra_workspace_list` 的 `structuredContent` 等于同参数 `libra --json agent workspace list` 的 `data`。
- [ ] AC-4 以 `libra_workspace_list` 返回的 `next_cursor` 翻页，结果等于 CLI `--cursor` 的下一页。
- [ ] AC-5 `libra_workspace_show` 对不存在的 id 得到工具错误，其 `stable_code` 等于同输入下 CLI `--json` 错误的 stable code。
- [ ] AC-6 `libra_workspace_list` 的 `annotations.readOnlyHint` 为 `true`。
- [ ] AC-7 `libra_workspace_show` 的 `annotations.readOnlyHint` 为 `true`。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test command_test command::agent_workspace_test::workspace_list_json_byte_stable -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test command_test command::agent_workspace_test::workspace_show_json_byte_stable -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_workspace_read_test workspace_list_matches_cli -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_workspace_read_test workspace_list_cursor_paging -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_workspace_read_test workspace_show_missing_matches_cli_error -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_workspace_read_test workspace_list_is_read_only -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_workspace_read_test workspace_show_is_read_only -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_workspace_list' docs/commands/agent.md && rg -qF -- 'libra_workspace_show' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_workspace_list' docs/commands/zh-CN/agent.md && rg -qF -- 'libra_workspace_show' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_workspace_list'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'libra_workspace_list' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'libra_workspace_show' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'libra_workspace_list' && printf '%s' "$PAGE" | rg -qF -- 'libra_workspace_show'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_workspace_read_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_workspace_read_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: CLI 与 MCP 共享的 workspace 查询单一事实源（GC-02）及 Cargo.toml [[test]] 行`

**Dependencies:** `MC-08`（写集串行：`Cargo.toml`、`tests/INDEX.md`、agent 文档）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/command/agent/workspace.rs`、`src/internal/ai/mcp_server/tools/mod.rs`、`src/internal/ai/mcp_server/tools/workspace.rs`（新）、`tests/command/agent_workspace_test.rs`、`tests/agent_mcp_workspace_read_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：workspace 读工具；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 只返回 CLI `--json` 已公开字段，经结果信封。

**Performance budget:** keyset 分页，上限沿用 CLI。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=workspace 只读工具与查询共享; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=7/8; VER=7/8; landing=2; prod-files=3; scope=M; deps=MC-08,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-08; release=independent; split-from=N/A; exception=N/A`

### Task MC-11: 当前工作树的 workspace claim 工具（事务绑定、可重放）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 新增写工具 `libra_workspace_claim`（参数：必填 `operation_id`、可选 `lease_ttl_ms`，范围 1000..=3600000，缺省 600000（10 分钟）；需 `--allow-write`）。路径与 lease 身份由服务端派生为**当前仓库工作树**（canonical 根目录 + MC-40 的 `mcp_lease_worktree_id`：linked worktree 用其 worktree id，主工作树用保留身份 `main:<repository_id 的 SHA-256 十六进制>`），以 linked 方式取得 lease，永不创建 TaskCopy；owner 为 `mcp:mcp~<actor>`。claim 在同一事务内按 MC-06 的规则为该 lease 另行预留结算额度（16 次 renew + 1 次 release，34 KiB 计入 `agent_mcp_usage.payload_bytes`；每次 renew/release 请求与结果合计 ≤ 2 KiB），因此之后的 renew/release 不受配额阻塞。claim 在同一事务内另写 `agent_mcp_lease(workspace_id, actor, claim_operation_id, lease_ttl_ms)`，结果 JSON 写入 `agent_mcp_intent.result_json`（MC-03），因此之后的 renew/release 不会改变重放所返回的首次结果。每次 lease 调用在**一个 SQLite 事务**内完成：先写意图与 `pending` operation，再以同一事务连接调用 `src/internal/workspace.rs` 中既有的 `WorkspaceStore::acquire_with_conn`/`renew_with_conn`/`release_with_conn`（`:730`、`:877`、`:959`，已接受任意 `ConnectionTrait`，无需改动 store 与 bridge），并在同一事务内把 operation 记为 `applied`、写入结果；提交前崩溃则整个事务回滚——意图、operation 与 lease 效果都不留下任何行，因此不存在「效果已发生而 operation 仍 pending」的窗口；客户端以同一 `operation_id` 重试时找不到记录，即按该次请求作为新请求执行。提交成功而响应丢失时，记录已存在，重放返回已记录的结果。同一 `operation_id`、同参数的重放返回首次记录的结果，参数不同 → `LBR-AGENT-033`。lease 工具的意图请求另存服务端派生的有界可信工作树身份（MC-40 的 lease 身份 ≤ 128 字节，以及 canonical 根目录的 SHA-256 十六进制摘要，而非路径本身，因此请求大小与路径长度无关）；返回重放结果之前先比较该身份与本次调用所在工作树（同样按 lease 身份与根目录摘要比较），不等即工具错误 `LBR-AGENT-032`（不返回已保存的结果，也不执行），因此在工作树 B 中以工作树 A 的 `operation_id` 重放不会得到 A 的成功结果。renew 由 MC-38、release 由 MC-41 承接。写工具对 `--allow-write` 的可见性与未声明参数的拒绝由 MC-06 AC-1/2 与 MC-01 AC-15 的注册表级门统一覆盖。唯一行为轴：「MCP 为当前工作树取得 lease」。

**Out of scope:** renew（由 MC-38 承接）、release（由 MC-41 承接）；任意路径或其它 workspace 的 claim、TaskCopy（永久非目标，ADR-MC-09）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge claim 接受路径并可建 TaskCopy | `src/internal/ai/agent_bridge/workspace.rs:109-178` |
| renew/release 只校验 owner/fence | `src/internal/ai/agent_bridge/workspace.rs:179-268` |
| 错误码 022/032 | `src/internal/ai/agent_bridge/protocol.rs:338`、`:355-360` |

**Acceptance criteria:**

- [ ] AC-1 在主工作树中 claim 成功后，registry 中该 lease 记录的 path 等于主工作树 canonical 根目录。
- [ ] AC-2 claim 成功后该记录的 lease owner 等于 `mcp:mcp~<actor>`。
- [ ] AC-3 另一 owner 持有当前工作树 lease 时 claim 得到工具错误 `LBR-AGENT-022`。
- [ ] AC-4 以同一 `operation_id`、同参数调用两次 claim，第二次返回的 `workspace_id` 等于第一次。
- [ ] AC-5 同一重放中第二次返回的 `lease_fence` 等于第一次。
- [ ] AC-6 同一重放之后 registry 中当前工作树的存活 lease 恰为一个。
- [ ] AC-7 在 store 写入之后、事务提交之前注入崩溃（`LIBRA_TEST=1` failpoint）并重启后，registry 中不存在该 `operation_id` 产生的 lease。
- [ ] AC-8 同一崩溃并重启后，`agent_mcp_intent` 与 `agent_mcp_operation` 两表中都没有该 `<actor>~<operation_id>` 的行。
- [ ] AC-9 同一崩溃并重启后，`agent_mcp_lease` 中没有 `claim_operation_id` 等于该 `<actor>~<operation_id>` 的行。
- [ ] AC-10 随后以同一 `operation_id`、同参数重试 claim 后，registry 中当前工作树出现本 actor 的存活 lease（重试成功是其前提）。
- [ ] AC-11 该重试之后，`agent_mcp_lease` 中 `claim_operation_id` 等于该 id 的行恰为一行（其 `workspace_id` 为重试返回值）。
- [ ] AC-12 同一崩溃前已存在的、同一 actor 的另一 lease（另一 linked worktree）在重启后不变。
- [ ] AC-13 同一 actor 在工作树 A 中成功 claim 后，在 linked 工作树 B 中以相同 `operation_id` 与参数重放，得到工具错误 `LBR-AGENT-032`。
- [ ] AC-14 组配额覆盖为 3 时，claim/release 交替循环中第 4 次 claim 得到工具错误 `LBR-AGENT-035`（每次 claim 计一组）。
- [ ] AC-15 同一循环结束后，registry 中由 MCP 创建的 `workspace_record` 行数等于成功的 claim 次数 3。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**判据规范（非计数正文）:** 门族（15 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`agent_mcp_workspace_lease_test::claim_path_is_current_worktree`；AC-2=`agent_mcp_workspace_lease_test::claim_owner_is_mcp_actor`；AC-3=`agent_mcp_workspace_lease_test::claim_held_by_other_owner_is_022`；AC-4=`agent_mcp_workspace_lease_test::claim_replay_returns_same_workspace_id`；AC-5=`agent_mcp_workspace_lease_test::claim_replay_returns_same_fence`；AC-6=`agent_mcp_workspace_lease_test::claim_replay_keeps_single_lease`；AC-7=`agent_mcp_workspace_lease_test::claim_crash_before_commit_leaves_no_lease`；AC-8=`agent_mcp_workspace_lease_test::claim_crash_rolls_back_intent_and_operation`；AC-9=`agent_mcp_workspace_lease_test::claim_crash_rolls_back_lease_metadata`；AC-10=`agent_mcp_workspace_lease_test::claim_retry_after_crash_succeeds`；AC-11=`agent_mcp_workspace_lease_test::claim_retry_leaves_single_lease_metadata`；AC-12=`agent_mcp_workspace_lease_test::claim_crash_keeps_preexisting_lease`；AC-13=`agent_mcp_workspace_lease_test::claim_replay_in_other_worktree_is_032`；AC-14=`agent_mcp_workspace_lease_test::claim_release_cycles_hit_group_quota`；AC-15=`agent_mcp_workspace_lease_test::claim_release_cycles_registry_rows_bounded`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_path_is_current_worktree -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_owner_is_mcp_actor -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_held_by_other_owner_is_022 -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_replay_returns_same_workspace_id -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_replay_returns_same_fence -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_replay_keeps_single_lease -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_crash_before_commit_leaves_no_lease -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_crash_rolls_back_intent_and_operation -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_crash_rolls_back_lease_metadata -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_retry_after_crash_succeeds -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_retry_leaves_single_lease_metadata -- --exact`（new）
- [ ] AC-12：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_crash_keeps_preexisting_lease -- --exact`（new）
- [ ] AC-13：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_replay_in_other_worktree_is_032 -- --exact`（new）
- [ ] AC-14：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_release_cycles_hit_group_quota -- --exact`（new）
- [ ] AC-15：`source .env.test && cargo test --test agent_mcp_workspace_lease_test claim_release_cycles_registry_rows_bounded -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_workspace_claim' docs/commands/agent.md && rg -qF -- 'operation_id' docs/commands/agent.md && rg -qF -- 'lease_ttl_ms' docs/commands/agent.md && rg -qF -- '600000' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_workspace_claim' docs/commands/zh-CN/agent.md && rg -qF -- 'operation_id' docs/commands/zh-CN/agent.md && rg -qF -- 'lease_ttl_ms' docs/commands/zh-CN/agent.md && rg -qF -- '600000' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_workspace_claim'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`（`libra_workspace_claim` 小节内：标题为 `####` 加反引号包裹的工具名）：`python3 -I tests/tools/doc_section_check.py --markdown libra_workspace_claim ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md 'operation_id' 'lease_ttl_ms' '600000'`；GC-MC-05 ①–⑤ 交付后线上页面（DOM 小节：自 `id` 为 `libra-workspace-claim` 的标题元素至下一个同级或更高级标题元素，不按行切分）：`curl -fsS https://libra.tools/en/docs/commands/agent | python3 -I tests/tools/doc_section_check.py --id libra-workspace-claim 'operation_id' 'lease_ttl_ms' '600000'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_workspace_lease_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_workspace_lease_test"' Cargo.toml`
- [ ] 小节检查脚本自检：`python3 -I tests/tools/doc_section_check.py --self-test`（对单行 HTML fixture 与 Markdown fixture 断言：目标小节之外——含其后更高级的 `###` 小节——的 token 不会使检查通过，目标小节内缺失 token 时退出码非 0）
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-40`（写集串行：`tools/workspace.rs`、agent 文档）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `tests/tools/doc_section_check.py`（新：HTML 模式以标准库 `html.parser` 按 DOM 提取指定 `id` 标题至下一个同级或更高级标题之间的文本；Markdown 模式 `--markdown <heading-text>` 提取以该文本为标题的小节至下一个同级或更高级（`#` 数不多于本标题）标题；两种模式都检查所给 token；`--self-test` 对两个 fixture 自检）、`tests/fixtures/docs/one-line-sections.html`（新：多个标题在同一行的单行 HTML）、`tests/fixtures/docs/sections.md`（新：目标 `####` 小节后紧跟 `###` 小节且后者含目标小节缺失的 token）、`src/internal/ai/mcp_server/tools/mod.rs`、`src/internal/ai/mcp_server/tools/workspace.rs`、`tests/agent_mcp_workspace_lease_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：工具表新增 `libra_workspace_claim`（必填 `operation_id`、可选 `lease_ttl_ms` 缺省 600000 毫秒、事务绑定与重放语义、只作用于当前工作树含主工作树）；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 路径、worktree 与 owner 只由服务端派生；每次 renew/release 复核 lease 归属当前工作树。

**Performance budget:** 每次调用一次 registry 读与一次 store 事务。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP 为当前工作树取得 lease（事务绑定、可重放）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=15/8@EX-MC-09; VER=15/8@EX-MC-09; landing=1; prod-files=2; scope=M; deps=MC-40,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-40; release=independent; split-from=N/A; exception=EX-MC-09`

### Task MC-12: trace session 只读工具与查询共享

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 把 `agent session list/show` 的查询抽为 `session.rs` 内返回页结构体的公开函数（CLI 输出逐字节不变），新增只读工具 `libra_trace_session_list`、`libra_trace_session_show`，结果等于对应 `--json` 的 `data` 经结果信封后的值（只含元数据摘要，不读取 transcript blob）。checkpoint 查询由 MC-20 承接。唯一行为轴：「MCP 读取已捕获的 agent session 元数据」。

**Out of scope:** checkpoint 查询（由 MC-20 承接）；transcript 导出与 `--raw`、rewind（DEFER-MC-01）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| session 查询与输出 | `src/command/agent/session.rs:407-500`、`:501-584`、`:765-780`、`:854-892` |
| 既有 CLI 测试模块 | `tests/command/agent_checkpoint_test.rs`（session 相关用例同在此模块） |

**Acceptance criteria:**

- [ ] AC-1 `libra --json agent session list` 对固定 fixture 的输出等于先于重构写入的内联期望值。
- [ ] AC-2 `libra --json agent session show <id>` 对固定 fixture 的输出等于先于重构写入的内联期望值。
- [ ] AC-3 `libra_trace_session_list` 的 `structuredContent` 等于同参数 CLI `--json` 的 `data`。
- [ ] AC-4 `libra_trace_session_list` 传 `limit = 1000` 的结果等于 CLI `--limit 1000` 的 `data`。
- [ ] AC-5 以 `libra_trace_session_list` 的 `next_cursor` 翻页，结果等于 CLI `--cursor` 的下一页。
- [ ] AC-6 非法 cursor 得到工具错误 `LBR-AGENT-027`。
- [ ] AC-7 `libra_trace_session_show` 对不存在的 id 得到工具错误，其 `stable_code` 等于 CLI 同输入的 stable code。
- [ ] AC-8 fixture transcript 的正文标记字符串不出现在 `libra_trace_session_list` 的响应中。
- [ ] AC-9 同一标记不出现在 `libra_trace_session_show` 的响应中。
- [ ] AC-10 仓库无 `agent_session` 表时 `libra_trace_session_list` 返回空页。
- [ ] AC-11 `libra_trace_session_list` 的 `annotations.readOnlyHint` 为 `true`。
- [ ] AC-12 `libra_trace_session_show` 的 `annotations.readOnlyHint` 为 `true`。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**判据规范（非计数正文）:** 门族（12 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`command_test::command::agent_checkpoint_test::session_list_json_byte_stable`；AC-2=`command_test::command::agent_checkpoint_test::session_show_json_byte_stable`；AC-3=`agent_mcp_trace_test::trace_session_list_matches_cli`；AC-4=`agent_mcp_trace_test::trace_session_limit_matches_cli`；AC-5=`agent_mcp_trace_test::trace_session_cursor_paging`；AC-6=`agent_mcp_trace_test::trace_invalid_cursor_is_tool_error`；AC-7=`agent_mcp_trace_test::trace_session_show_missing`；AC-8=`agent_mcp_trace_test::session_list_excludes_transcript_body`；AC-9=`agent_mcp_trace_test::session_show_excludes_transcript_body`；AC-10=`agent_mcp_trace_test::trace_session_list_empty_store`；AC-11=`agent_mcp_trace_test::trace_session_list_read_only`；AC-12=`agent_mcp_trace_test::trace_session_show_read_only`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test command_test command::agent_checkpoint_test::session_list_json_byte_stable -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test command_test command::agent_checkpoint_test::session_show_json_byte_stable -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_trace_test trace_session_list_matches_cli -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_trace_test trace_session_limit_matches_cli -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_trace_test trace_session_cursor_paging -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_trace_test trace_invalid_cursor_is_tool_error -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_trace_test trace_session_show_missing -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_trace_test session_list_excludes_transcript_body -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_mcp_trace_test session_show_excludes_transcript_body -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_mcp_trace_test trace_session_list_empty_store -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_mcp_trace_test trace_session_list_read_only -- --exact`（new）
- [ ] AC-12：`source .env.test && cargo test --test agent_mcp_trace_test trace_session_show_read_only -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_trace_session_list' docs/commands/agent.md && rg -qF -- 'libra_trace_session_show' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_trace_session_list' docs/commands/zh-CN/agent.md && rg -qF -- 'libra_trace_session_show' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_trace_session_list'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'libra_trace_session_list' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'libra_trace_session_show' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'libra_trace_session_list' && printf '%s' "$PAGE" | rg -qF -- 'libra_trace_session_show'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_trace_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_trace_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: CLI 与 MCP 共享的 trace 查询单一事实源（GC-02）及 Cargo.toml [[test]] 行`

**Dependencies:** `MC-41`（写集串行：`tools/mod.rs`、agent 文档）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/command/agent/session.rs`、`src/internal/ai/mcp_server/tools/mod.rs`、`src/internal/ai/mcp_server/tools/trace.rs`（新）、`tests/command/agent_checkpoint_test.rs`、`tests/agent_mcp_trace_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：trace session 两工具；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 只返回 CLI `--json` 已公开字段；不读取 transcript blob。

**Performance budget:** keyset 分页，上限沿用 CLI；多取 1 行判断下一页。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=trace session 只读工具与查询共享; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=12/8@EX-MC-06; VER=12/8@EX-MC-06; landing=2; prod-files=3; scope=M; deps=MC-41,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-41; release=independent; split-from=N/A; exception=EX-MC-06`

### Task MC-13: loopback Streamable HTTP 传输（legacy）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 请求接收有界：① **连接准入**——接受循环持有容量 64 的信号量，在读取任何头部之前为每个连接取得一个许可（连接关闭时归还）；许可耗尽时新连接立即收到 `503`（`Retry-After: 1`）并关闭，因此同时处于读取阶段的连接与其缓冲有界：每个连接除 ≤ 256 KiB 的请求体外，还有 Hyper HTTP/1 的连接读缓冲（本卡不收紧，取 Hyper 默认上限约 400 KiB；固定的 Hyper 1.8.1 在超长头部时可能多一次读分配，见 hyper#4081，因此不把它当作严格头部上限），预算按每连接 ≤ 1 MiB 估计，合计 ≤ 64 MiB（容量估计，非硬上限）；② **只服务 HTTP/1.1 与头部期限**——以 `hyper::server::conn::http1::Builder` 服务连接（不经 `axum::serve`，不启用 HTTP/2，因此每个连接同时至多一个请求、连接许可即请求读取并发的上界），设 `header_read_timeout(10 s)`（`hyper_util::rt::TokioTimer`），超时即关闭连接；HTTP/2 连接前言（`PRI * HTTP/2.0`）按非法 HTTP/1 请求处理并关闭连接；③ **请求体绝对期限**——处理器以 `tokio::time::timeout`（自头部读完起算 10 s 的绝对截止时间，不随帧到达重置）包住整段 `axum::body::to_bytes(body, 256 KiB)`，超时返回 `408`；准入槽位（本卡临时模式与 MC-17 的并发/队列）只在请求体完整收到之后才获取、在响应写出后释放。为此本卡把 `hyper = { version = "1.8.1", features = ["server", "http1"] }` 与 `hyper-util = { version = "0.1.20", features = ["tokio"] }`（只用其 `TokioIo`/`TokioTimer`，不启用 `server-auto`/HTTP/2） 加为直接依赖（与 `Cargo.lock` 现有版本一致，由工具链更新 lockfile）。新增 `--transport http` 及 `--listen`、`--token-out`、`--allowed-host`、`--allowed-origin`（仅 Unix）。令牌由服务进程在**每次启动时**用操作系统 CSPRNG 生成 32 字节，**先校验目录并取得令牌路径的独占所有权**：无论 `--token-out` 是否已存在，其所在目录都必须属于当前有效用户且不可被组/其他用户写（否则拒绝启动）；令牌哈希不写在被加锁的锁文件里，而是写在同目录的状态文件 `.libra-mcp-token-<hash>.state`（内容 `{current, pending}`），每次更新都以「写临时文件 → fsync → rename 覆盖 → fsync 目录」整体替换，因此任何时刻崩溃都只会留下完整的旧状态或完整的新状态（不会撕裂）；两阶段记录本服务写入的令牌哈希（**首次创建与轮换相同**）：先把新令牌的 SHA-256 写为状态中的 `pending`，首次创建时先在同目录以 `O_CREAT|O_EXCL`、0600 写完整的临时文件并 `fsync`，再以 `link(临时文件, --token-out)` 原子发布（目标已存在则 `link` 失败，因此绝不替换任何已存在文件）、`fsync` 目录、删除临时文件——令牌路径上永远只会出现完整且已落盘的令牌；轮换时写临时文件、`fsync` 临时文件、`rename` 替换令牌文件、`fsync` 目录，最后把 `pending` 提升为 `current` 并清空 `pending`（`fsync`）；`--token-out` 已存在时，除原有的普通文件/属主/0600/目录私有校验外，还要求其内容的 SHA-256 等于 `current` 或 `pending` 之一（因此任一步之间崩溃后都能识别自己的令牌文件）（即它是本服务先前发布且未被改动的令牌），否则拒绝启动且不改动该文件——因此误指的私钥或配置文件不会被覆盖。令牌锁与状态文件放在**令牌文件自己的父目录**（该目录已要求私有：属于当前有效用户、不可被组/其他用户写）中的保留文件名：`.libra-mcp-token-<sha256(最后路径分量)>.lock` 与同名 `.state`；因此所有权只取决于令牌所在的物理目录与文件名，与 `LIBRA_HOME`、仓库或进程环境无关——经符号链接别名的父目录解析到同一物理目录、得到同一把锁；`--token-out` 的最后路径分量不得以保留前缀 `.libra-mcp-token-` 开头（否则拒绝启动），且其 canonical 父目录落在当前仓库共享存储目录（含 `mcp/` 写锁目录）之内时拒绝启动，因此令牌文件的替换永远不会触及任何锁或状态文件。锁以 MC-06 的 `acquire_private_lock` 获取（`O_CREAT|O_NOFOLLOW`、0600、`fstat` 校验；不复用会跟随符号链接并写入 PID 的 `acquire_control_lock`），整个服务生命周期持有；锁已被存活进程持有即以非零退出码结束且不触碰 `--token-out`（无论端口是否相同，含 `127.0.0.1:0`）。然后**绑定监听套接字**（绑定失败同样不触碰 `--token-out`），二者都成功后才生成令牌并以 64 个小写十六进制字符写入 `--token-out`：路径不存在时按下述「临时文件 + `link`」序列发布（不直接创建目标路径）；路径已存在时只接受「非符号链接的普通文件、属主为当前有效用户、权限恰为 0600、所在目录不可被组/其他用户写」，并在同目录以 `O_EXCL`、0600 写临时文件、`fsync` 后 `rename` 原子替换（旧令牌随之失效），其余情形拒绝启动且不改动该路径；因此服务管理器可用同一命令行反复重启（正常退出、写入期限退出或崩溃之后）。服务从不读取外部提供的秘密，因此不存在弱令牌导入路径；每次启动轮换。客户端交接（文档化）：远程客户端只持有 TLS 终端自身的凭据，终端在服务就绪（stderr `listening on` 行）后由服务管理器触发重载，从 `--token-out` 读取令牌注入上游 `Authorization: Bearer`；本机客户端每次连接前重读该文件：用既有 `axum` 在 loopback 上提供 `/mcp`，按 ADR-MC-07 校验令牌、Origin、Host 与 peer；按 ADR-MC-04 对 legacy 请求校验 `MCP-Protocol-Version` 头；复用同一协议核心与工具注册表；SIGTERM 优雅停机。工具调用速率沿用 MC-21 的进程级令牌桶，HTTP 上把 `LBR-AGENT-029` 映射为 429 + `Retry-After: 1`。本卡的并发界限为**临时安全模式**：同一时刻只执行一个请求（信号量容量 1），槽位被占用时新请求立即返回 503 + `Retry-After: 1`（不排队）；MC-17 再提升为并发 16、等待队列 64，并在同卡移除本卡的三个临时模式用例（本卡 AC-34..36 只对 MC-13 至 MC-17 之间的版本有效）。唯一行为轴：「MCP 的本机 HTTP 传输与访问控制」。

**Out of scope:** 并发 16 / 队列 64 的 HTTP 请求准入（由 MC-17 承接）；写调用串行化（MCP 仓库写锁，MC-06 已交付）；TLS、OAuth、SSE、`GET` 流（永久非目标，ADR-MC-07）；modern 纪元与 `Mcp-Method`/`Mcp-Name` 头（由 MC-14 承接）；Windows（DEFER-MC-05）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| loopback-only 与令牌先例 | `src/command/service.rs:1-11`、`:104-107`、`:168-176` |
| 令牌权限校验在非 Unix 无条件成功 | `src/command/control_lock.rs:752-790` |
| `axum` 已在依赖中 | `Cargo.toml:101` |
| 规范 HTTP 与版本头 | 规范 `2025-11-25/basic/transports.mdx` 第 70-106 行与「Protocol Version Header」（pin `1155ca46`） |

**Acceptance criteria:**

- [ ] AC-1 `--listen 0.0.0.0:0` 以用法拒绝 `LBR-CLI-002` 结束。
- [ ] AC-2 `--listen localhost:0` 以用法拒绝 `LBR-CLI-002` 结束。
- [ ] AC-3 平台判定函数对非 Unix 输入返回「不支持」。
- [ ] AC-4 平台判定函数对 Unix 输入返回「支持」。
- [ ] AC-5 `--token-out` 指向符号链接时进程退出码非 0。
- [ ] AC-6 `--token-out` 指向权限为 0644 的已存在文件时进程退出码非 0。
- [ ] AC-7 同一情形下该文件内容不变。
- [ ] AC-8 正常启动后 `--token-out` 文件的权限为 0600。
- [ ] AC-9 该文件内容匹配 `^[0-9a-f]{64}\n?$`。
- [ ] AC-10 服务以 SIGTERM 正常退出后，以完全相同的命令行（同一 `--token-out`）再次启动，stderr 出现 `listening on` 行。
- [ ] AC-11 该次重启后文件中的令牌与重启前不同。
- [ ] AC-12 该次重启后以旧令牌发送的 `initialize` 状态码为 401。
- [ ] AC-13 `LIBRA_TEST=1` 下以 `--allow-write` 启动的 HTTP 服务因 `libra_test_repo_write`（`stall = true`、写入期限按 MC-06 的测试覆盖缩短为 2 s）退出后，以相同命令行重启，stderr 出现 `listening on` 行。
- [ ] AC-14 以该文件中的令牌作 `Authorization: Bearer` 的 `initialize` 状态码为 200。
- [ ] AC-15 `--token-file` 不是 `libra agent mcp` 的参数（给出即以用法拒绝 `LBR-CLI-002` 结束）。
- [ ] AC-16 缺少 `Authorization` 头的请求状态码为 401。
- [ ] AC-17 同一 401 响应带 `WWW-Authenticate: Bearer` 头。
- [ ] AC-18 错误令牌的请求状态码为 401。
- [ ] AC-19 令牌只放在 URL query（`/mcp?token=…`）的请求状态码为 401。
- [ ] AC-20 `Origin: https://evil.example` 的请求状态码为 403。
- [ ] AC-21 `Host: evil.example` 的请求状态码为 403。
- [ ] AC-22 `Host` 为 `--allowed-host` 中的值时 `initialize` 状态码为 200。
- [ ] AC-23 合法 POST `initialize` 的状态码为 200。
- [ ] AC-24 同一响应的 `Content-Type` 为 `application/json`。
- [ ] AC-25 请求 `2025-06-18` 的 `initialize` 结果 `protocolVersion` 等于 `"2025-06-18"`。
- [ ] AC-26 POST `notifications/initialized` 的状态码为 202。
- [ ] AC-27 同一 202 响应体为空。
- [ ] AC-28 `GET /mcp` 的状态码为 405。
- [ ] AC-29 `DELETE /mcp` 的状态码为 405。
- [ ] AC-30 非 `initialize` 请求缺少 `MCP-Protocol-Version` 头时状态码为 400。
- [ ] AC-31 非 `initialize` 请求的 `MCP-Protocol-Version` 为 `2025-03-26` 时状态码为 400。
- [ ] AC-32 请求体超过 256 KiB 时状态码为 413。
- [ ] AC-33 413 之后的下一个合法请求状态码为 200。
- [ ] AC-34 `LIBRA_TEST=1` 下一个 `libra_test_sleep {"ms": 3000}` 执行期间发出的第二个请求状态码为 503。
- [ ] AC-35 同一第二个请求从发出到收到响应不超过 1 s。
- [ ] AC-36 同一 503 响应带 `Retry-After: 1` 头。
- [ ] AC-37 MC-21 的令牌桶耗尽后，经 HTTP 发出的 `tools/call` 状态码为 429。
- [ ] AC-38 同一 429 响应带 `Retry-After: 1` 头。
- [ ] AC-39 同一桶耗尽状态下经 HTTP 发出的 `initialize` 状态码为 200。
- [ ] AC-40 在途请求未完成时收到 SIGTERM，该请求最终状态码为 200。
- [ ] AC-41 同一 SIGTERM 后进程退出码为 0。
- [ ] AC-42 同一 `--listen` 地址已有存活服务时，以相同命令行启动的第二个实例退出码非 0。
- [ ] AC-43 同一情形下 `--token-out` 文件的内容不变（存活服务的令牌未被替换）。
- [ ] AC-44 同一情形下以该文件中的令牌发送的 `initialize` 状态码为 200。
- [ ] AC-45 另一端口（`127.0.0.1:0`）、相同 `--token-out` 的第二个实例在第一个实例存活时退出码非 0。
- [ ] AC-46 同一情形下 `--token-out` 文件的内容不变。
- [ ] AC-47 同一情形下以该文件中的令牌发送给第一个实例的 `initialize` 状态码为 200。
- [ ] AC-48 `--token-out` 不存在、但其所在目录权限为 0777 时进程退出码非 0。
- [ ] AC-49 令牌父目录中对应的 `.libra-mcp-token-<hash>.lock` 预先被创建为指向另一文件的符号链接时进程退出码非 0。
- [ ] AC-50 同一情形下该符号链接指向的文件内容不变。
- [ ] AC-51 `--token-out` 指向仓库共享存储目录下 `mcp/` 中的路径时进程退出码非 0。
- [ ] AC-52 服务 A 以 `--token-out <dir>/token`、服务 B（另一端口）以 `--token-out <dir>/token.lock` 先后启动时，两者都正常启动。
- [ ] AC-53 同一情形下以 A 的令牌文件中的令牌发送给 A 的 `initialize` 状态码为 200（B 未替换 A 的令牌或锁）。
- [ ] AC-54 `--token-out` 指向一个已存在、属于当前用户、权限 0600、但不是本服务写入的普通文件时进程退出码非 0。
- [ ] AC-55 同一情形下该文件内容不变。
- [ ] AC-56 在记录 `pending` 之后、`rename` 之前注入崩溃（`LIBRA_TEST=1` failpoint）后，以相同命令行重启，stderr 出现 `listening on` 行。
- [ ] AC-57 在 `rename` 之后、提升 `current` 之前注入崩溃后，以相同命令行重启，stderr 出现 `listening on` 行。
- [ ] AC-58 首次启动（`--token-out` 不存在）时，在记录 `pending` 之后、创建令牌文件之前注入崩溃，以相同命令行重启，stderr 出现 `listening on` 行。
- [ ] AC-59 首次启动时在令牌文件创建之后、提升 `current` 之前注入崩溃，以相同命令行重启，stderr 出现 `listening on` 行。
- [ ] AC-60 单元门：以可注入的文件系统操作记录器运行首次创建，记录的顺序为「以原子替换写入含 `pending` 的状态文件 → 写临时令牌 → fsync(临时令牌) → link(临时令牌, 令牌路径) → fsync(令牌目录) → 删除临时令牌 → 以原子替换写入含 `current` 的状态文件」。
- [ ] AC-61 单元门：以同一记录器运行轮换，记录的顺序为「以原子替换写入含 `pending` 的状态文件 → 写临时文件 → fsync(临时文件) → rename → fsync(令牌目录) → 以原子替换写入含 `current` 的状态文件」。
- [ ] AC-62 在写状态临时文件写到一半时注入崩溃（`LIBRA_TEST=1` failpoint）后，以相同命令行重启，stderr 出现 `listening on` 行（旧状态完整保留）。
- [ ] AC-63 首次启动时在临时令牌文件写到一半时注入崩溃后，以相同命令行重启时 stderr 出现 `listening on` 行。
- [ ] AC-64 同一临时仓库中同时运行 stdio `--actor workbuddy --allow-write`、HTTP `--actor grokbot --allow-write` 两个服务与一个 `libra agent bridge --stdio` 进程，两个 MCP 服务几乎同时各发起一次 `libra_checkpoint_create`，二者都在 35 s 内成功（后到者等待写锁后完成）。
- [ ] AC-65 同一并发窗口内 bridge 的 `status.get` 成功返回（MCP 写锁不阻塞 Harness）。
- [ ] AC-66 以尚不存在的 `--token-out` 启动后（启动成功为前提），令牌父目录中出现锁文件 `.libra-mcp-token-<sha256(文件名)>.lock`。
- [ ] AC-67 服务 A 以 `--token-out <dir>/token` 运行时，服务 B 以经符号链接别名的父目录 `--token-out <alias>/token`（另一端口）启动，退出码非 0（同一规范路径得到同一把锁）。
- [ ] AC-68 两个服务以不同的 `LIBRA_HOME`、相同的 `--token-out`（不同端口）先后启动时，第二个退出码非 0。
- [ ] AC-69 `--token-out` 的文件名以 `.libra-mcp-token-` 开头时进程退出码非 0。
- [ ] AC-70 发出头部后每 2 s 发送 1 字节请求体（持续超过 10 s）的请求，在头部读完后 12 s 内得到状态码 408（截止时间不随帧到达重置）。
- [ ] AC-71 同一停滞请求存续期间，另一个完整的 `initialize` 请求在 1 s 内得到 200（停滞请求不占用准入槽位）。
- [ ] AC-72 只发送部分请求头后停滞的连接，在 12 s 内被服务端关闭。
- [ ] AC-73 同时打开 64 个只发部分请求体的连接后，第 65 个连接在 1 s 内收到状态码 503。
- [ ] AC-74 发送 HTTP/2 连接前言（`PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n`）的连接在 1 s 内被服务端关闭。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**判据规范（非计数正文）:** 门族（74 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`agent_mcp_http_test::refuses_wildcard_listen`；AC-2=`agent_mcp_http_test::refuses_hostname_listen`；AC-3=`--lib internal::ai::mcp_server::http::tests::http_unsupported_off_unix`；AC-4=`--lib internal::ai::mcp_server::http::tests::http_supported_on_unix`；AC-5=`agent_mcp_http_test::token_out_refuses_symlink`；AC-6=`agent_mcp_http_test::token_out_refuses_non_0600_existing`；AC-7=`agent_mcp_http_test::token_out_keeps_rejected_file`；AC-8=`agent_mcp_http_test::token_out_file_mode`；AC-9=`agent_mcp_http_test::token_out_format`；AC-10=`agent_mcp_http_test::restart_same_token_out_after_exit`；AC-11=`agent_mcp_http_test::token_rotates_per_start`；AC-12=`agent_mcp_http_test::restart_invalidates_old_token`；AC-13=`agent_mcp_http_test::restart_after_write_deadline_exit`；AC-14=`agent_mcp_http_test::minted_token_authorizes`；AC-15=`agent_mcp_http_test::no_token_import_option`；AC-16=`agent_mcp_http_test::missing_bearer_is_401`；AC-17=`agent_mcp_http_test::missing_bearer_has_challenge`；AC-18=`agent_mcp_http_test::wrong_bearer_is_401`；AC-19=`agent_mcp_http_test::query_token_is_ignored`；AC-20=`agent_mcp_http_test::bad_origin_is_403`；AC-21=`agent_mcp_http_test::bad_host_is_403`；AC-22=`agent_mcp_http_test::allowed_host_is_accepted`；AC-23=`agent_mcp_http_test::post_initialize_is_200`；AC-24=`agent_mcp_http_test::post_initialize_is_json`；AC-25=`agent_mcp_http_test::post_initialize_negotiates`；AC-26=`agent_mcp_http_test::notification_is_202`；AC-27=`agent_mcp_http_test::notification_body_empty`；AC-28=`agent_mcp_http_test::get_is_405`；AC-29=`agent_mcp_http_test::delete_is_405`；AC-30=`agent_mcp_http_test::missing_version_header_is_400`；AC-31=`agent_mcp_http_test::unsupported_version_header_is_400`；AC-32=`agent_mcp_http_test::oversized_body_is_413`；AC-33=`agent_mcp_http_test::server_survives_413`；AC-34=`agent_mcp_http_test::interim_mode_rejects_when_busy`；AC-35=`agent_mcp_http_test::interim_mode_rejects_without_waiting`；AC-36=`agent_mcp_http_test::interim_mode_busy_has_retry_after`；AC-37=`agent_mcp_http_test::rate_limited_tool_call_is_429`；AC-38=`agent_mcp_http_test::rate_limited_has_retry_after`；AC-39=`agent_mcp_http_test::initialize_not_rate_limited`；AC-40=`agent_mcp_http_test::sigterm_drains_inflight`；AC-41=`agent_mcp_http_test::sigterm_exits_zero`；AC-42=`agent_mcp_http_test::second_instance_same_port_fails`；AC-43=`agent_mcp_http_test::failed_second_instance_keeps_token_file`；AC-44=`agent_mcp_http_test::failed_second_instance_keeps_live_token_valid`；AC-45=`agent_mcp_http_test::second_instance_other_port_same_token_out_fails`；AC-46=`agent_mcp_http_test::other_port_failure_keeps_token_file`；AC-47=`agent_mcp_http_test::other_port_failure_keeps_live_token_valid`；AC-48=`agent_mcp_http_test::token_out_new_path_in_shared_dir_refused`；AC-49=`agent_mcp_http_test::token_lock_symlink_refused`；AC-50=`agent_mcp_http_test::token_lock_symlink_target_untouched`；AC-51=`agent_mcp_http_test::token_out_inside_repo_storage_refused`；AC-52=`agent_mcp_http_test::colliding_token_names_both_start`；AC-53=`agent_mcp_http_test::colliding_token_names_keep_first_token_valid`；AC-54=`agent_mcp_http_test::token_out_refuses_unrelated_private_file`；AC-55=`agent_mcp_http_test::token_out_keeps_unrelated_private_file`；AC-56=`agent_mcp_http_test::token_rotation_crash_before_rename_restarts`；AC-57=`agent_mcp_http_test::token_rotation_crash_after_rename_restarts`；AC-58=`agent_mcp_http_test::token_first_create_crash_before_file_restarts`；AC-59=`agent_mcp_http_test::token_first_create_crash_after_file_restarts`；AC-60=`--lib internal::ai::mcp_server::http::tests::token_publish_fsync_order`；AC-61=`--lib internal::ai::mcp_server::http::tests::token_rotation_fsync_order`；AC-62=`agent_mcp_http_test::token_state_torn_write_restarts`；AC-63=`agent_mcp_http_test::token_first_create_partial_write_restarts`；AC-64=`agent_mcp_http_test::workbuddy_grokbot_and_bridge_coexist_writes`；AC-65=`agent_mcp_http_test::bridge_unblocked_during_mcp_writes`；AC-66=`agent_mcp_http_test::token_lock_key_for_missing_file`；AC-67=`agent_mcp_http_test::aliased_token_path_shares_lock`；AC-68=`agent_mcp_http_test::different_libra_home_same_token_path_shares_lock`；AC-69=`agent_mcp_http_test::reserved_token_filename_refused`；AC-70=`agent_mcp_http_test::trickled_body_hits_absolute_deadline_408`；AC-71=`agent_mcp_http_test::stalled_body_does_not_hold_admission`；AC-72=`agent_mcp_http_test::stalled_headers_closed_by_deadline`；AC-73=`agent_mcp_http_test::connection_admission_limit_503`；AC-74=`agent_mcp_http_test::http2_preface_rejected`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_http_test refuses_wildcard_listen -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_http_test refuses_hostname_listen -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib internal::ai::mcp_server::http::tests::http_unsupported_off_unix -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --lib internal::ai::mcp_server::http::tests::http_supported_on_unix -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_http_test token_out_refuses_symlink -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_http_test token_out_refuses_non_0600_existing -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_http_test token_out_keeps_rejected_file -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_http_test token_out_file_mode -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_mcp_http_test token_out_format -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_mcp_http_test restart_same_token_out_after_exit -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_mcp_http_test token_rotates_per_start -- --exact`（new）
- [ ] AC-12：`source .env.test && cargo test --test agent_mcp_http_test restart_invalidates_old_token -- --exact`（new）
- [ ] AC-13：`source .env.test && cargo test --test agent_mcp_http_test restart_after_write_deadline_exit -- --exact`（new）
- [ ] AC-14：`source .env.test && cargo test --test agent_mcp_http_test minted_token_authorizes -- --exact`（new）
- [ ] AC-15：`source .env.test && cargo test --test agent_mcp_http_test no_token_import_option -- --exact`（new）
- [ ] AC-16：`source .env.test && cargo test --test agent_mcp_http_test missing_bearer_is_401 -- --exact`（new）
- [ ] AC-17：`source .env.test && cargo test --test agent_mcp_http_test missing_bearer_has_challenge -- --exact`（new）
- [ ] AC-18：`source .env.test && cargo test --test agent_mcp_http_test wrong_bearer_is_401 -- --exact`（new）
- [ ] AC-19：`source .env.test && cargo test --test agent_mcp_http_test query_token_is_ignored -- --exact`（new）
- [ ] AC-20：`source .env.test && cargo test --test agent_mcp_http_test bad_origin_is_403 -- --exact`（new）
- [ ] AC-21：`source .env.test && cargo test --test agent_mcp_http_test bad_host_is_403 -- --exact`（new）
- [ ] AC-22：`source .env.test && cargo test --test agent_mcp_http_test allowed_host_is_accepted -- --exact`（new）
- [ ] AC-23：`source .env.test && cargo test --test agent_mcp_http_test post_initialize_is_200 -- --exact`（new）
- [ ] AC-24：`source .env.test && cargo test --test agent_mcp_http_test post_initialize_is_json -- --exact`（new）
- [ ] AC-25：`source .env.test && cargo test --test agent_mcp_http_test post_initialize_negotiates -- --exact`（new）
- [ ] AC-26：`source .env.test && cargo test --test agent_mcp_http_test notification_is_202 -- --exact`（new）
- [ ] AC-27：`source .env.test && cargo test --test agent_mcp_http_test notification_body_empty -- --exact`（new）
- [ ] AC-28：`source .env.test && cargo test --test agent_mcp_http_test get_is_405 -- --exact`（new）
- [ ] AC-29：`source .env.test && cargo test --test agent_mcp_http_test delete_is_405 -- --exact`（new）
- [ ] AC-30：`source .env.test && cargo test --test agent_mcp_http_test missing_version_header_is_400 -- --exact`（new）
- [ ] AC-31：`source .env.test && cargo test --test agent_mcp_http_test unsupported_version_header_is_400 -- --exact`（new）
- [ ] AC-32：`source .env.test && cargo test --test agent_mcp_http_test oversized_body_is_413 -- --exact`（new）
- [ ] AC-33：`source .env.test && cargo test --test agent_mcp_http_test server_survives_413 -- --exact`（new）
- [ ] AC-34：`source .env.test && cargo test --test agent_mcp_http_test interim_mode_rejects_when_busy -- --exact`（new）
- [ ] AC-35：`source .env.test && cargo test --test agent_mcp_http_test interim_mode_rejects_without_waiting -- --exact`（new）
- [ ] AC-36：`source .env.test && cargo test --test agent_mcp_http_test interim_mode_busy_has_retry_after -- --exact`（new）
- [ ] AC-37：`source .env.test && cargo test --test agent_mcp_http_test rate_limited_tool_call_is_429 -- --exact`（new）
- [ ] AC-38：`source .env.test && cargo test --test agent_mcp_http_test rate_limited_has_retry_after -- --exact`（new）
- [ ] AC-39：`source .env.test && cargo test --test agent_mcp_http_test initialize_not_rate_limited -- --exact`（new）
- [ ] AC-40：`source .env.test && cargo test --test agent_mcp_http_test sigterm_drains_inflight -- --exact`（new）
- [ ] AC-41：`source .env.test && cargo test --test agent_mcp_http_test sigterm_exits_zero -- --exact`（new）
- [ ] AC-42：`source .env.test && cargo test --test agent_mcp_http_test second_instance_same_port_fails -- --exact`（new）
- [ ] AC-43：`source .env.test && cargo test --test agent_mcp_http_test failed_second_instance_keeps_token_file -- --exact`（new）
- [ ] AC-44：`source .env.test && cargo test --test agent_mcp_http_test failed_second_instance_keeps_live_token_valid -- --exact`（new）
- [ ] AC-45：`source .env.test && cargo test --test agent_mcp_http_test second_instance_other_port_same_token_out_fails -- --exact`（new）
- [ ] AC-46：`source .env.test && cargo test --test agent_mcp_http_test other_port_failure_keeps_token_file -- --exact`（new）
- [ ] AC-47：`source .env.test && cargo test --test agent_mcp_http_test other_port_failure_keeps_live_token_valid -- --exact`（new）
- [ ] AC-48：`source .env.test && cargo test --test agent_mcp_http_test token_out_new_path_in_shared_dir_refused -- --exact`（new）
- [ ] AC-49：`source .env.test && cargo test --test agent_mcp_http_test token_lock_symlink_refused -- --exact`（new）
- [ ] AC-50：`source .env.test && cargo test --test agent_mcp_http_test token_lock_symlink_target_untouched -- --exact`（new）
- [ ] AC-51：`source .env.test && cargo test --test agent_mcp_http_test token_out_inside_repo_storage_refused -- --exact`（new）
- [ ] AC-52：`source .env.test && cargo test --test agent_mcp_http_test colliding_token_names_both_start -- --exact`（new）
- [ ] AC-53：`source .env.test && cargo test --test agent_mcp_http_test colliding_token_names_keep_first_token_valid -- --exact`（new）
- [ ] AC-54：`source .env.test && cargo test --test agent_mcp_http_test token_out_refuses_unrelated_private_file -- --exact`（new）
- [ ] AC-55：`source .env.test && cargo test --test agent_mcp_http_test token_out_keeps_unrelated_private_file -- --exact`（new）
- [ ] AC-56：`source .env.test && cargo test --test agent_mcp_http_test token_rotation_crash_before_rename_restarts -- --exact`（new）
- [ ] AC-57：`source .env.test && cargo test --test agent_mcp_http_test token_rotation_crash_after_rename_restarts -- --exact`（new）
- [ ] AC-58：`source .env.test && cargo test --test agent_mcp_http_test token_first_create_crash_before_file_restarts -- --exact`（new）
- [ ] AC-59：`source .env.test && cargo test --test agent_mcp_http_test token_first_create_crash_after_file_restarts -- --exact`（new）
- [ ] AC-60：`source .env.test && cargo test --lib internal::ai::mcp_server::http::tests::token_publish_fsync_order -- --exact`（new）
- [ ] AC-61：`source .env.test && cargo test --lib internal::ai::mcp_server::http::tests::token_rotation_fsync_order -- --exact`（new）
- [ ] AC-62：`source .env.test && cargo test --test agent_mcp_http_test token_state_torn_write_restarts -- --exact`（new）
- [ ] AC-63：`source .env.test && cargo test --test agent_mcp_http_test token_first_create_partial_write_restarts -- --exact`（new）
- [ ] AC-64：`source .env.test && cargo test --test agent_mcp_http_test workbuddy_grokbot_and_bridge_coexist_writes -- --exact`（new）
- [ ] AC-65：`source .env.test && cargo test --test agent_mcp_http_test bridge_unblocked_during_mcp_writes -- --exact`（new）
- [ ] AC-66：`source .env.test && cargo test --test agent_mcp_http_test token_lock_key_for_missing_file -- --exact`（new）
- [ ] AC-67：`source .env.test && cargo test --test agent_mcp_http_test aliased_token_path_shares_lock -- --exact`（new）
- [ ] AC-68：`source .env.test && cargo test --test agent_mcp_http_test different_libra_home_same_token_path_shares_lock -- --exact`（new）
- [ ] AC-69：`source .env.test && cargo test --test agent_mcp_http_test reserved_token_filename_refused -- --exact`（new）
- [ ] AC-70：`source .env.test && cargo test --test agent_mcp_http_test trickled_body_hits_absolute_deadline_408 -- --exact`（new）
- [ ] AC-71：`source .env.test && cargo test --test agent_mcp_http_test stalled_body_does_not_hold_admission -- --exact`（new）
- [ ] AC-72：`source .env.test && cargo test --test agent_mcp_http_test stalled_headers_closed_by_deadline -- --exact`（new）
- [ ] AC-73：`source .env.test && cargo test --test agent_mcp_http_test connection_admission_limit_503 -- --exact`（new）
- [ ] AC-74：`source .env.test && cargo test --test agent_mcp_http_test http2_preface_rejected -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- '--transport http' docs/commands/agent.md && rg -qF -- '--token-out' docs/commands/agent.md && rg -qF -- '--allowed-host' docs/commands/agent.md && rg -qF -- '--allowed-origin' docs/commands/agent.md && rg -qF -- 'Authorization: Bearer' docs/commands/agent.md && rg -qF -- 'Windows' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- '--transport http' docs/commands/zh-CN/agent.md && rg -qF -- '--token-out' docs/commands/zh-CN/agent.md && rg -qF -- '--allowed-host' docs/commands/zh-CN/agent.md && rg -qF -- '--allowed-origin' docs/commands/zh-CN/agent.md && rg -qF -- 'Authorization: Bearer' docs/commands/zh-CN/agent.md && rg -qF -- 'Windows' docs/commands/zh-CN/agent.md`
- [ ] `docs/development/commands/agent.md`：`rg -qF -- '--allowed-host' docs/development/commands/agent.md && rg -qF -- '--allowed-origin' docs/development/commands/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- '--transport http'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- '--transport http' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '--token-out' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '--allowed-host' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '--allowed-origin' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'Authorization: Bearer' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'Windows' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- '--transport http' && printf '%s' "$PAGE" | rg -qF -- '--token-out' && printf '%s' "$PAGE" | rg -qF -- '--allowed-host' && printf '%s' "$PAGE" | rg -qF -- '--allowed-origin' && printf '%s' "$PAGE" | rg -qF -- 'Authorization: Bearer' && printf '%s' "$PAGE" | rg -qF -- 'Windows'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_http_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_http_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-20`（写集串行：agent 文档；消费 MC-19 的读 deadline 与 `libra_test_sleep`）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/command/agent/mcp.rs`、`src/internal/ai/mcp_server/http.rs`（新）、`src/internal/ai/mcp_server/mod.rs`、`src/internal/ai/mcp_server/session.rs`、`tests/agent_mcp_http_test.rs`（新）、`Cargo.toml`（`[[test]]` 行；`hyper`、`hyper-util` 直接依赖）、`Cargo.lock`（工具链生成）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：HTTP 参数（并发上限见 MC-17）、令牌（每次启动由服务生成 256 bit 写入 `--token-out`，0600；已存在的合格文件原子替换、重启轮换；不接受外部令牌）、服务管理器重启与客户端交接流程（TLS 终端持远程凭据、就绪后重载并注入上游令牌）、远程 HTTPS 部署示例（同机 TLS 终端转发到 loopback 端口，`--allowed-host <公网主机名>`）、通用远程 MCP 客户端配置示例（`url` + `Authorization` 头，GC-MC-06）、Windows 不支持说明、EXAMPLES `libra agent mcp --transport http --listen 127.0.0.1:8765 --token-out <路径>`；开发文档写 HTTP 访问控制与准入设计；`COMPATIBILITY.md` 第 252 行追加一句。不受影响：`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** loopback-only（解析期 + 绑定期 + peer）；常数时间比较令牌摘要；令牌不写日志；令牌不来自 query（SB-02）。

**Performance budget:** 请求体 ≤ 256 KiB；临时模式并发 1、无等待队列（忙即 503）；读 deadline 30 s。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=loopback Streamable HTTP 传输与访问控制（legacy 纪元）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=74/8@EX-MC-04; VER=74/8@EX-MC-04; landing=2; prod-files=4; scope=M; deps=MC-20,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-20; release=independent; split-from=N/A; exception=EX-MC-04`

### Task MC-14: `2026-07-28` modern 纪元

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 增加 modern 纪元（ADR-MC-04）：实现 `server/discover`；stdio 上未经 legacy 握手的请求、HTTP 上 `MCP-Protocol-Version` 为 modern 值的请求，按 `_meta` 必需字段（`io.modelcontextprotocol/protocolVersion`、`io.modelcontextprotocol/clientCapabilities`）处理；modern 结果带 `resultType: "complete"` 与 `_meta["io.modelcontextprotocol/serverInfo"]`，`tools/list` 带 `ttlMs` 与 `cacheScope: "private"`；modern 纪元无 `ping`；HTTP 要求 `Mcp-Method`（所有请求）与 `Mcp-Name`（`tools/call`）存在且与请求体一致（`Mcp-Name` 为 Base64 哨兵格式时先解码再比较，解码失败为 400 + `-32020`），所有 modern 结果（含 `server/discover` 与工具错误结果）都带 `resultType: "complete"`，`MCP-Protocol-Version` 与 `_meta` 一致。stdio 的纪元判定改为：`server/discover` 不确定纪元，连接在第一次成功处理非 `server/discover` 的 modern 请求之前仍可用 `initialize` 进入 legacy（支持「先探测、后回退」的客户端）。未握手且缺少必需 `_meta` 的请求按 modern 规则以 `-32602` 拒绝（MC-01 的对应门只断言「是 JSON-RPC 错误」，不受影响）。唯一行为轴：「modern 纪元协议协商」。

**Out of scope:** `subscriptions/listen`、MRTR、tasks 扩展（永久非目标，ADR-MC-03）；新工具。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现行协议版本常量与握手先例（bridge） | `src/internal/ai/agent_bridge/protocol.rs:24-25`（`PROTOCOL_MAJOR/MINOR`）、`:62-82`（allowlist） |
| 现行 agent 子命令入口 | `src/command/agent/mod.rs:232-233`、`:451` |
| MCP 协议核心与 HTTP 传输（MC-01、MC-13 交付；开工时刷新行号） | `src/internal/ai/mcp_server/{protocol,stdio,http}.rs` |
| modern 纪元变更 | 规范 `2026-07-28/changelog.mdx`（pin `1155ca46`）Major 1–9、Minor 4、5、12 |
| `_meta` 必需字段与 `-32602`（HTTP 400） | 规范 `2026-07-28/basic/index.mdx`「Per-request protocol fields」 |
| 版本协商与 `-32022` | 规范 `2026-07-28/basic/versioning.mdx` |
| HTTP 头、`-32020`、未知方法 404 | 规范 `2026-07-28/basic/transports/streamable-http.mdx`「Protocol Version Header」「Standard Request Headers」 |

**Acceptance criteria:**

- [ ] AC-1 `server/discover` 返回的支持版本集合等于 `["2026-07-28","2025-11-25","2025-06-18"]`。
- [ ] AC-2 `server/discover` 的 `capabilities` 等于 `{"tools":{"listChanged":false}}`。
- [ ] AC-3 stdio 上不经 `initialize` 的 modern `tools/list` 返回结果对象（非错误）。
- [ ] AC-4 该结果的 `resultType` 等于 `"complete"`。
- [ ] AC-5 modern `server/discover` 结果的 `resultType` 等于 `"complete"`。
- [ ] AC-6 `server/discover` 结果的 `ttlMs` 等于 `0`。
- [ ] AC-7 `server/discover` 结果的 `cacheScope` 等于 `"private"`。
- [ ] AC-8 `server/discover` 结果通过固定提交 `1155ca46` 的 `schema/2026-07-28/schema.json` 中 `DiscoverResult` 定义的 JSON Schema 校验（`jsonschema` dev-dependency，schema 副本存于测试 fixture）。
- [ ] AC-9 modern `tools/call libra_status` 成功结果的 `resultType` 等于 `"complete"`。
- [ ] AC-10 modern `tools/call` 工具错误结果（带多余参数调用 `libra_status`）的 `resultType` 等于 `"complete"`。
- [ ] AC-11 modern `tools/list` 结果的 `ttlMs` 为非负整数。
- [ ] AC-12 modern `tools/list` 结果的 `cacheScope` 等于 `"private"`。
- [ ] AC-13 该结果的 `_meta` 含 `io.modelcontextprotocol/serverInfo` 键。
- [ ] AC-14 `_meta` 版本为 `1900-01-01` 时得到协议错误 `-32022`。
- [ ] AC-15 该错误的 `data.supported` 等于 `server/discover` 的版本集合。
- [ ] AC-16 该错误的 `data.requested` 等于 `"1900-01-01"`。
- [ ] AC-17 缺少 `clientCapabilities` 的 modern 请求得到协议错误 `-32602`。
- [ ] AC-18 缺少 `protocolVersion` 且连接未经 legacy 握手的请求得到协议错误 `-32602`。
- [ ] AC-19 HTTP 上缺少必需 `_meta` 字段的请求状态码为 400。
- [ ] AC-20 modern `tools/call libra_status` 的 `structuredContent` 等于 legacy 调用的 `structuredContent`。
- [ ] AC-21 legacy 握手后的 `tools/list` 结果不含 `resultType` 键。
- [ ] AC-22 legacy 握手后的 `tools/list` 结果不含 `ttlMs` 键。
- [ ] AC-23 modern `ping` 得到协议错误 `-32601`。
- [ ] AC-24 HTTP 上 modern `ping` 的状态码为 404。
- [ ] AC-25 legacy 握手后的 `ping` 结果等于 `{}`。
- [ ] AC-26 HTTP 上 `MCP-Protocol-Version` 头与 `_meta` 版本不一致时状态码为 400。
- [ ] AC-27 同一请求得到协议错误 `-32020`。
- [ ] AC-28 HTTP 上 `Mcp-Method` 与请求 `method` 不一致时状态码为 400。
- [ ] AC-29 同一请求得到协议错误 `-32020`。
- [ ] AC-30 HTTP 上 `tools/call` 的 `Mcp-Name` 与 `params.name` 不一致时状态码为 400。
- [ ] AC-31 同一请求得到协议错误 `-32020`。
- [ ] AC-32 以 Base64 哨兵格式编码、解码后等于 `params.name` 的 `Mcp-Name` 头调用 `libra_status`，结果的 `structuredContent.status` 等于 `libra --json status` 的 `data`。
- [ ] AC-33 Base64 哨兵格式但内容无法解码的 `Mcp-Name` 头，状态码为 400。
- [ ] AC-34 同一请求得到协议错误 `-32020`。
- [ ] AC-35 HTTP 上 modern 版本头为不支持值时状态码为 400。
- [ ] AC-36 同一请求的响应体为协议错误 `-32022`。
- [ ] AC-37 同一错误的 `data.supported` 等于 `server/discover` 的版本集合。
- [ ] AC-38 同一错误的 `data.requested` 等于请求头中的版本值。
- [ ] AC-39 HTTP 上 `_meta` 声明 `2026-07-28` 但缺少 `MCP-Protocol-Version` 头的请求状态码为 400。
- [ ] AC-40 同一请求得到协议错误 `-32020`。
- [ ] AC-41 同一 stdio 连接先发 `server/discover`、再发 `initialize`（`2025-11-25`）时，`initialize` 的结果 `protocolVersion` 等于 `"2025-11-25"`。
- [ ] AC-42 HTTP 上缺少 `Mcp-Method` 头的 modern 请求状态码为 400。
- [ ] AC-43 同一请求得到协议错误 `-32020`。
- [ ] AC-44 HTTP 上缺少 `Mcp-Name` 头的 modern `tools/call` 状态码为 400。
- [ ] AC-45 同一请求得到协议错误 `-32020`。
- [ ] AC-46 HTTP 上带 `MCP-Protocol-Version: 2026-07-28` 头（含匹配的 `_meta` 与 `Mcp-Method`）的 `initialize` 状态码为 404。
- [ ] AC-47 同一请求的响应体为协议错误 `-32601`。
- [ ] AC-48 HTTP 上 `_meta` 声明 `2026-07-28`、但缺少版本头的 `initialize` 状态码为 400。
- [ ] AC-49 同一请求得到协议错误 `-32020`。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**判据规范（非计数正文）:** 门族（49 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`agent_mcp_modern_test::discover_versions`；AC-2=`agent_mcp_modern_test::discover_capabilities`；AC-3=`agent_mcp_modern_test::modern_tools_list_without_initialize`；AC-4=`agent_mcp_modern_test::modern_result_type_complete`；AC-5=`agent_mcp_modern_test::discover_result_type_complete`；AC-6=`agent_mcp_modern_test::discover_ttl_ms_zero`；AC-7=`agent_mcp_modern_test::discover_cache_scope_private`；AC-8=`agent_mcp_modern_test::discover_result_matches_pinned_schema`；AC-9=`agent_mcp_modern_test::tool_success_result_type_complete`；AC-10=`agent_mcp_modern_test::tool_error_result_type_complete`；AC-11=`agent_mcp_modern_test::modern_tools_list_ttl`；AC-12=`agent_mcp_modern_test::modern_tools_list_cache_scope`；AC-13=`agent_mcp_modern_test::modern_result_server_info`；AC-14=`agent_mcp_modern_test::unsupported_modern_version_is_32022`；AC-15=`agent_mcp_modern_test::unsupported_version_lists_supported`；AC-16=`agent_mcp_modern_test::unsupported_version_echoes_requested`；AC-17=`agent_mcp_modern_test::missing_client_capabilities_is_32602`；AC-18=`agent_mcp_modern_test::missing_protocol_version_is_32602`；AC-19=`agent_mcp_modern_test::http_missing_meta_is_400`；AC-20=`agent_mcp_modern_test::modern_and_legacy_results_match`；AC-21=`agent_mcp_modern_test::legacy_results_have_no_result_type`；AC-22=`agent_mcp_modern_test::legacy_results_have_no_ttl`；AC-23=`agent_mcp_modern_test::modern_ping_is_method_not_found`；AC-24=`agent_mcp_modern_test::http_modern_ping_is_404`；AC-25=`agent_mcp_modern_test::legacy_ping_still_works`；AC-26=`agent_mcp_modern_test::http_version_header_mismatch_is_400`；AC-27=`agent_mcp_modern_test::http_version_header_mismatch_is_32020`；AC-28=`agent_mcp_modern_test::http_method_header_mismatch_is_400`；AC-29=`agent_mcp_modern_test::http_method_header_mismatch_is_32020`；AC-30=`agent_mcp_modern_test::http_name_header_mismatch_is_400`；AC-31=`agent_mcp_modern_test::http_name_header_mismatch_is_32020`；AC-32=`agent_mcp_modern_test::http_encoded_name_header_executes_tool`；AC-33=`agent_mcp_modern_test::http_malformed_encoded_name_header_is_400`；AC-34=`agent_mcp_modern_test::http_malformed_encoded_name_header_is_32020`；AC-35=`agent_mcp_modern_test::http_unsupported_modern_version_is_400`；AC-36=`agent_mcp_modern_test::http_unsupported_version_is_32022`；AC-37=`agent_mcp_modern_test::http_unsupported_version_lists_supported`；AC-38=`agent_mcp_modern_test::http_unsupported_version_echoes_requested`；AC-39=`agent_mcp_modern_test::http_modern_missing_version_header_is_400`；AC-40=`agent_mcp_modern_test::http_modern_missing_version_header_is_32020`；AC-41=`agent_mcp_modern_test::discover_then_initialize_falls_back_to_legacy`；AC-42=`agent_mcp_modern_test::http_missing_method_header_is_400`；AC-43=`agent_mcp_modern_test::http_missing_method_header_is_32020`；AC-44=`agent_mcp_modern_test::http_missing_name_header_is_400`；AC-45=`agent_mcp_modern_test::http_missing_name_header_is_32020`；AC-46=`agent_mcp_modern_test::http_modern_initialize_is_404`；AC-47=`agent_mcp_modern_test::http_modern_initialize_is_method_not_found`；AC-48=`agent_mcp_modern_test::http_modern_initialize_missing_header_400`；AC-49=`agent_mcp_modern_test::http_modern_initialize_missing_header_32020`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_modern_test discover_versions -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_modern_test discover_capabilities -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_modern_test modern_tools_list_without_initialize -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_modern_test modern_result_type_complete -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_modern_test discover_result_type_complete -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_modern_test discover_ttl_ms_zero -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_modern_test discover_cache_scope_private -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_modern_test discover_result_matches_pinned_schema -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_mcp_modern_test tool_success_result_type_complete -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_mcp_modern_test tool_error_result_type_complete -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_mcp_modern_test modern_tools_list_ttl -- --exact`（new）
- [ ] AC-12：`source .env.test && cargo test --test agent_mcp_modern_test modern_tools_list_cache_scope -- --exact`（new）
- [ ] AC-13：`source .env.test && cargo test --test agent_mcp_modern_test modern_result_server_info -- --exact`（new）
- [ ] AC-14：`source .env.test && cargo test --test agent_mcp_modern_test unsupported_modern_version_is_32022 -- --exact`（new）
- [ ] AC-15：`source .env.test && cargo test --test agent_mcp_modern_test unsupported_version_lists_supported -- --exact`（new）
- [ ] AC-16：`source .env.test && cargo test --test agent_mcp_modern_test unsupported_version_echoes_requested -- --exact`（new）
- [ ] AC-17：`source .env.test && cargo test --test agent_mcp_modern_test missing_client_capabilities_is_32602 -- --exact`（new）
- [ ] AC-18：`source .env.test && cargo test --test agent_mcp_modern_test missing_protocol_version_is_32602 -- --exact`（new）
- [ ] AC-19：`source .env.test && cargo test --test agent_mcp_modern_test http_missing_meta_is_400 -- --exact`（new）
- [ ] AC-20：`source .env.test && cargo test --test agent_mcp_modern_test modern_and_legacy_results_match -- --exact`（new）
- [ ] AC-21：`source .env.test && cargo test --test agent_mcp_modern_test legacy_results_have_no_result_type -- --exact`（new）
- [ ] AC-22：`source .env.test && cargo test --test agent_mcp_modern_test legacy_results_have_no_ttl -- --exact`（new）
- [ ] AC-23：`source .env.test && cargo test --test agent_mcp_modern_test modern_ping_is_method_not_found -- --exact`（new）
- [ ] AC-24：`source .env.test && cargo test --test agent_mcp_modern_test http_modern_ping_is_404 -- --exact`（new）
- [ ] AC-25：`source .env.test && cargo test --test agent_mcp_modern_test legacy_ping_still_works -- --exact`（new）
- [ ] AC-26：`source .env.test && cargo test --test agent_mcp_modern_test http_version_header_mismatch_is_400 -- --exact`（new）
- [ ] AC-27：`source .env.test && cargo test --test agent_mcp_modern_test http_version_header_mismatch_is_32020 -- --exact`（new）
- [ ] AC-28：`source .env.test && cargo test --test agent_mcp_modern_test http_method_header_mismatch_is_400 -- --exact`（new）
- [ ] AC-29：`source .env.test && cargo test --test agent_mcp_modern_test http_method_header_mismatch_is_32020 -- --exact`（new）
- [ ] AC-30：`source .env.test && cargo test --test agent_mcp_modern_test http_name_header_mismatch_is_400 -- --exact`（new）
- [ ] AC-31：`source .env.test && cargo test --test agent_mcp_modern_test http_name_header_mismatch_is_32020 -- --exact`（new）
- [ ] AC-32：`source .env.test && cargo test --test agent_mcp_modern_test http_encoded_name_header_executes_tool -- --exact`（new）
- [ ] AC-33：`source .env.test && cargo test --test agent_mcp_modern_test http_malformed_encoded_name_header_is_400 -- --exact`（new）
- [ ] AC-34：`source .env.test && cargo test --test agent_mcp_modern_test http_malformed_encoded_name_header_is_32020 -- --exact`（new）
- [ ] AC-35：`source .env.test && cargo test --test agent_mcp_modern_test http_unsupported_modern_version_is_400 -- --exact`（new）
- [ ] AC-36：`source .env.test && cargo test --test agent_mcp_modern_test http_unsupported_version_is_32022 -- --exact`（new）
- [ ] AC-37：`source .env.test && cargo test --test agent_mcp_modern_test http_unsupported_version_lists_supported -- --exact`（new）
- [ ] AC-38：`source .env.test && cargo test --test agent_mcp_modern_test http_unsupported_version_echoes_requested -- --exact`（new）
- [ ] AC-39：`source .env.test && cargo test --test agent_mcp_modern_test http_modern_missing_version_header_is_400 -- --exact`（new）
- [ ] AC-40：`source .env.test && cargo test --test agent_mcp_modern_test http_modern_missing_version_header_is_32020 -- --exact`（new）
- [ ] AC-41：`source .env.test && cargo test --test agent_mcp_modern_test discover_then_initialize_falls_back_to_legacy -- --exact`（new）
- [ ] AC-42：`source .env.test && cargo test --test agent_mcp_modern_test http_missing_method_header_is_400 -- --exact`（new）
- [ ] AC-43：`source .env.test && cargo test --test agent_mcp_modern_test http_missing_method_header_is_32020 -- --exact`（new）
- [ ] AC-44：`source .env.test && cargo test --test agent_mcp_modern_test http_missing_name_header_is_400 -- --exact`（new）
- [ ] AC-45：`source .env.test && cargo test --test agent_mcp_modern_test http_missing_name_header_is_32020 -- --exact`（new）
- [ ] AC-46：`source .env.test && cargo test --test agent_mcp_modern_test http_modern_initialize_is_404 -- --exact`（new）
- [ ] AC-47：`source .env.test && cargo test --test agent_mcp_modern_test http_modern_initialize_is_method_not_found -- --exact`（new）
- [ ] AC-48：`source .env.test && cargo test --test agent_mcp_modern_test http_modern_initialize_missing_header_400 -- --exact`（new）
- [ ] AC-49：`source .env.test && cargo test --test agent_mcp_modern_test http_modern_initialize_missing_header_32020 -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- '2026-07-28' docs/commands/agent.md && rg -qF -- 'server/discover' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- '2026-07-28' docs/commands/zh-CN/agent.md && rg -qF -- 'server/discover' docs/commands/zh-CN/agent.md`
- [ ] `docs/development/commands/agent.md`：`rg -qF -- 'server/discover' docs/development/commands/agent.md && rg -qF -- 'MCP-Protocol-Version' docs/development/commands/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- '2026-07-28'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- '2026-07-28' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'server/discover' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- '2026-07-28' && printf '%s' "$PAGE" | rg -qF -- 'server/discover'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_modern_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_modern_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-17`（写集串行：`http.rs`；消费 MC-13 的 HTTP 传输）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/protocol.rs`、`src/internal/ai/mcp_server/stdio.rs`、`src/internal/ai/mcp_server/http.rs`、`src/internal/ai/mcp_server/modern.rs`（新）、`tests/agent_mcp_modern_test.rs`（新）、`tests/fixtures/mcp/schema-2026-07-28.json`（新，固定提交 `1155ca460e5097019f32769b38dcba194432e2fb` 的副本）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：协议版本表新增 `2026-07-28` 与纪元路由；开发文档写纪元判定规则；`COMPATIBILITY.md` 第 252 行追加一句。不受影响：`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 版本与能力只来自请求，不改变授权；clientInfo 不参与授权。

**Performance budget:** 每请求 O(1) 的 `_meta`/头校验。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=2026-07-28 modern 纪元协议协商; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=49/8@EX-MC-05; VER=49/8@EX-MC-05; landing=1; prod-files=4; scope=M; deps=MC-17,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-17; release=independent; split-from=N/A; exception=EX-MC-05`

### Task MC-15: WorkBuddy 与 Grok Bot 真实客户端联调验收

**Task type:** `audit`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 用已发布的 MC-14 版本，在用户提供的环境中实际验证：WorkBuddy 以 stdio 配置拉起 `libra agent mcp --stdio` 并调用工具；Grok Bot 经运维方 HTTPS 终端访问 `libra agent mcp --transport http` 并调用工具。两者都只连接**一次性探针仓库**（不连接用户的工作仓库），探针仓库在联调后删除；本卡不修改本仓库的任何 ref 或工作树。只记录脱敏证据到本计划「实施证据汇总」；发现缺陷时按 ER-10 新建 `FIX-MC-*` 卡，不在本卡改代码。唯一行为轴：「需求方客户端的接入验收」。

**Out of scope:** 代码改动（由 `FIX-MC-*` 承接）；其它客户端（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 环境依赖 | DEP-MC-04、DEP-MC-05 |
| 现行 agent 子命令注册与文档（MCP 小节由 MC-01、MC-13 加入，开工时刷新行号） | `src/command/agent/mod.rs:232-233`；`docs/commands/agent.md:324` |
| 探针仓库初始化所用命令 | `libra init`（`src/command/init.rs`） |

**Acceptance criteria:**

- [ ] AC-1 WorkBuddy 的 MCP 服务器配置（`command`/`args`/`cwd`，脱敏）已写入证据段。
- [ ] AC-2 以 `NAME=wb` 执行下方脚本，以 `mktemp -d` 创建唯一的 WorkBuddy 探针仓库（含随机标记文件），其当前分支为随机探针名。
- [ ] AC-3 WorkBuddy（`cwd` 指向该探针目录，提示词不含分支名）调用 `libra_status` 的返回内容（脱敏截取，写入证据）含该探针分支名。
- [ ] AC-4 WorkBuddy 会话打开时，恰有一个祖先进程命令行含 WorkBuddy 特征的 `libra agent mcp --stdio` 进程（其 PID 与启动时间被记录）。
- [ ] AC-5 在 WorkBuddy 中关闭该 MCP 会话后，上一门写入证据段的进程（以 PID + 启动时间识别，执行时以环境变量显式传入）已不存在。
- [ ] AC-6 Grok Bot 的远程 MCP 配置（URL 主机名与头名，令牌遮蔽）已写入证据段。
- [ ] AC-7 以 `NAME=grokbot` 执行同一脚本并在该目录启动 HTTP 服务后，Grok Bot 对话（提示词只要求「调用 libra_status 并报告分支」，不含分支名）返回的结果含该探针分支名。
- [ ] AC-8 该 HTTP 服务进程（只在 grok 探针目录中启动，因而探针分支名只能经由它到达 Grok Bot）在联调窗口内记录的 `method = tools/call`、`tool = libra_status` 的 `agent.mcp.request` 至少一条，且其中至少一条与 AC-7 的观测时间相关联（客户端重试次数另行记入证据，不作为失败条件）。
- [ ] AC-9 联调结束后，标记文件内容与证据一致的两个探针目录已删除。
- [ ] AC-10 对 HTTPS 端点不带令牌的 POST 状态码为 401。
- [ ] AC-11 故障注入：在 `PATH` 前置一个打印证据中出现的文本后以 1 退出的 `libra` 替身，执行 AC-3 的判定命令，该命令以非 0 结束（查询失败即使打印了可匹配的文本也不会通过）。
- [ ] AC-12 故障注入：对一个标记文件不可读（`chmod 000`）的临时目录执行 AC-9 的标记核对逻辑时，该目录未被删除（核对以非 0 结束是其前提）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：手工证据：WorkBuddy 配置截取
- [ ] AC-2：在执行 MC-15 的同一交互 shell 会话中以 `.`（source）执行下列脚本（不得以子进程 `bash` 运行，探针状态只留在该会话的环境变量中，不写任何状态文件），返回 0 为通过：

  ```bash
  set -euo pipefail
  NAME="${NAME:?set NAME=wb or NAME=grokbot}"
  DIR="$(mktemp -d "${TMPDIR:-/tmp}/libra-mcp-probe-$NAME.XXXXXX")"
  MARK="$(openssl rand -hex 16)"
  printf '%s\n' "$MARK" > "$DIR/.libra-mcp-probe-marker"
  PROBE="mcp-probe-$(openssl rand -hex 8)"
  (cd "$DIR" && libra init >/dev/null && libra commit --allow-empty -m probe >/dev/null && libra switch -c "$PROBE" >/dev/null)
  CUR="$(cd "$DIR" && libra branch --show-current)" && test -n "$CUR" && test "$CUR" = "$PROBE"
  UP="$(printf '%s' "$NAME" | tr '[:lower:]' '[:upper:]')"
  export "${UP}_DIR=$DIR" "${UP}_MARK=$MARK"   # 绝对路径与标记只存在于当前 shell 会话的环境变量中，不写任何文件、不写入计划
  echo "probe name=$NAME marker_sha256=$(printf '%s' "$MARK" | shasum -a 256 | cut -c1-12) branch=$PROBE"   # 只把这一行写入证据段（ER-11：不含绝对路径）；不得把分支名写进任何提示词
  ```
- [ ] AC-3：`B="$(cd "$WB_DIR" && libra branch --show-current)" && test -n "$B" && printf '%s' "$WB_EXCERPT" | grep -qF -- "$B"`（在同一 shell 会话中执行；分支名查询以 `&&` 串联，查询失败——即使它打印了内容——或结果为空都不通过；`WB_EXCERPT` 取自证据段，`WB_DIR` 只来自该会话的环境变量）
- [ ] AC-4：以 `bash` 执行下列脚本，退出码 0 为通过：

  ```bash
  set -euo pipefail
  WB_PATTERN="${WB_PATTERN:-WorkBuddy}"   # WorkBuddy 主进程命令行特征，实际值写入证据
  MCP_PID=""
  for P in $(pgrep -f 'agent mcp --stdio' || true); do
    A=$P
    while [ "$A" -gt 1 ]; do
      A=$(ps -o ppid= -p "$A" | tr -d ' ')
      if ps -o command= -p "$A" | grep -qF "$WB_PATTERN"; then MCP_PID="$P${MCP_PID:+ $MCP_PID}"; break; fi
    done
  done
  test "$(printf '%s\n' $MCP_PID | grep -c .)" -eq 1 || { echo "FAIL: expected exactly one WorkBuddy-owned MCP child"; exit 1; }
  MCP_START="$(ps -o lstart= -p "$MCP_PID")"
  export MCP_PID MCP_START
  echo "recorded pid=$MCP_PID start=$MCP_START"   # 两个值逐字写入证据段，供下一门显式传入
  ```
- [ ] AC-5：以 `bash` 执行下列脚本，退出码 0 为通过：

  ```bash
  set -euo pipefail
  # 以 MCP_PID=<证据段记录的 PID> MCP_START="<证据段记录的启动时间>" bash <本脚本> 执行
  test -n "${MCP_PID:-}" && test -n "${MCP_START:-}"
  # 先区分「确认不存在」与「查询失败」：kill -0 的 ESRCH 才算不存在；其它失败一律不通过
  if kill -0 "$MCP_PID" 2>/dev/null; then
    START="$(ps -o lstart= -p "$MCP_PID")" && test -n "$START" || { echo "FAIL: cannot inspect live PID $MCP_PID"; exit 1; }
    if [ "$START" = "$MCP_START" ]; then echo "FAIL: MCP child still running"; exit 1; fi
    echo "OK: PID reused by another process"
  else
    ERR="$(kill -0 "$MCP_PID" 2>&1 || true)"
    printf '%s' "$ERR" | grep -qi 'no such process' || { echo "FAIL: process query failed: $ERR"; exit 1; }
    echo OK
  fi
  ```
- [ ] AC-6：手工证据：Grok Bot 配置截取
- [ ] AC-7：`B="$(cd "$GROKBOT_DIR" && libra branch --show-current)" && test -n "$B" && printf '%s' "$GROKBOT_EXCERPT" | grep -qF -- "$B"`（在同一 shell 会话中执行；分支名查询以 `&&` 串联，查询失败——即使它打印了内容——或结果为空都不通过；`GROKBOT_EXCERPT` 取自证据段，`GROKBOT_DIR` 只来自该会话的环境变量；提示词原文同时写入证据）
- [ ] AC-8：手工证据：服务端日志截取（含时间戳、`tool` 字段）
- [ ] AC-9：在同一 shell 会话中以 `.`（source）执行下列脚本，返回 0 为通过：

  ```bash
  set -euo pipefail
  for PAIR in "$WB_DIR:$WB_MARK" "$GROKBOT_DIR:$GROKBOT_MARK"; do
    D="${PAIR%%:*}"; M="${PAIR##*:}"
    MK="$(cat "$D/.libra-mcp-probe-marker")" && test -n "$MK" && test "$MK" = "$M" || { echo "FAIL: marker unreadable or mismatch in $D"; exit 1; }   # 先赋值并检查退出码，读取失败绝不删除
    rm -rf -- "$D"
    test ! -e "$D"
  done
  unset WB_DIR WB_MARK GROKBOT_DIR GROKBOT_MARK
  echo OK
  ```
- [ ] AC-10：`CODE="$(curl -sS -m 20 -o /dev/null -w '%{http_code}' -X POST "https://$MCP_PUBLIC_HOST/mcp")" && test "$CODE" = 401`（`curl` 失败或超时即不通过；不带 `-f`，401 时 `curl` 以 0 退出）（`MCP_PUBLIC_HOST` 取自同卡证据段记录的公网主机名）
- [ ] AC-11：以 `bash` 在同一 shell 会话中执行下列脚本，退出码 0 为通过：

  ```bash
  set -u
  D="$(mktemp -d)"
  printf '#!/bin/sh\nprintf "%%s" "$MOCK_OUT"\nexit 1\n' > "$D/libra"; chmod +x "$D/libra"
  MOCK_OUT="$(printf '%s' "$WB_EXCERPT" | head -c 16)"; export MOCK_OUT
  test -n "$MOCK_OUT"
  if (PATH="$D:$PATH"; B="$(cd "$WB_DIR" && libra branch --show-current)" && test -n "$B" && printf '%s' "$WB_EXCERPT" | grep -qF -- "$B"); then RC=1; else RC=0; fi
  rm -rf "$D"
  test "$RC" -eq 0
  ```
- [ ] AC-12：以 `bash` 执行下列脚本，退出码 0 为通过：

  ```bash
  set -u
  D="$(mktemp -d)"; M="$(openssl rand -hex 16)"; printf '%s\n' "$M" > "$D/.libra-mcp-probe-marker"; chmod 000 "$D/.libra-mcp-probe-marker"
  if (MK="$(cat "$D/.libra-mcp-probe-marker")" && test -n "$MK" && test "$MK" = "$M"); then RC=1; else RC=0; fi
  test -d "$D" || RC=1
  chmod 600 "$D/.libra-mcp-probe-marker"; rm -rf "$D"
  test "$RC" -eq 0
  ```

**Full-suite trigger:** `N/A（no-release）`

**Dependencies:** `MC-14`（已发布版本）；`DEP-MC-04`；`DEP-MC-05`

**Deliverables:** 本计划「实施证据汇总」中的 MC-15 小节（唯一产物）

**Implementation write set:** `docs/development/plan/plan-20261019-agent-mcp-server.md`、`docs/development/plan/plan-status.md`

**Release write set:** `N/A`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——只记录验收证据，不改用户文档、兼容矩阵或命令行为。

**Rollback mode:** `revert`

**Migration and rollback:** `N/A`

**Security and privacy:** 证据遮蔽令牌、主机内部地址与账号标识（ER-11）。

**Performance budget:** N/A

**Estimated scope:** `S`

**Version increment:** `N/A（产物随 MC-14 发布的版本验证，不进入用户渠道）`

**Release boundary:** `no-release`

**C/D coverage from:** `CLOSE-MC`

**Granularity:** `type=audit; axis=需求方客户端（WorkBuddy stdio、Grok Bot 远程 HTTPS）的接入验收; recovery=revert 证据段; complete=yes; self-contained=yes; AC=12/20; VER=12/20; landing=0; prod-files=0; scope=S; deps=MC-14,DEP-MC-04,DEP-MC-05; writeset=序列化于 MC-14; release=no-release; split-from=N/A; exception=N/A`

### Task MC-16: MCP 结果信封：脱敏

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 自 MC-01 拆出。新增 GC-MC-03 ① 的唯一结果信封 `mcp_server/output.rs`：所有工具结果在返回前序列化后经 `review::redact_untrusted` 脱敏、重解析并清洗控制字符（失败为工具错误 `LBR-AGENT-034`，不回退原文）。大小上限与读 deadline 由 MC-19 承接。唯一行为轴：「MCP 工具结果脱敏」。

**Out of scope:** 大小上限与读 deadline（由 MC-19 承接）；生产工具（由 MC-02 起承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 脱敏函数 | `src/internal/ai/agent_bridge/redaction.rs:20-53`（`review::redact_untrusted`、`scrub_controls`） |
| 协议核心与 `libra_test_echo`（MC-01 交付） | `src/internal/ai/mcp_server/{protocol,stdio}.rs` |
| 本地门 | `0.30.70`（tag `474b034`）把 `content[0].text` 留成脱敏前的 JSON。前滚修复 `0.30.71`（`d1c28a4`）：先清洗 `structuredContent`，再 `redact_untrusted`、重解析、再清洗，并重建文本副本。`envelope_redacts_secret` 与 `envelope_scrubs_control_chars` 均为 `test result: ok. 1 passed; 0 failed`。clippy `-D warnings` exit 0。nextest：`Summary [ 568.129s] 10661 tests run: 10661 passed (11 slow, 1 flaky), 21 skipped`，exit 0。 |
| D 组 | tag `v0.30.71` 指向 `d1c28a452a298d0fce592bffd2c57a4f3cad2a63`。`release.yml` [39488255544](https://github.com/libra-tools/libra/actions/runs/39488255544) success。四个平台产物 HTTP 200。安装脚本 `DEFAULT_VERSION="v0.30.71"`。同一提交上 `base.yml` [39488106338](https://github.com/libra-tools/libra/actions/runs/39488106338) success（八个 job 全 success）与 `codeql.yml` [39488106268](https://github.com/libra-tools/libra/actions/runs/39488106268) success。`v0.30.70` 不改标签。 |
| 网站 | `cf` `519c03902a34c3dac5350df19585e816fe31f6d0` 只改 `agent.en.md`。源文件与 `https://libra.tools/en/docs/commands/agent` 都含 `LBR-AGENT-034` 与 `redact`。Claude Code：`CLAUDE_VERDICT: PASS`，`P0=0`，`P1=0`。 |
| Codex | 对 `d1c28a4`：`VERDICT: PASS`，`P0=0`，`P1=0`。对 `474b034` 的评审是 `VERDICT: FAIL`，`P0=0`，`P1=1`（未写出 finding）；缺陷由 `0.30.71` 修复。 |

**Acceptance criteria:**

- [x] AC-1 `libra_test_echo` 返回含 fixture secret（与 `agent_bridge_redaction_test` 同类的 token）的对象时，响应全文不含该 token 字面量。
- [x] AC-2 `libra_test_echo` 返回含 ESC（`\u001b`）字符的字符串时，响应全文不含 ESC 字符。
- [x] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [x] AC-1：`source .env.test && cargo test --test agent_mcp_envelope_test envelope_redacts_secret -- --exact`（new）
- [x] AC-2：`source .env.test && cargo test --test agent_mcp_envelope_test envelope_scrubs_control_chars -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [x] `docs/commands/agent.md`：`rg -qF -- 'LBR-AGENT-034' docs/commands/agent.md && rg -qF -- 'redact' docs/commands/agent.md`
- [x] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'LBR-AGENT-034' docs/commands/zh-CN/agent.md && rg -qF -- 'redact' docs/commands/zh-CN/agent.md`
- [x] `docs/development/commands/agent.md`：`rg -qF -- 'output.rs' docs/development/commands/agent.md && rg -qF -- 'redact_untrusted' docs/development/commands/agent.md`
- [x] 网站源文件 `agent`：`rg -qF -- 'LBR-AGENT-034' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'redact' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'LBR-AGENT-034' && printf '%s' "$PAGE" | rg -qF -- 'redact'`（GC-MC-05 ⑥）
- [x] 测试索引：`rg -q '^\| `agent_mcp_envelope_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_envelope_test"' Cargo.toml`
- [x] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）。Codex：`VERDICT: PASS`，`P0=0`，`P1=0`。Claude Code：`CLAUDE_VERDICT: PASS`，`P0=0`，`P1=0`。

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-22`（写集串行：`stdio.rs`；消费 MC-01 的协议核心与 `libra_test_echo`）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/output.rs`（新）、`src/internal/ai/mcp_server/stdio.rs`、`tests/agent_mcp_envelope_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：「MCP server」小节新增结果脱敏说明与失败时的工具错误 `LBR-AGENT-034`；开发文档写信封设计（GC-MC-03 ①）；`COMPATIBILITY.md`：N/A（MC-01 已登记 `agent mcp` 表面，本卡不新增公开工具或参数）。不受影响：`COMPATIBILITY.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** GC-MC-03 ①；不回退原文。

**Performance budget:** 脱敏为 O(结果大小)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP 工具结果脱敏; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=MC-22,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-22; release=independent; split-from=MC-01; exception=N/A`

### Task MC-17: HTTP 并发准入、过载与写串行

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-13 拆出。连接先经 MC-13 的连接准入（≤ 64 个连接），请求体在其 10 s 绝对期限内完整收到之后才进入执行准入（停滞的请求体不占执行槽位）；准入与限流的顺序：请求先经并发/队列准入（超出即 503，不消耗令牌），**获得执行槽位时**才向 MC-21 的令牌桶取令牌（取不到即 429，不执行工具）。把 MC-13 的临时模式（并发 1、忙即 503）提升为（同卡从 `tests/agent_mcp_http_test.rs` 移除 `interim_mode_rejects_when_busy`、`interim_mode_rejects_without_waiting`、`interim_mode_busy_has_retry_after` 三个临时用例）：读请求并发上限 16、等待队列上限 64（超出立即返回 503 + `Retry-After: 1`），写工具在进程内串行（并沿用 MC-06 的 MCP 仓库写锁）。唯一行为轴：「HTTP 并发准入与过载」。

**Out of scope:** 访问控制与协议头（MC-13 已交付）；modern 纪元（由 MC-14 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有 axum 服务的监听与路由先例 | `src/command/service.rs:678`（`TcpListener::bind`）、`:693`（`Router::new`）、`:733`（`axum::serve`） |
| HTTP 传输与临时串行模式（MC-13 交付；开工时刷新行号） | `src/internal/ai/mcp_server/http.rs` |
| 测试工具 `libra_test_sleep`（MC-19 交付） | `src/internal/ai/mcp_server/tools/mod.rs` |

**Acceptance criteria:**

- [ ] AC-1 `LIBRA_TEST=1` 且限流桶设为测试上限（debug-only 覆盖为容量 1000）时，16 个并发 `libra_test_sleep` 占满后再发 64 个请求，这 64 个最终状态码均为 200。
- [ ] AC-2 同一负载下第 81 个请求状态码为 503。
- [ ] AC-3 同一 503 响应带 `Retry-After: 1` 头。
- [ ] AC-4 `--allow-write` 下两个并发 `libra_commit`（同一 `expected_head`、`allow_empty = true`、不同 `operation_id`）中恰有一个成功。
- [ ] AC-5 另一个得到工具错误 `LBR-AGENT-038`。
- [ ] AC-6 默认限流（容量 20）下 16 个并发 `libra_test_sleep` 占满后再发 64 个请求，非 200 响应的状态码集合是 `{429, 503}` 的子集。
- [ ] AC-7 同一负载下 429 的数量不少于 1（排队请求在取得槽位时才消耗令牌）。
- [ ] AC-8 `tests/agent_mcp_http_test.rs` 中不再存在名称以 `interim_mode_` 开头的用例。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_http_admission_test queued_reads_complete -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_http_admission_test excess_read_is_503 -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_http_admission_test excess_read_has_retry_after -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_http_admission_test concurrent_commits_one_succeeds -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_http_admission_test concurrent_commits_other_is_stale -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_http_admission_test queued_rejections_are_429_or_503 -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_http_admission_test queued_requests_take_tokens_on_execution -- --exact`（new）
- [ ] AC-8：按「Verification 判定口径」零命中模板：`if rg -n 'fn interim_mode_' tests/agent_mcp_http_test.rs; then echo FAIL; exit 1; else rc=$?; if [ "$rc" -ne 1 ]; then exit "$rc"; fi; echo OK; fi`

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- '503' docs/commands/agent.md && rg -qF -- 'Retry-After' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- '503' docs/commands/zh-CN/agent.md && rg -qF -- 'Retry-After' docs/commands/zh-CN/agent.md`
- [ ] `docs/development/commands/agent.md`：`rg -qF -- 'Retry-After' docs/development/commands/agent.md && rg -qF -- '64' docs/development/commands/agent.md`
- [ ] 网站源文件 `agent`：`rg -qF -- '503' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'Retry-After' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- '503' && printf '%s' "$PAGE" | rg -qF -- 'Retry-After'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_http_admission_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_http_admission_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-13`（消费 HTTP 传输）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/http.rs`、`src/internal/ai/mcp_server/rate.rs`（新增仅在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时生效的桶容量覆盖 `LIBRA_MCP_TEST_RATE_CAPACITY`）、`tests/agent_mcp_http_admission_test.rs`（新）、`tests/agent_mcp_http_test.rs`（移除三个临时模式用例）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：HTTP 小节新增并发上限、等待队列与 503/`Retry-After` 行为；开发文档写准入设计；`COMPATIBILITY.md`：N/A（不新增公开参数）。不受影响：`COMPATIBILITY.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 过载时快速拒绝，不无界排队。

**Performance budget:** 并发读 16、等待队列 64、超出 503；写串行。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=HTTP 传输的并发准入与过载行为; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=2; scope=M; deps=MC-13,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-13; release=independent; split-from=MC-13; exception=N/A`

### Task MC-18: commit 的原子 HEAD 前提（内部 CAS）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 为 `CommitArgs` 增加内部字段 `expected_parent: Option<String>`（`#[arg(skip)]`，CLI 不暴露、缺省 `None`）。非 `None` 时，`update_head` 在写 ref 的同一 SQLite 事务内读取当前分支（或 detached HEAD）提交并与之比较，不等则返回提交错误且不写 ref、不写 reflog；`None` 时行为不变。新增 `vcs::commit_create_fenced(message, allow_empty, expected_head)`，把该错误映射为 `BridgeError::stale_fence`（`LBR-AGENT-038`）。既有 `vcs::commit_create`（Harness 路径）不变。唯一行为轴：「commit 的原子 HEAD 前提」。

**Out of scope:** `libra commit` 的用户可见参数（永久非目标：本字段只供内部调用者使用）；restore 的原子互斥（DEFER-MC-07）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| commit 的 HEAD 更新在 reflog 事务内执行 | `src/command/commit.rs:3440-3500`（`update_head`、`update_head_and_reflog` → `with_reflog`） |
| `CommitArgs` 定义 | `src/command/commit.rs:94` |
| bridge commit 入口 | `src/internal/ai/agent_bridge/vcs.rs:404-458` |
| update-ref 的 CAS 先例 | `src/command/update_ref.rs:205-240` |

**Acceptance criteria:**

- [ ] AC-1 `expected_parent` 等于当前分支提交时，调用后新 HEAD 的父提交等于该 `expected_parent`。
- [ ] AC-2 `expected_parent` 不等于当前分支提交时返回错误。
- [ ] AC-3 detached HEAD 且 `expected_parent` 不匹配时返回错误。
- [ ] AC-4 `vcs::commit_create_fenced` 在不匹配时返回 `LBR-AGENT-038`。
- [ ] AC-5 `libra commit --help` 的输出不含 `expected` 字样（字段未暴露为 CLI 参数）。
- [ ] AC-6 竞态门：测试进程内调用 `vcs::commit_create_fenced`，令其在提交对象写出之后、ref 写入事务之前于 `LIBRA_TEST=1` 暂停点（等待测试创建的标记文件）停住，期间另一进程执行 `libra commit --allow-empty -m race`，放行后该调用返回 `LBR-AGENT-038`。
- [ ] AC-7 同一竞态后分支 ref 等于另一进程创建的提交。
- [ ] AC-8 同一竞态后 HEAD reflog 只新增另一进程的一条记录。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib command::commit::tests::expected_parent_match_commits -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --lib command::commit::tests::expected_parent_mismatch_errors -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib command::commit::tests::expected_parent_mismatch_detached -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --lib internal::ai::agent_bridge::vcs::tests::commit_create_fenced_maps_stale -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test command_test command::commit_test::commit_help_hides_expected_parent -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test commit_fenced_race_test fenced_commit_loses_race_with_error -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test commit_fenced_race_test fenced_commit_race_keeps_other_ref -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test commit_fenced_race_test fenced_commit_race_single_reflog_entry -- --exact`（new）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 commit 核心（CLI 与 MCP 共用 `run_commit`）及 Cargo.toml [[test]] 行`

**Dependencies:** `MC-07`（写集串行：发布窗口顺序）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/commit.rs`、`src/internal/ai/agent_bridge/vcs.rs`、`tests/command/commit_test.rs`、`tests/commit_fenced_race_test.rs`（新；三个用例以 `#[serial(cwd)]` 串行）、`tests/SERIAL_REGISTRY.tsv`（新增三行）、`.config/nextest.toml`（`sh tests/NEXTEST_GROUPS.sh` 重新生成）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——`expected_parent` 是 `#[arg(skip)]` 内部字段，`libra commit` 的参数、输出与错误不变（AC-7 守卫）；`COMPATIBILITY.md`、`docs/commands/commit.md`、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** CAS 在 ref 写入事务内执行，fence 不再存在检查与写入之间的窗口。

**Performance budget:** 每次提交多一次事务内 ref 读取，O(1)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=commit 在 HEAD 更新事务内校验期望父提交（内部 CAS，CLI 不暴露）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=2; scope=M; deps=MC-07,DEP-MC-03; writeset=序列化于 MC-07; release=independent; split-from=N/A; exception=N/A`

### Task MC-19: MCP 结果大小上限与读 deadline

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 自 MC-16 拆出。在结果信封中加入 GC-MC-03 ②③：以**完整序列化的 JSON-RPC 响应**（含 `structuredContent` 与 `content[0].text` 两份内容）计量，超过 256 KiB 时为工具错误 `LBR-AGENT-028`（可截断文本字段的截断由使用方声明，见 MC-05，截断后仍以完整响应计量）；读工具 30 s deadline，超时为工具错误 `LBR-AGENT-036`（`retryable: true`）。另注册仅在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时存在的 `libra_test_sleep`（休眠 `ms` 毫秒）与 `libra_test_blob`（服务端生成 `bytes` 字节的字符串字段，请求本身很小）。唯一行为轴：「MCP 结果资源上限」。

**Out of scope:** 脱敏（MC-16 已交付）；HTTP 准入（由 MC-13、MC-17 承接）；阻塞型读工具的有界、可取消执行（由紧随其后的 MC-29、MC-31 承接，任何生产读工具都在二者之后发布）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge 的 256 KiB 结果上限与超时码先例 | `src/internal/ai/agent_bridge/protocol.rs:47`（`MAX_RESULT_BYTES`）、`:374`（`LBR-AGENT-036`）、`:283-300`（`LBR-AGENT-028`） |
| 结果信封（MC-16 交付） | `src/internal/ai/mcp_server/output.rs` |
| 本地门 | `0.30.72`。六个 `agent_mcp_limits_test` `--exact` 均为 `test result: ok. 1 passed; 0 failed`。`agent_mcp_conformance_test` 7 passed。clippy `-D warnings` exit 0。nextest：`Summary [ 597.267s] 10667 tests run: 10667 passed (12 slow), 21 skipped`，exit 0。 |
| D 组 | tag `v0.30.72` 指向 `4c331fdcb654f17a361f4abfa88ac8472643ea81`。`release.yml` [39499419035](https://github.com/libra-tools/libra/actions/runs/39499419035) success。四个平台产物 HTTP 200。安装脚本 `DEFAULT_VERSION="v0.30.72"`。同一提交上 `base.yml` [39499321010](https://github.com/libra-tools/libra/actions/runs/39499321010) success（八个 job 全 success）与 `codeql.yml` [39499321387](https://github.com/libra-tools/libra/actions/runs/39499321387) success。 |
| 网站 | `cf` `547d50eadfff2a228b9e4a0d0bf0653c1f052418` 只改 `agent.en.md`。源文件与 `https://libra.tools/en/docs/commands/agent` 都含 `LBR-AGENT-028`、`LBR-AGENT-036` 与 `256 KiB`。Claude Code：`CLAUDE_VERDICT: PASS`，`P0=0`，`P1=0`。 |
| Codex | 对 `4c331fd`：`VERDICT: PASS`，`P0=0`，`P1=0`。 |

**Acceptance criteria:**

- [x] AC-1 `libra_test_blob {"bytes": 143360}`（140 KiB，两份内容合计超过 256 KiB）得到工具错误 `LBR-AGENT-028`。
- [x] AC-2 `libra_test_blob {"bytes": 102400}` 的响应 `result.structuredContent.len` 等于 102400（`libra_test_blob` 在结果中回报生成的字节数）。
- [x] AC-3 同一响应行的字节数不超过 262144。
- [x] AC-4 `libra_test_sleep {"ms": 31000}` 得到工具错误 `LBR-AGENT-036`。
- [x] AC-5 同一超时结果的 `structuredContent.retryable` 为 `true`。
- [x] AC-6 读 deadline 触发后的下一个 `ping` 得到 `{}`。
- [x] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [x] AC-1：`source .env.test && cargo test --test agent_mcp_limits_test envelope_caps_full_response -- --exact`（new）
- [x] AC-2：`source .env.test && cargo test --test agent_mcp_limits_test envelope_allows_under_cap -- --exact`（new）
- [x] AC-3：`source .env.test && cargo test --test agent_mcp_limits_test envelope_under_cap_fits_wire_limit -- --exact`（new）
- [x] AC-4：`source .env.test && cargo test --test agent_mcp_limits_test read_deadline_is_tool_error -- --exact`（new）
- [x] AC-5：`source .env.test && cargo test --test agent_mcp_limits_test read_deadline_is_retryable -- --exact`（new）
- [x] AC-6：`source .env.test && cargo test --test agent_mcp_limits_test server_survives_deadline -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [x] `docs/commands/agent.md`：`rg -qF -- 'LBR-AGENT-028' docs/commands/agent.md && rg -qF -- 'LBR-AGENT-036' docs/commands/agent.md && rg -qF -- '256 KiB' docs/commands/agent.md`
- [x] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'LBR-AGENT-028' docs/commands/zh-CN/agent.md && rg -qF -- 'LBR-AGENT-036' docs/commands/zh-CN/agent.md && rg -qF -- '256 KiB' docs/commands/zh-CN/agent.md`
- [x] `docs/development/commands/agent.md`：`rg -qF -- '256 KiB' docs/development/commands/agent.md && rg -qF -- 'deadline' docs/development/commands/agent.md`
- [x] 网站源文件 `agent`：`rg -qF -- 'LBR-AGENT-028' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'LBR-AGENT-036' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '256 KiB' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'LBR-AGENT-028' && printf '%s' "$PAGE" | rg -qF -- 'LBR-AGENT-036' && printf '%s' "$PAGE" | rg -qF -- '256 KiB'`（GC-MC-05 ⑥）
- [x] `docs/error-codes.md`：`rg -qF -- 'MCP tool result' docs/error-codes.md`
- [x] 测试索引：`rg -q '^\| `agent_mcp_limits_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_limits_test"' Cargo.toml`
- [x] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）。Codex：`VERDICT: PASS`，`P0=0`，`P1=0`。Claude Code：`CLAUDE_VERDICT: PASS`，`P0=0`，`P1=0`。

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-16`（消费结果信封）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/output.rs`、`src/internal/ai/mcp_server/tools/mod.rs`、`tests/agent_mcp_limits_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`docs/error-codes.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`docs/error-codes.md`。内容：「MCP server」小节新增 256 KiB 结果上限、30 s 读 deadline 与对应工具错误码；开发文档写上限设计（GC-MC-03 ②③）；`docs/error-codes.md` 两张表中 `LBR-AGENT-028` 与 `LBR-AGENT-036` 的描述补充 MCP 含义（英文措辞含 `MCP tool result`：工具结果超过 256 KiB；读工具超过 30 s）；`COMPATIBILITY.md`：N/A（不新增公开工具或参数）。不受影响：`COMPATIBILITY.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 资源上限 fail-closed；测试工具在 release 构建中不存在。

**Performance budget:** 读 deadline 30 s；结果 ≤ 256 KiB。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP 工具结果的资源上限（大小、读 deadline）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=MC-16,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-16; release=independent; split-from=MC-16; exception=N/A`

### Task MC-20: trace checkpoint 只读工具与查询共享

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-12 拆出。把 `agent checkpoint list/show` 的查询抽为 `checkpoint.rs` 内返回页结构体的公开函数（CLI 输出逐字节不变），新增只读工具 `libra_trace_checkpoint_list`、`libra_trace_checkpoint_show`，结果等于对应 `--json` 的 `data` 经结果信封后的值（只含元数据摘要，不读取 transcript blob）。唯一行为轴：「MCP 读取已捕获的 agent checkpoint 元数据」。

**Out of scope:** session 查询（MC-12 已交付）；transcript 导出与 `--raw`、rewind（DEFER-MC-01）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| checkpoint 查询与输出 | `src/command/agent/checkpoint.rs:260-329`、`:330-405`、`:1531-1557` |
| 既有 CLI 测试模块 | `tests/command/agent_checkpoint_test.rs` |

**Acceptance criteria:**

- [ ] AC-1 `libra --json agent checkpoint list` 对固定 fixture 的输出等于先于重构写入的内联期望值。
- [ ] AC-2 `libra --json agent checkpoint show <id>` 对固定 fixture 的输出等于先于重构写入的内联期望值。
- [ ] AC-3 `libra_trace_checkpoint_list` 的 `structuredContent` 等于同参数 CLI `--json` 的 `data`。
- [ ] AC-4 `libra_trace_checkpoint_list` 传 `limit = 1000` 的结果等于 CLI `--limit 1000` 的 `data`。
- [ ] AC-5 以 `libra_trace_checkpoint_list` 的 `next_cursor` 翻页，结果等于 CLI `--cursor` 的下一页。
- [ ] AC-6 `libra_trace_checkpoint_show` 对不存在的 id 得到工具错误，其 `stable_code` 等于 CLI 同输入的 stable code。
- [ ] AC-7 fixture transcript 的正文标记字符串不出现在 `libra_trace_checkpoint_list` 的响应中。
- [ ] AC-8 同一标记不出现在 `libra_trace_checkpoint_show` 的响应中。
- [ ] AC-9 仓库无 `agent_checkpoint` 表时 `libra_trace_checkpoint_list` 返回空页。
- [ ] AC-10 `libra_trace_checkpoint_list` 的 `annotations.readOnlyHint` 为 `true`。
- [ ] AC-11 `libra_trace_checkpoint_show` 的 `annotations.readOnlyHint` 为 `true`。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**判据规范（非计数正文）:** 门族（11 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`command_test::command::agent_checkpoint_test::checkpoint_list_json_byte_stable`；AC-2=`command_test::command::agent_checkpoint_test::checkpoint_show_json_byte_stable`；AC-3=`agent_mcp_trace_checkpoint_test::trace_checkpoint_list_matches_cli`；AC-4=`agent_mcp_trace_checkpoint_test::trace_checkpoint_limit_matches_cli`；AC-5=`agent_mcp_trace_checkpoint_test::trace_checkpoint_cursor_paging`；AC-6=`agent_mcp_trace_checkpoint_test::trace_checkpoint_show_missing`；AC-7=`agent_mcp_trace_checkpoint_test::checkpoint_list_excludes_transcript_body`；AC-8=`agent_mcp_trace_checkpoint_test::checkpoint_show_excludes_transcript_body`；AC-9=`agent_mcp_trace_checkpoint_test::trace_checkpoint_list_empty_store`；AC-10=`agent_mcp_trace_checkpoint_test::trace_checkpoint_list_read_only`；AC-11=`agent_mcp_trace_checkpoint_test::trace_checkpoint_show_read_only`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test command_test command::agent_checkpoint_test::checkpoint_list_json_byte_stable -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test command_test command::agent_checkpoint_test::checkpoint_show_json_byte_stable -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_trace_checkpoint_test trace_checkpoint_list_matches_cli -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_trace_checkpoint_test trace_checkpoint_limit_matches_cli -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_trace_checkpoint_test trace_checkpoint_cursor_paging -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_trace_checkpoint_test trace_checkpoint_show_missing -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_trace_checkpoint_test checkpoint_list_excludes_transcript_body -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_trace_checkpoint_test checkpoint_show_excludes_transcript_body -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_mcp_trace_checkpoint_test trace_checkpoint_list_empty_store -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_mcp_trace_checkpoint_test trace_checkpoint_list_read_only -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_mcp_trace_checkpoint_test trace_checkpoint_show_read_only -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_trace_checkpoint_list' docs/commands/agent.md && rg -qF -- 'libra_trace_checkpoint_show' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_trace_checkpoint_list' docs/commands/zh-CN/agent.md && rg -qF -- 'libra_trace_checkpoint_show' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_trace_checkpoint_list'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'libra_trace_checkpoint_list' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'libra_trace_checkpoint_show' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'libra_trace_checkpoint_list' && printf '%s' "$PAGE" | rg -qF -- 'libra_trace_checkpoint_show'`（GC-MC-05 ⑥）
- [ ] 测试索引：`rg -q '^\| `agent_mcp_trace_checkpoint_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_trace_checkpoint_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: CLI 与 MCP 共享的 checkpoint 查询单一事实源（GC-02）及 Cargo.toml [[test]] 行`

**Dependencies:** `MC-12`（写集串行：`tools/trace.rs`、agent 文档）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/command/agent/checkpoint.rs`、`src/internal/ai/mcp_server/tools/mod.rs`、`src/internal/ai/mcp_server/tools/trace.rs`、`tests/command/agent_checkpoint_test.rs`、`tests/agent_mcp_trace_checkpoint_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：trace checkpoint 两工具；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 只返回 CLI `--json` 已公开字段；不读取 transcript blob。

**Performance budget:** keyset 分页，上限沿用 CLI；多取 1 行判断下一页。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=trace checkpoint 只读工具与查询共享; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=11/8@EX-MC-07; VER=11/8@EX-MC-07; landing=2; prod-files=3; scope=M; deps=MC-12,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-12; release=independent; split-from=MC-12; exception=EX-MC-07`

### Task MC-21: MCP 工具调用速率限制（传输无关）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在协议核心为 `tools/call` 增加进程级令牌桶（每秒补充 10 次、桶容量 20）；超出时不执行工具，返回工具错误 `LBR-AGENT-029`（`retryable: true`，沿用 bridge「请求过多」稳定码）。`ping`、`tools/list`（以及 MC-14 起的 `server/discover`）不消耗配额；stdio 上 `initialize` 只能是连接的首个请求、发生在任何工具调用之前，不存在「桶耗尽后 initialize」的情形，HTTP 上的无状态 `initialize` 不消耗配额由 MC-13 验证。stdio 与后续 HTTP 传输共用同一限流器（HTTP 把它映射为 429，由 MC-13 交付）。唯一行为轴：「MCP 工具调用速率限制」。

**Out of scope:** HTTP 状态码映射（由 MC-13 承接）；按客户端区分的多令牌限流（DEFER-MC-04）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge「请求过多」稳定码先例 | `src/internal/ai/agent_bridge/protocol.rs:304-312`（`inflight_limit` → `LBR-AGENT-029`）；`docs/error-codes.md:149` |
| 协议核心与资源上限（MC-01、MC-19 交付；开工时刷新行号） | `src/internal/ai/mcp_server/{stdio,output}.rs` |

**Acceptance criteria:**

- [ ] AC-1 1 s 内连续发出 40 次 `tools/call libra_test_echo`，得到工具错误 `LBR-AGENT-029` 的响应不少于 10 个。
- [ ] AC-2 同一负载下每个 `LBR-AGENT-029` 结果的 `structuredContent.retryable` 为 `true`。
- [ ] AC-3 同一负载下成功响应不多于 30 个。
- [ ] AC-4 以每秒 50 次持续发送 5 s，成功响应总数不多于 80 个（桶容量 20 + 5 s × 10/s 再加 10 的计时余量）。
- [ ] AC-5 同一持续负载中，第 2 至第 5 秒里「出现过 `LBR-AGENT-029` 结果」的秒数等于 4。
- [ ] AC-6 停止发送 3 s 后的下一次 `tools/call` 成功。
- [ ] AC-7 桶耗尽时 `tools/list` 返回结果对象（非错误）。
- [ ] AC-8 桶耗尽时 `ping` 的结果等于 `{}`。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_rate_test burst_beyond_bucket_is_limited -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_rate_test rate_limited_is_retryable -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_rate_test burst_success_bounded -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_rate_test sustained_success_bounded -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_rate_test sustained_limit_applies_every_second -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_rate_test rate_limit_recovers -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_rate_test tools_list_not_rate_limited -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_rate_test ping_not_rate_limited -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'LBR-AGENT-029' docs/commands/agent.md && rg -qF -- '20' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'LBR-AGENT-029' docs/commands/zh-CN/agent.md && rg -qF -- '20' docs/commands/zh-CN/agent.md`
- [ ] `docs/development/commands/agent.md`：`rg -qF -- 'rate.rs' docs/development/commands/agent.md && rg -qF -- 'LBR-AGENT-029' docs/development/commands/agent.md`
- [ ] 网站源文件 `agent`：`rg -qF -- 'LBR-AGENT-029' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '20' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'LBR-AGENT-029' && printf '%s' "$PAGE" | rg -qF -- '20'`（GC-MC-05 ⑥）
- [ ] `docs/error-codes.md`：`rg -qF -- 'MCP tool call' docs/error-codes.md`
- [ ] 测试索引：`rg -q '^\| `agent_mcp_rate_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_rate_test"' Cargo.toml`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-33`（发布窗口顺序；`tools/mod.rs`、agent 文档与 MC-29 串行，`output.rs`、`stdio.rs` 与 MC-19 串行）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/stdio.rs`、`src/internal/ai/mcp_server/rate.rs`（新）、`src/internal/ai/mcp_server/mod.rs`、`tests/agent_mcp_rate_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`docs/error-codes.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`docs/error-codes.md`。内容：`docs/error-codes.md` 两张表中 `LBR-AGENT-029` 的描述补充 MCP 含义（英文措辞含 `MCP tool call`：工具调用超过每秒 10 次、突发 20 的速率限制）；「MCP server」小节新增工具调用速率限制（每秒 10 次、突发 20、超出为可重试的工具错误 `LBR-AGENT-029`；`ping`、`tools/list`、`server/discover` 与 HTTP `initialize` 不消耗配额）；开发文档写令牌桶设计；`COMPATIBILITY.md`：N/A（不新增公开参数）。不受影响：`COMPATIBILITY.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 限制任一客户端对仓库的持续驱动；超限调用不执行工具。

**Performance budget:** 令牌桶 O(1)；超限快速拒绝。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP 工具调用速率限制（进程级令牌桶，传输无关）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=3; scope=M; deps=MC-33,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-33; release=independent; split-from=N/A; exception=N/A`

### Task MC-22: MCP 请求 tracing span（净化字段）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 自 MC-01 拆出。每个请求发出一个 `agent.mcp.request` tracing span，字段只取服务端可控或已净化的值：`method`（仅当属于已知方法集合时记原名，否则记 `"unknown"`）、`id_hash`（JSON-RPC `id` 规范化后 SHA-256 的前 16 位十六进制，不记原值）、`repository_id`；`tools/call` 另带 `tool`（仅当属于已注册工具名时记原名，否则 `"unknown"`）；不记 `clientInfo`、`params` 与结果内容。唯一行为轴：「MCP 请求 tracing」。

**Out of scope:** OTLP 导出配置（沿用既有 `otlp` feature，不在本计划改动）；结果信封（由 MC-16、MC-19 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge 请求 span 的结构先例——注意它直接记录原始 `method` 与 `id`，本卡**不得**照搬，必须按本卡规则净化 | `src/command/agent/bridge.rs:61-72`；`tests/agent_bridge_span_test.rs:1-40`（全局订阅者捕获方式可复用） |
| 协议核心（MC-01 交付） | `src/internal/ai/mcp_server/stdio.rs` |
| 版本面 | `0.30.69`。八个 `agent_mcp_span_test` `--exact` 均通过（`cargo test --test agent_mcp_span_test`：8 passed）。`cargo clippy --all-targets --all-features -- -D warnings` exit 0。`cargo nextest run --all --no-fail-fast --retries 2`：`Summary [ 605.838s] 10644 tests run: 10644 passed (12 slow, 1 flaky), 21 skipped`，exit 0。 |
| D 组 | tag `v0.30.69` 指向 `b915419b7d98d606404499f70da2f7193dbd84ec`。`release.yml` [39440601807](https://github.com/libra-tools/libra/actions/runs/39440601807) success。四个平台产物 HTTP 200。安装脚本 `DEFAULT_VERSION="v0.30.69"`。含该提交的 main `3b1200a80e00e8d1a8866ab07aa790c27f8a17a9` 上 `base.yml` [39459099628](https://github.com/libra-tools/libra/actions/runs/39459099628) success（八个 job 全 success）与 `codeql.yml` [39459099663](https://github.com/libra-tools/libra/actions/runs/39459099663) success。中间几次 `base.yml` 被后续 main 提交取消，最终这次跑完。 |

**Acceptance criteria:**

- [x] AC-1 一次 `ping` 请求产生的名为 `agent.mcp.request` 的 span 数量为 1。
- [x] AC-2 该 span 的 `method` 字段等于 `"ping"`。
- [x] AC-3 以含标记字符串的未知方法名发出请求时，span 的 `method` 字段等于 `"unknown"`。
- [x] AC-4 以含标记字符串的 `id` 发出请求时，该标记不出现在 span 的任何字段中。
- [x] AC-5 握手时 `clientInfo.name` 中的标记字符串不出现在任何 span 字段中。
- [x] AC-6 请求 `params` 中的标记字符串不出现在该 span 的任何字段中。
- [x] AC-7 `tools/call libra_test_echo` 的 span 的 `tool` 字段等于 `"libra_test_echo"`。
- [x] AC-8 `tools/call` 以含标记字符串的未注册工具名调用时，span 的 `tool` 字段等于 `"unknown"`。
- [x] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [x] AC-1：`source .env.test && cargo test --test agent_mcp_span_test one_request_span_per_request -- --exact`（new）
- [x] AC-2：`source .env.test && cargo test --test agent_mcp_span_test request_span_method_field -- --exact`（new）
- [x] AC-3：`source .env.test && cargo test --test agent_mcp_span_test unknown_method_is_not_recorded -- --exact`（new）
- [x] AC-4：`source .env.test && cargo test --test agent_mcp_span_test request_id_is_not_recorded -- --exact`（new）
- [x] AC-5：`source .env.test && cargo test --test agent_mcp_span_test client_info_is_not_recorded -- --exact`（new）
- [x] AC-6：`source .env.test && cargo test --test agent_mcp_span_test request_span_excludes_params -- --exact`（new）
- [x] AC-7：`source .env.test && cargo test --test agent_mcp_span_test tools_call_span_has_tool_name -- --exact`（new）
- [x] AC-8：`source .env.test && cargo test --test agent_mcp_span_test unknown_tool_name_is_not_recorded -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [x] `docs/development/commands/agent.md`：`rg -qF -- 'agent.mcp.request' docs/development/commands/agent.md && rg -qF -- 'id_hash' docs/development/commands/agent.md`
- [x] 测试索引：`rg -q '^\| `agent_mcp_span_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_span_test"' Cargo.toml`
- [x] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）。Codex：`VERDICT: PASS`，`P0=0`，`P1=0`。本卡无网站页。

**Full-suite trigger:** `T-1: Cargo.toml 新增 [[test]] 行`

**Dependencies:** `MC-01`（消费协议核心与 `libra_test_echo`）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/stdio.rs`、`src/internal/ai/mcp_server/protocol.rs`、`tests/agent_mcp_span_test.rs`（新；全局订阅者，单文件单 target）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/development/commands/agent.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/development/commands/agent.md`。内容：开发文档写 span 字段与净化规则；用户命令文档、网站页、`COMPATIBILITY.md`：N/A（tracing 为诊断输出，无用户可见命令行为变化）。不受影响：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 客户端可控值不进入 tracing/OTLP；未知方法与工具名一律记为 `unknown`。

**Performance budget:** 每请求一个 span，O(1)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP 请求的 tracing span（只记服务端可控或已净化字段）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=2; scope=M; deps=MC-01,DEP-MC-03; writeset=序列化于 MC-01; release=independent; split-from=MC-01; exception=N/A`

### Task MC-23: 仓库 hook 的任务局部拒绝策略

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 为仓库 hook 运行器增加 tokio `task_local` 策略 `HookPolicy::Refuse`：作用域内 `run_repo_hook_with_io` 在解析到一个会执行的 hook（存在、未被 `LIBRA_NO_HOOKS` 关闭、且不是 `libra init` 内置的空 pre-commit）时**不启动任何进程**，返回新增错误变体 `RepoHookError::RefusedByPolicy { hook }`；`run_advisory_repo_hook`（post-commit）在作用域内同样不执行，记一条 `warn` 日志并向调用方返回 `AdvisoryOutcome::RefusedByPolicy`（MC-08 据此在结果中报告 `post_commit_skipped`）。另提供纯查询 `runnable_commit_hooks() -> Result<Vec<RepoHook>, RepoHookError>`（与执行路径共用同一解析函数，不执行任何 hook）。作用域外的行为完全不变：不改期限、不改进程清理，CLI 与 Harness bridge 的 hook 执行保持原样（本卡以回归门证明）。唯一行为轴：「调用方可禁止 hook 进程启动」。

**Out of scope:** MCP commit 使用该策略（由 MC-08 承接）；在 MCP 下安全运行 hook（需能约束全部派生进程含脱离进程组者的隔离，DEFER-MC-09）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| hook 运行入口、`LIBRA_NO_HOOKS` 与内置空 pre-commit 判定 | `src/internal/repo_hooks.rs:140-200` |
| advisory hook 入口 | `src/internal/repo_hooks.rs:385`（`run_advisory_repo_hook`） |
| commit 调用 hook 的位置 | `src/command/commit.rs:1464-1470`、`:1598`、`:2255` |

**Acceptance criteria:**

- [ ] AC-1 作用域内仓库存在自定义 `pre-commit`（写标记文件）时，`run_repo_hook_with_io(PreCommit, ..)` 返回 `RefusedByPolicy`。
- [ ] AC-2 同一调用后标记文件不存在。
- [ ] AC-3 作用域内自定义 `post-commit` 经 `run_advisory_repo_hook` 调用后，其标记文件不存在。
- [ ] AC-4 作用域外同一自定义 `pre-commit` 被执行（标记文件存在）。
- [ ] AC-5 CLI 回归：自定义 `pre-commit` 存在时 `libra commit -m x` 执行该 hook（标记文件存在）。
- [ ] AC-6 Harness 回归：`libra agent bridge --stdio` 的 `commit.create` 在自定义 `pre-commit` 存在时执行该 hook（标记文件存在）。
- [ ] AC-7 CLI 回归：自定义 `post-commit` 存在时 `libra commit -m x` 成功后该 hook 已执行（标记文件存在）。
- [ ] AC-8 Harness 回归：`commit.create` 成功后自定义 `post-commit` 已执行（标记文件存在）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test repo_hook_policy_test refuse_scope_rejects_custom_pre_commit -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test repo_hook_policy_test refuse_scope_does_not_spawn_hook -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test repo_hook_policy_test refuse_scope_skips_advisory_post_commit -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test repo_hook_policy_test outside_scope_runs_pre_commit -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test repo_hook_policy_test cli_commit_still_runs_pre_commit -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test repo_hook_policy_test bridge_commit_still_runs_pre_commit -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test repo_hook_policy_test cli_commit_still_runs_post_commit -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test repo_hook_policy_test bridge_commit_still_runs_post_commit -- --exact`（new）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 hook 运行器（CLI、Harness 与 MCP 共用）及 Cargo.toml [[test]] 行`

**Dependencies:** `MC-18`（发布窗口顺序）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/internal/repo_hooks.rs`、`tests/repo_hook_policy_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——`HookPolicy` 是内部作用域，作用域外 hook 行为与 CLI 参数不变；用户可见的「MCP 提交在存在 hook 时拒绝」由 MC-08 写入 agent 文档；`COMPATIBILITY.md`、repository-hooks 文档、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** MCP 写入路径不启动 hook 进程，因此不存在可在写锁释放后继续写工作树的 hook 派生进程。

**Performance budget:** 预检多一次 hooks 目录解析，O(hook 数)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=hook 运行器的调用方作用域拒绝策略（作用域外行为不变）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=MC-18,DEP-MC-03; writeset=序列化于 MC-18; release=independent; split-from=N/A; exception=N/A`

### Task MC-24: commit 前提包含 HEAD 符号引用

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在 MC-18 的内部 CAS 上增加 `CommitArgs.expected_head_ref: Option<HeadRef>`（`#[arg(skip)]`；`HeadRef` 为「分支名」或「detached」）。`vcs::commit_create_fenced` 接受调用方给出的 `expected_head_ref`（MCP 传入客户端在 `libra_status` 快照中审核过的 `head_ref`，见 MC-25/MC-08），在 fence 入口先与当前 HEAD 符号引用比较（不等即 `LBR-AGENT-038`），`update_head` 在同一事务内比较「符号引用 + OID」，任一不等即返回提交错误且不写任何 ref 与 reflog。暂存树前提 `expected_tree` 由 MC-43 承接。唯一行为轴：「HEAD 符号引用前提」。

**Out of scope:** OID 前提（MC-18 已交付）；用户可见参数（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| HEAD 更新在事务内重读分支 | `src/command/commit.rs:3440-3487`（`update_head`：按当前 `Head::Branch(name)` 写 ref） |
| OID 前提（MC-18 交付；开工时刷新行号） | `src/command/commit.rs`（`expected_parent`）、`src/internal/ai/agent_bridge/vcs.rs`（`commit_create_fenced`） |

**Acceptance criteria:**

- [ ] AC-1 竞态门：`vcs::commit_create_fenced` 停在暂停点时另一进程执行 `libra switch -c other`（同一 OID 的新分支），放行后调用返回 `LBR-AGENT-038`。
- [ ] AC-2 同一竞态后原分支的 ref 不变。
- [ ] AC-3 同一竞态后 `other` 分支的 ref 不变。
- [ ] AC-4 同一竞态后 HEAD reflog 中不存在 fenced 调用的提交记录。
- [ ] AC-5 fence 时为 detached、写入前被切到同一 OID 的分支时，调用返回错误（单元门）。
- [ ] AC-6 符号引用与 OID 都匹配时，新 HEAD 的父提交等于 `expected_parent`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test commit_fenced_race_test same_oid_branch_switch_is_fenced -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test commit_fenced_race_test same_oid_switch_keeps_original_branch -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test commit_fenced_race_test same_oid_switch_keeps_other_branch -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test commit_fenced_race_test same_oid_switch_no_fenced_reflog -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --lib command::commit::tests::expected_head_ref_detached_to_branch_mismatch -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --lib command::commit::tests::expected_head_ref_match_commits -- --exact`（new）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 commit 核心`

**Dependencies:** `MC-23`（发布窗口顺序）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/commit.rs`、`src/internal/ai/agent_bridge/vcs.rs`、`tests/commit_fenced_race_test.rs`、`tests/SERIAL_REGISTRY.tsv`（新增三行）、`.config/nextest.toml`（`sh tests/NEXTEST_GROUPS.sh` 重新生成）

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——`expected_head_ref` 是 `#[arg(skip)]` 内部字段，`libra commit` 的参数、输出与错误不变；`COMPATIBILITY.md`、命令文档、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 同 OID 的分支切换不能让 MCP 提交落到错误分支。

**Performance budget:** 事务内多一次 HEAD 符号引用读取，O(1)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=commit 前提扩展：HEAD 符号引用（入口与事务内）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=MC-23,DEP-MC-03; writeset=序列化于 MC-23; release=independent; split-from=N/A; exception=N/A`

### Task MC-25: `libra_status` 快照身份字段（不写对象）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-02 拆出。在 `structuredContent` 中增加 `head`、`head_ref`、`index_tree`，来自**同一次**读取：先读一次 HEAD（OID 与符号引用：`head_ref` 为分支名，detached 时为 `null`；未出生 HEAD 时 `head = null`、`head_ref` 为该未出生分支名），再载入一次 index；`index_tree` 由该次载入的 index 在内存中计算树 OID（`tree_plumbing` 新增不落盘的 `compute_tree_id_from_index`，与 `write_tree_from_index` 共用构树逻辑及其条目过滤——跳过 intent-to-add 条目，只是不调用 `save_tree_object`），**不向对象库写入任何对象**，因此工具保持 `readOnlyHint: true`。MC-26 的 `staged_changes` 复用同一快照与同一过滤后的条目集。`head`/`head_ref`/`index_tree` 是 `libra_commit` 的 `expected_head`/`expected_head_ref`/`expected_tree` 前提（MC-08、MC-24、MC-43）。唯一行为轴：「MCP 快照身份」。

**Out of scope:** `staged_changes`（由 MC-26 承接）；`status` 概要（MC-02 已交付）；提交前提的校验（MC-24、MC-43、MC-08）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 构树、条目过滤与落盘 | `src/internal/tree_plumbing.rs:81`（`write_tree_from_index`）、`:106`（跳过 intent-to-add）、`:335`（`save_tree_object`） |
| 未出生 HEAD 的 status 处理 | `src/command/status.rs:1020` |
| MC-02 的 `libra_status`（已先发布） | `src/internal/ai/mcp_server/tools/repo.rs` |

**Acceptance criteria:**

- [ ] AC-1 有提交的 fixture 中 `structuredContent.head` 等于 `libra rev-parse HEAD`。
- [ ] AC-2 同一 fixture 中 `structuredContent.head_ref` 等于 `libra symbolic-ref --short HEAD` 的输出。
- [ ] AC-3 detached HEAD 时 `structuredContent.head_ref` 为 `null`。
- [ ] AC-4 `structuredContent.index_tree` 等于随后执行 `libra write-tree` 输出的树 OID。
- [ ] AC-5 `libra add -N new.txt` 之后 `index_tree` 仍等于随后 `libra write-tree` 的输出（与写入器同一过滤）。
- [ ] AC-6 暂存一个新文件后调用，调用前后对象库目录（loose 对象与 pack 文件）的文件列表相同。
- [ ] AC-7 未出生 HEAD 的空仓库中 `structuredContent.head` 为 `null`。
- [ ] AC-8 同一仓库中 `structuredContent.head_ref` 等于 `libra symbolic-ref --short HEAD` 的输出。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_status_test status_head_matches_rev_parse -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_status_test status_head_ref_matches_symbolic_ref -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_status_test status_detached_head_ref_is_null -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_status_test status_reports_index_tree -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_status_test status_index_tree_skips_intent_to_add -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_status_test status_writes_no_objects -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_status_test status_unborn_head_is_null -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_status_test status_unborn_head_ref_is_branch -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'head_ref' docs/commands/agent.md && rg -qF -- 'index_tree' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'head_ref' docs/commands/zh-CN/agent.md && rg -qF -- 'index_tree' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'head_ref'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'head_ref' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'index_tree' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'head_ref' && printf '%s' "$PAGE" | rg -qF -- 'index_tree'`（GC-MC-05 ⑥）
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 helper（`tree_plumbing`）`

**Dependencies:** `MC-28`（发布窗口顺序）；`DEP-MC-03`。本卡另消费 MC-02 的 `libra_status`（已先发布）。；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/tree_plumbing.rs`（新增不落盘的树 OID 计算）、`src/internal/ai/mcp_server/tools/repo.rs`、`tests/agent_mcp_status_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：工具表中 `libra_status` 增加 `head`/`head_ref`/`index_tree`（同一次读取、不写对象、detached 时 `head_ref = null`、未出生 HEAD 时 `head = null`）；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 不写对象库。

**Performance budget:** 多一次内存构树（O(index 条目)）；读 deadline 30 s。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=libra_status 的快照身份（head/head_ref/index_tree）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=2; scope=M; deps=MC-28,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-28; release=independent; split-from=MC-02; exception=N/A`

### Task MC-26: `libra_status` 暂存差异 `staged_changes`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-25 拆出。增加 `staged_changes`（`[{path, status}]`，`status ∈ {A,M,D}`，按路径排序）：由 HEAD 树展开的条目（未出生 HEAD 时为空）与 MC-25 **同一次**载入、**同一过滤**（跳过 intent-to-add）后的 stage-0 条目在内存中比较得出；条目比较同时比较 OID 与 mode，因此仅 mode 变化记为 `M`。因构造同源，`staged_changes` 恰为 `head` 树到 `index_tree` 的差异，不受之后任何 `add` 影响。`status` 由独立的 status 收集器采样，不与快照同源；因此在 `status` 收集完成后再读一次 HEAD 并在内存中重算 index 树，二者都与快照相同时 `head_index_unchanged = true`，否则为 `false`。该标志**只**复核 HEAD 与 index：工作树侧的概要（未暂存、未跟踪）可能反映快照之后的时刻，标志仍为 `true`（文档写明：审核只以 `staged_changes`/`index_tree` 为准，`status` 只是概要；`head_index_unchanged = false` 时连概要的暂存部分也不可信）。唯一行为轴：「MCP 暂存差异与概要一致性」。

**Out of scope:** 快照身份字段（MC-25 已交付）；提交前提（MC-24、MC-43、MC-08）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| tree 写入器的条目过滤 | `src/internal/tree_plumbing.rs:106` |
| status 暂存侧对 intent-to-add 的同一规则 | `src/command/status.rs:1006` |

**Acceptance criteria:**

- [ ] AC-1 测试先以 `libra write-tree` 落盘后，`staged_changes` 等于 `libra --json diff-tree <head> <index_tree>` 给出的路径与状态集合。
- [ ] AC-2 `LIBRA_TEST=1` 下工具在载入 index 之后的暂停点停住时，另一进程执行 `libra add new.txt`，放行后返回的 `staged_changes` 不含 `new.txt`。
- [ ] AC-3 `libra add -N new.txt` 之后 `staged_changes` 不含 `new.txt`。
- [ ] AC-4 已跟踪文件 `f` 经 `libra rm --cached f && libra add -N f` 后，`staged_changes` 含 `{"path":"f","status":"D"}`。
- [ ] AC-5 已跟踪文件只改可执行位并 `libra add` 后，`staged_changes` 含该路径且 `status` 为 `M`。
- [ ] AC-6 未出生 HEAD 且已暂存 `a.txt` 时 `staged_changes` 等于 `[{"path":"a.txt","status":"A"}]`。
- [ ] AC-7 `LIBRA_TEST=1` 下工具在捕获快照之后、收集 `status` 之前的暂停点停住时另一进程执行 `libra add new.txt`，放行后结果的 `head_index_unchanged` 为 `false`。
- [ ] AC-8 同一暂停点期间只修改工作树文件（不 `add`）时，结果的 `head_index_unchanged` 为 `true`（该标志只覆盖 HEAD 与 index）。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_status_test status_staged_changes_match_tree_diff -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_status_test status_snapshot_ignores_concurrent_add -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_status_test status_intent_to_add_new_path_not_staged -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_status_test status_intent_to_add_tracked_path_is_deleted -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_status_test status_mode_only_change_is_modified -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_status_test status_unborn_staged_file_is_added -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_status_test status_inconsistent_when_add_between_snapshot_and_summary -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_status_test status_head_index_unchanged_ignores_worktree_only_change -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'staged_changes' docs/commands/agent.md && rg -qF -- 'head_index_unchanged' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'staged_changes' docs/commands/zh-CN/agent.md && rg -qF -- 'head_index_unchanged' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'staged_changes'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'staged_changes' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'head_index_unchanged' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'staged_changes' && printf '%s' "$PAGE" | rg -qF -- 'head_index_unchanged'`（GC-MC-05 ⑥）
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-5: 计划级 GC-MC-01 要求每张实现卡的 C 组全量包含全部 `agent_bridge_*` target 与工具 schema 一致性门（`agent_mcp_conformance_test`），本卡按 T-5 执行全量`

**Dependencies:** `MC-25`（消费同一快照与过滤后的条目集）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/tools/repo.rs`、`tests/agent_mcp_status_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：工具表中 `libra_status` 增加 `staged_changes`（与 `index_tree` 同源、intent-to-add 不计入、仅 mode 变化为 `M`），说明审核暂存内容以快照为准；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 不写对象库；暂停点只在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时存在。

**Performance budget:** 一次 HEAD 树展开与 O(index 条目) 比较；读 deadline 30 s。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=libra_status 的暂存差异（与 index_tree 同源）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=MC-25,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-25; release=independent; split-from=MC-25; exception=N/A`

### Task MC-27: `libra_diff` staged 结果绑定快照身份

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** `mode = "staged"` 时只载入一次 HEAD（OID 与符号引用）与一次 index：以 MC-25 的 `compute_tree_id_from_index` 在内存中得出 `index_tree`（不写对象），并以**同一次载入**的 index 条目（与写入器同一过滤）与 HEAD 树为输入生成 patch（blob 为不可变对象，因此 patch 与 `index_tree` 按构造一致）。现行 `run_diff_for_service` 与共享 diff 引擎 `src/command/diff/compare.rs::run_diff` 自行载入 index（`:69`）并重新读取 HEAD（`:958`），不接受外部快照，因此本卡改动 diff 引擎：`run_diff` 增加可选的已捕获输入 `DiffSnapshot { head: Option<(ObjectHash, Tree)>, index: Index }`，给出时不再载入 index、不再读取 HEAD；staged 快照路径**只经 MC-31 的有界入口**：`run_diff_for_service_bounded` 接受该快照（`src/command/diff.rs` 的 `diff_index_snapshot` 是其薄封装），因此尺寸预检（MC-32）、行数上限与关闭 rename 检测对 staged 快照同样生效；复用同一 patch 渲染，`--no-ext-diff`/`--no-textconv` 语义不变，CLI 不传快照（输出不变，由现有 diff 测试守护）。审核完整性标志 `review_complete` 由 MC-30 增加；结果增加 `head`、`head_ref`、`index_tree` 三个字段（与 `libra_status` 同义）。文档写明审核流程：客户端以所审核 staged diff 的 `head`/`head_ref`/`index_tree` 作为 `libra_commit` 的 `expected_head`/`expected_head_ref`/`expected_tree`；此后任何重新暂存（含同一路径换内容）都会改变 `index_tree`，提交以 `LBR-AGENT-038` 拒绝；`truncated: true` 的 diff 不是完整审核依据（文档警告）。`worktree`/`checkpoint` 模式不变。唯一行为轴：「staged diff 与快照绑定」。

**Out of scope:** `libra_status` 的快照字段（MC-25/MC-26 已交付）；`head` 字段的独立门（与 `libra_status` 同一读取函数，MC-25 AC-1 已覆盖）；以该快照提交时的拒绝（MC-08 承接，因 MC-08 在本卡之后发布）；服务端强制「提交前必须审核 diff」（客户端流程，不可由服务端验证）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| staged diff 现行实现（读当前 index） | `src/internal/ai/agent_bridge/vcs.rs:212-302` |
| 服务端 diff 入口自行取输入 | `src/command/diff.rs:1018`（`run_diff_for_service`） |
| 不落盘树 OID 计算（MC-25 交付） | `src/internal/tree_plumbing.rs`（`compute_tree_id_from_index`） |

**Acceptance criteria:**

- [ ] AC-1 `mode = "staged"` 结果的 `index_tree` 等于随后 `libra write-tree` 输出的树 OID。
- [ ] AC-2 同一结果的 `head_ref` 等于 `libra symbolic-ref --short HEAD` 的输出。
- [ ] AC-3 `LIBRA_TEST=1` 下工具在载入 index 之后的暂停点停住时，另一进程执行 `libra add new.txt`，放行后返回的 patch 不含 `new.txt`。
- [ ] AC-4 `LIBRA_TEST=1` 下工具在载入 index 之后的暂停点停住时，另一进程把已暂存的 `a.txt` 改为新内容并重新 `libra add`，放行后返回的 patch 含暂停前暂存内容的新增行。
- [ ] AC-5 同一情形下返回的 `index_tree` 等于暂停前执行 `libra write-tree` 的输出。
- [ ] AC-6 staged diff 调用前后对象库目录（loose 对象与 pack 文件）的文件列表相同。
- [ ] AC-7 staged 结果中一个 2 MiB 的已暂存 blob 记 `patch_omitted = true`（快照路径经有界入口）。
- [ ] AC-8 配置 `diff.renames = true` 且暂存一次改名时，staged 结果给出一个 `D` 与一个 `A` 条目、没有 `R` 条目。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_diff_test diff_staged_reports_index_tree -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_diff_test diff_staged_reports_head_ref -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_diff_test diff_staged_snapshot_ignores_concurrent_add -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_diff_test diff_staged_patch_ignores_concurrent_restage -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_diff_test diff_staged_tree_ignores_concurrent_restage -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_diff_test diff_staged_writes_no_objects -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_diff_test diff_staged_snapshot_applies_size_bound -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_diff_test diff_staged_snapshot_disables_renames -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'index_tree' docs/commands/agent.md && rg -qF -- 'head_ref' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'index_tree' docs/commands/zh-CN/agent.md && rg -qF -- 'head_ref' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'index_tree'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'index_tree' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'head_ref' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'index_tree' && printf '%s' "$PAGE" | rg -qF -- 'head_ref'`（GC-MC-05 ⑥）
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 diff 入口（CLI 与 MCP 共用）；另按 GC-MC-01 全量包含全部 `agent_bridge_*` target`

**Dependencies:** `MC-26`（写集串行：`tools/repo.rs` 与 agent 文档）；`DEP-MC-03`。本卡另消费 MC-25 的不落盘树计算（已先发布）。；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/command/diff/compare.rs`（`run_diff` 接受已捕获的 `DiffSnapshot`）、`src/command/diff.rs`（共享入口 `diff_index_snapshot`）、`src/internal/ai/mcp_server/tools/repo.rs`、`tests/agent_mcp_diff_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：`libra_diff` staged 结果增加 `head`/`head_ref`/`index_tree` 并写明审核→提交流程；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 不写对象库；GC-MC-03 脱敏不变；暂停点只在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时存在。

**Performance budget:** 多一次内存构树（O(index 条目)）；patch ≤ 256 KiB；读 deadline 30 s。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=libra_diff staged 模式的快照绑定（patch 与 head/head_ref/index_tree 同源）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=3; prod-files=3; scope=M; deps=MC-26,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-26; release=independent; split-from=N/A; exception=N/A`

### Task MC-28: commit 未出生分支前提（`ExpectedParent::Unborn`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-24 拆出。把 MC-18 的 `CommitArgs.expected_parent: Option<String>` 改为 `Option<ExpectedParent>`（`Commit(oid)` | `Unborn`；`#[arg(skip)]`，CLI 不暴露）。`Unborn` 要求 `vcs::commit_create_fenced` 的 fence 入口与 `update_head` 的 ref 写入事务内当前分支 ref 都不存在，否则返回提交错误（fenced 调用映射 `LBR-AGENT-038`）且不写任何 ref 与 reflog；`Commit(oid)` 语义与 MC-18 相同。调用方（MC-08）把 `expected_head = null` 映射为 `Unborn`，从不映射为 `None`（`None` 只保留给不设前提的 CLI/Harness 路径）。唯一行为轴：「未出生前提」。

**Out of scope:** MCP 参数映射（由 MC-08 承接）；OID/符号引用/暂存树前提（MC-18、MC-24、MC-43 已交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| HEAD 更新事务（MC-18/MC-24 已扩展；开工时刷新行号） | `src/command/commit.rs`（`update_head`、`expected_parent`） |
| 未出生分支的首个提交路径 | `src/command/commit.rs:3440-3487` |

**Acceptance criteria:**

- [ ] AC-1 分支 ref 不存在时以 `Unborn` 提交成功，新提交没有父提交（单元门）。
- [ ] AC-2 分支 ref 已存在时以 `Unborn` 提交返回错误（单元门）。
- [ ] AC-3 同一情形下分支 ref 不变（单元门）。
- [ ] AC-4 竞争首个提交：`vcs::commit_create_fenced(Unborn)` 停在 MC-18 的 `LIBRA_TEST=1` 暂停点时，另一进程执行 `libra commit -m first`，放行后调用返回 `LBR-AGENT-038`。
- [ ] AC-5 同一竞争后分支 ref 等于另一进程创建的首个提交。
- [ ] AC-6 同一竞争后 HEAD reflog 只有另一进程的一条记录。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib command::commit::tests::expected_unborn_commits_root -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --lib command::commit::tests::expected_unborn_with_existing_ref_errors -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib command::commit::tests::expected_unborn_with_existing_ref_keeps_ref -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test commit_fenced_race_test unborn_first_commit_race_is_fenced -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test commit_fenced_race_test unborn_race_keeps_other_ref -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test commit_fenced_race_test unborn_race_single_reflog_entry -- --exact`（new）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 commit 核心`

**Dependencies:** `MC-43`（写集串行：`commit.rs`；`vcs.rs`、`commit_fenced_race_test.rs` 与 MC-24 串行）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/commit.rs`、`src/internal/ai/agent_bridge/vcs.rs`、`tests/commit_fenced_race_test.rs`、`tests/SERIAL_REGISTRY.tsv`（新增三行）、`.config/nextest.toml`（`sh tests/NEXTEST_GROUPS.sh` 重新生成）

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——`ExpectedParent` 是 `#[arg(skip)]` 内部类型，`libra commit` 的参数、输出与错误不变；`COMPATIBILITY.md`、命令文档、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 未出生分支上的竞争首个提交不能让 MCP 调用失去前提。

**Performance budget:** 事务内多一次 ref 存在性读取，O(1)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=commit 期望父提交的「未出生」状态（入口与事务内）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=MC-43,DEP-MC-03; writeset=序列化于 MC-43; release=independent; split-from=MC-24; exception=N/A`

### Task MC-29: 读工具的子进程执行器（超时强制终止）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `in-progress` / 空

**Description:** 自 MC-19 拆出。新增读执行器 `mcp_server/exec.rs`：每次读工具调用由服务进程派生一个隐藏的读子进程 `libra agent mcp --read-worker`（`#[arg(hide = true)]`，不出现在 help；只从 stdin 读取一个由服务端构造的内部请求、向 stdout 写一个结果；在 `src/cli.rs` 中先于常规预检被分派到**只读预检**：只做仓库发现并以只读方式读取 `core.objectformat` 设置 hash kind（SHA-1/SHA-256 仓库都正确），不执行数据库迁移（schema 不是当前版本即以 `LBR-AGENT-037` 失败）、不修复对象索引、不启动自动升级检查或任何启动维护；以发起进程的工作目录启动，只读访问仓库），容量 4 的信号量限制同时存在的读子进程数。读 deadline 到期时服务立即回复 `LBR-AGENT-036`，并在 Unix 上向读子进程的进程组发送 `SIGKILL`、在 Windows 上 `TerminateProcess`，**回收（wait）完成后才归还槽位**（≤ 1 s）。因此任何读路径（status、diff 含 rename 检测、checkpoint、workspace、trace）的阻塞计算、I/O 与内存都随子进程一起被强制终止，超时的读工作不会继续占用槽位或资源。另注册仅在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时存在的 `libra_test_block`（在读子进程内阻塞并写 PID 文件）与 `libra_test_hash_kind`（在读子进程内返回当前 hash kind），以及 debug-only 读 deadline 覆盖 `LIBRA_MCP_TEST_READ_DEADLINE_MS`。唯一行为轴：「读工具子进程执行器」。

**Out of scope:** 写工具（在服务进程内执行，不受读 deadline 约束，见 ADR-MC-06 的写入期限）；diff 的尺寸预检与 rename 关闭（由 MC-31 承接）；具体读工具接入（各工具卡经注册表自动在执行器内运行）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge 的读超时先例 | `src/internal/ai/agent_bridge/protocol.rs:374`（`LBR-AGENT-036`） |
| 读 deadline（MC-19 交付） | `src/internal/ai/mcp_server/tools/mod.rs` |
| 本地门 | `v0.30.73` 至 `v0.30.76` 的 tag 都不改。`v0.30.76`（`9694ff0`）Codex `VERDICT: FAIL`，`P0=0`，`P1=1`：正常退出路径在槽位仍存活时 `reader.join()`，未结束的读取线程也在释放槽位之前启动 join。前滚 `0.30.77`：stdout 经有界 channel 交回；任何 `join` 之前先 `drop` 槽位，回复路径不再 `join`。`deadline_reply_does_not_wait_for_stdout_holder`、`blocking_read_times_out`、`slots_return_after_workers_killed`、`envelope_allows_under_cap` 均为 `test result: ok. 1 passed; 0 failed`。clippy `-D warnings` exit 0。nextest：`Summary [ 572.138s] 10676 tests run: 10676 passed (12 slow), 21 skipped`，exit 0。 |
| 0.30.79 候选 | 三个版本面为 `0.30.79`。`source .env.test && cargo test --test agent_mcp_exec_test <name> -- --exact`：AC-1..AC-8 与 `deadline_reply_does_not_wait_for_stdout_holder` 均为 `test result: ok. 1 passed; 0 failed`。英文命令页含 `LBR-AGENT-036` 与 `4 个并发读子进程`。网站 `cf` `7b157e9cda6e772653aa772671cd4842c59f6285` 与 `https://libra.tools/en/docs/commands/agent`（HTTP 200）含 `LBR-AGENT-036` 与 `4 concurrent read workers`。Codex review session `01a15d4b-07cc-7880-a53b-2afdd4a03e4b`（read-only，未提交 diff）：无行为缺陷，`P0=0`，`P1=0`。D 组未取。 |

**Acceptance criteria:**

- [x] AC-1 读 deadline 覆盖为 2 s 时，`libra_test_block` 调用在 5 s 内得到工具错误 `LBR-AGENT-036`。
- [x] AC-2 同一超时后 1 s 内，`libra_test_block` 的读子进程（PID 文件所列）已不存在。
- [x] AC-3 4 个并发 `libra_test_block` 全部超时后 2 s 内，第 5 个读工具调用 `libra_test_sleep {"ms": 10}` 成功（槽位已随子进程回收归还）。
- [x] AC-4 在 `libra init --object-format sha256` 的仓库中，`libra_test_hash_kind` 返回 `"sha256"`。
- [x] AC-5 仓库数据库 schema 版本低于本二进制要求（测试把版本记录回退一级模拟）时，读工具调用得到工具错误 `LBR-AGENT-037`。
- [x] AC-6 同一调用后数据库记录的 schema 版本不变（读子进程不执行迁移）。
- [x] AC-7 对象索引处于需修复状态的仓库中调用读工具后，对象索引文件的内容与修改时间不变（读子进程不修复）。
- [x] AC-8 读工具调用后 `LIBRA_HOME/upgrade/` 下没有新增或修改的文件（读子进程不启动自动升级检查）。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [x] AC-1：`source .env.test && cargo test --test agent_mcp_exec_test blocking_read_times_out -- --exact`（new）
- [x] AC-2：`source .env.test && cargo test --test agent_mcp_exec_test timed_out_worker_is_killed -- --exact`（new）
- [x] AC-3：`source .env.test && cargo test --test agent_mcp_exec_test slots_return_after_workers_killed -- --exact`（new）
- [x] AC-4：`source .env.test && cargo test --test agent_mcp_exec_test read_worker_uses_repo_hash_kind -- --exact`（new）
- [x] AC-5：`source .env.test && cargo test --test agent_mcp_exec_test read_worker_refuses_outdated_schema -- --exact`（new）
- [x] AC-6：`source .env.test && cargo test --test agent_mcp_exec_test read_worker_does_not_migrate -- --exact`（new）
- [x] AC-7：`source .env.test && cargo test --test agent_mcp_exec_test read_worker_does_not_repair_object_index -- --exact`（new）
- [x] AC-8：`source .env.test && cargo test --test agent_mcp_exec_test read_worker_skips_upgrade_check -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [x] `docs/commands/agent.md`：`rg -qF -- 'LBR-AGENT-036' docs/commands/agent.md && rg -qF -- '4 个并发读子进程' docs/commands/agent.md`
- [x] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'LBR-AGENT-036' docs/commands/zh-CN/agent.md && rg -qF -- '4 个并发读子进程' docs/commands/zh-CN/agent.md`
- [x] `docs/development/commands/agent.md`：`rg -qF -- '--read-worker' docs/development/commands/agent.md && rg -qF -- 'SIGKILL' docs/development/commands/agent.md`
- [x] 网站源文件 `agent`：`rg -qF -- 'LBR-AGENT-036' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- '4 concurrent read workers' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'LBR-AGENT-036' && printf '%s' "$PAGE" | rg -qF -- '4 concurrent read workers'`（GC-MC-05 ⑥）
- [x] 测试索引：`rg -q '^\| `agent_mcp_exec_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_exec_test"' Cargo.toml`
- [x] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-1: src/cli.rs 全局预检分派与 Cargo.toml [[test]] 行`

**Dependencies:** `MC-19`（消费读 deadline 与测试工具）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/exec.rs`（新）、`src/internal/ai/mcp_server/tools/mod.rs`、`src/command/agent/mcp.rs`（隐藏的 `--read-worker` 入口）、`src/cli.rs`（`--read-worker` 先于常规预检分派到只读预检）、`tests/agent_mcp_exec_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：「MCP server」小节写明读工具在至多 4 个并发读子进程（英文页措辞 `4 concurrent read workers`）中执行、超时即回复 `LBR-AGENT-036` 并强制终止该子进程；开发文档写 `--read-worker` 子进程执行器、`SIGKILL`/`TerminateProcess` 与回收后归还槽位的设计；`COMPATIBILITY.md`、`docs/error-codes.md`：N/A（不新增参数与码）。不受影响：`COMPATIBILITY.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 超时的读工作随子进程被强制终止，不会无界累积；读子进程只读访问仓库、请求由服务端构造；测试工具与覆盖只在 debug 且 `LIBRA_TEST=1` 时存在。

**Performance budget:** 每次读调用一次进程派生（数十毫秒量级）；读并发 ≤ 4；超时后 ≤ 1 s 内回收并归还槽位。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=读工具子进程执行器（有界并发、超时强制终止、回收后归还槽位）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=4; prod-files=4; scope=M; deps=MC-19,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-19; release=independent; split-from=MC-19; exception=N/A`

### Task MC-31: MCP diff 的资源上限（尺寸预检、行数上限、关闭 rename 检测）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-29 拆出。新增只供 MCP 使用的有界服务端 diff 入口：`src/command/diff.rs::run_diff_for_service_bounded` 与 `agent_bridge/vcs.rs::diff_data_bounded`（bridge 原 `diff_data`、`run_diff_for_service` 与 CLI 不变）。有界入口对每个文件**在分配/读取正文之前**做尺寸预检：index/HEAD 侧用 MC-32 的 `object_size`（delta 感知、不重建对象），工作树侧用 MC-36 的 `read_worktree_blob_bytes_bounded`（单一句柄、读取中强制上限、符号链接语义不变；LFS 经 MC-39），新旧两侧之和超过 1 MiB 即不读取（或停止读取）正文、记 `patch_omitted = true`、`omitted_reason = "too_large"`；有界入口同时接受可选的已捕获快照（MC-27 的 staged 快照路径经此入口，见 MC-27）；读取后新旧行数之和超过 10 000 同样省略。全局收集预算由 MC-37 承接；有界入口**一律关闭 rename 检测**（无论 `diff.renames` 配置），改名显示为一删一增（文档写明），因此不存在无预算的 rename 后处理。时间上限由 MC-29 的读子进程强制终止保证。唯一行为轴：「MCP diff 资源上限」。

**Out of scope:** 读子进程执行器（MC-29 已交付）；MCP `libra_diff` 工具（由 MC-05 承接，消费本卡入口）；CLI 的 rename 与大文件行为（不变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| diff 引擎同步读 index 与计算 | `src/command/diff/compare.rs:60-69`、`:256`（`Diff::diff`） |
| 服务端 diff 入口 | `src/command/diff.rs:1018`（`run_diff_for_service`） |
| bridge diff 结果构造（预算与页上限） | `src/internal/ai/agent_bridge/vcs.rs:212-260` |

**Acceptance criteria:**

- [ ] AC-1 单元门：有界入口对一个 2 MiB 的工作树文件（单文件上限 1 MiB）记 `patch_omitted = true`。
- [ ] AC-2 单元门：有界入口对一个 2 MiB 的已暂存 blob（以 pack delta 存储）记 `patch_omitted = true`。
- [ ] AC-3 单元门：一个 < 1 MiB 但新旧行数之和为 50 000 的文件在有界入口下 `patch_omitted` 为 `true`。
- [ ] AC-4 配置 `diff.renames = true` 且暂存一次改名时，有界入口给出一个 `D` 与一个 `A` 条目、没有 `R` 条目。
- [ ] AC-5 同一配置下 `libra diff --cached --name-status` CLI 仍输出 `R` 行（CLI 不变）。
- [ ] AC-6 端到端：读 deadline 覆盖为 2 s、经 MC-29 执行器对 400 个各改动 5 000 行的文件运行有界 diff，调用在 5 s 内得到 `LBR-AGENT-036`。
- [ ] AC-7 SHA-256 仓库中经 MC-29 执行器运行的有界 diff 成功，patch 含暂存改动的新增行。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib command::diff::tests::bounded_marks_large_worktree_file_omitted -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --lib command::diff::tests::bounded_marks_large_packed_delta_blob_omitted -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib command::diff::tests::bounded_omits_too_many_lines -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --lib command::diff::tests::bounded_disables_rename_detection -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test command_test command::diff_test::cli_rename_detection_unchanged -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_exec_test real_bounded_diff_times_out -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_exec_test bounded_diff_works_in_sha256_repo -- --exact`（new）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 diff 与对象读取 helper（CLI、Harness 与 MCP 共用模块）`

**Dependencies:** `MC-39`（消费有界读取与 LFS 指针 helper；MC-36、MC-32 的 `object_size` 与 MC-29/MC-35 的读子进程已先发布）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/diff.rs`（`run_diff_for_service_bounded`）、`src/command/diff/compare.rs`（尺寸预检、rename 关闭与可选快照的参数）、`src/internal/ai/agent_bridge/vcs.rs`（只新增 `diff_data_bounded`）、`tests/agent_mcp_exec_test.rs`、`tests/command/diff_test.rs`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——有界入口只供 MCP 使用，`libra diff` CLI 与 bridge 的参数、输出（含 rename）不变；MCP 侧的 `patch_omitted`/`omitted_reason` 与「改名显示为一删一增」由 MC-05 在工具文档中说明；`COMPATIBILITY.md`、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 超大文件不进入内存；不存在无预算的 rename 后处理；时间由 MC-29 强制终止兜底。

**Performance budget:** 每文件一次尺寸预检（stat 或对象头）；单文件正文 ≤ 1 MiB、行数 ≤ 10 000。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP diff 资源上限（尺寸预检、行数上限、关闭 rename）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=7/8; VER=7/8; landing=3; prod-files=3; scope=M; deps=MC-39,DEP-MC-03; writeset=序列化于 MC-39; release=independent; split-from=MC-29; exception=N/A`

### Task MC-32: 对象尺寸 helper（delta 感知、不重建对象）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-31 拆出（R28）。在 `src/command/mod.rs` 现有对象读取函数旁新增 helper `object_size(oid)`：loose 对象只解压到对象头；pack 非 delta 条目读条目头；pack **delta** 条目（`OFS_DELTA`/`REF_DELTA`）解压 delta 数据开头、读取其中的目标对象尺寸 varint（不重建对象），因此小 delta 展开成大对象时也得到真实尺寸，解压字节数 ≤ 64 KiB。工作树侧的有界读取由 MC-36 承接。现有读取函数与 CLI 行为不变。唯一行为轴：「对象尺寸」。

**Out of scope:** 工作树侧有界读取（由 MC-36 承接）；MCP diff 的有界入口与行数上限（由 MC-31 承接）；CLI 的读取路径（不变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现行无界读取 | `src/command/mod.rs:360`（`fs::read`） |
| pack delta 条目头只给出 delta 数据尺寸 | Git pack 格式（delta 数据以基对象尺寸与目标对象尺寸两个 varint 开头） |

**Acceptance criteria:**

- [ ] AC-1 loose 2 MiB blob 的 `object_size` 返回 2097152。
- [ ] AC-2 同一调用的解压字节数（debug 计数器）≤ 65536。
- [ ] AC-3 pack 中非 delta 的 2 MiB blob 的 `object_size` 返回 2097152。
- [ ] AC-4 pack 中以小 delta 存储、目标为 2 MiB 的 blob，`object_size` 返回 2097152。
- [ ] AC-5 同一 delta 调用的解压字节数 ≤ 65536（不重建对象）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib command::tests::object_size_loose_blob -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --lib command::tests::object_size_loose_inflates_header_only -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib command::tests::object_size_packed_full -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --lib command::tests::object_size_packed_delta_reports_target_size -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --lib command::tests::object_size_packed_delta_does_not_reconstruct -- --exact`（new）

**Full-suite trigger:** `T-1: GC-02 意义上的共享对象/文件读取 helper`

**Dependencies:** `MC-35`（发布窗口顺序）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/mod.rs`（新增 `object_size`）

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——新增内部 helper，CLI 读取路径、参数与输出不变；`COMPATIBILITY.md`、命令文档、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** delta 不会被重建以求尺寸；对象头解压量有界。

**Performance budget:** 对象头解压 ≤ 64 KiB。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=对象尺寸（delta 感知、不重建对象）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=1; prod-files=1; scope=M; deps=MC-35,DEP-MC-03; writeset=序列化于 MC-35; release=independent; split-from=MC-31; exception=N/A`

### Task MC-33: status 的有界收集（供 MCP 使用）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-02 前置拆出（R28，R30 重写）。在 `src/command/status.rs` 新增 `collect_status_json_envelope_bounded(cwd, budget)`，`budget = { max_paths, max_entries }`（MCP 取 `max_paths = 100 000`、`max_entries = 1000`；仅在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时可由 `LIBRA_MCP_TEST_STATUS_MAX_PATHS` 覆盖）：① **扫描预算**——工作树扫描（`src/command/status_untracked.rs:76`、`:401` 的目录读取不再以 `usize::MAX` 调用，改为剩余预算）与各变化向量的收集共享一个计数，累计收集的路径数达到 `max_paths` 即停止发现新路径，并把既有的完整性字段置为未完成（`base_scan_complete = false`、`complete = false`），因此扫描阶段保留的路径数有界；② **输出上限**——status JSON 中**全部**动态数组（`staged.new`、`staged.modified`、`staged.deleted`、`staged.renamed`、`unstaged.new`、`unstaged.intent_to_add`、`unstaged.modified`、`unstaged.deleted`、`unstaged.renamed`、`unmerged`、`untracked`、`ignored`、`warnings`、`renames`、`io_blocked`、`merge_state.conflicted_paths`，共 16 个，含嵌套数组）在收集时各自至多保留 `max_entries` 条，超出只计数；③ 只有发生省略时才输出 `omitted_count: {<数组路径>: n}`（无省略时不出现该键，结果形状与 CLI `data` 完全相同，MC-02 的等价门因此成立）。index 载入仍为 O(index)（与所有 libra 命令相同）。现有 `collect_status_json_envelope_for_api` 与 CLI 不变。唯一行为轴：「status 有界收集」。

**Out of scope:** MCP 工具接线（由 MC-02 承接）；index 载入（O(index)，与所有命令相同）；扫描时间（由 MC-29 的读子进程强制终止兜底）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现行 status 收集先构建完整 JSON | `src/command/status.rs:477-521`（`collect_status_json_envelope_for_api`） |
| 扫描读取 HEAD 与暂存变化 | `src/command/status/scan.rs:93` |

**Acceptance criteria:**

- [ ] AC-1 扫描预算覆盖为 5000、工作树含 20 000 个未跟踪文件时，结果的 `complete` 为 `false`。
- [ ] AC-2 同一调用中收集器记录的已保留路径总数（debug 计数器）≤ 5000。
- [ ] AC-3 单元门：以每个动态数组各 2000 项构造的状态数据经有界 JSON 构建后，所列 16 个数组（`staged.new`、`staged.modified`、`staged.deleted`、`staged.renamed`、`unstaged.new`、`unstaged.intent_to_add`、`unstaged.modified`、`unstaged.deleted`、`unstaged.renamed`、`unmerged`、`untracked`、`ignored`、`warnings`、`renames`、`io_blocked`、`merge_state.conflicted_paths`）的长度都 ≤ 1000。
- [ ] AC-4 同一单元门中 `omitted_count` 对这 16 个数组路径各有一项且值为 1000。
- [ ] AC-5 无任何省略的小 fixture 上，有界结果与 `libra --json status` 的 `data` 完全相同（因此不含 `omitted_count`）。
- [ ] AC-6 真实 fixture：暂存 2000 次改名时有界结果的 `renames` 长度为 1000。
- [ ] AC-7 真实 fixture（Unix）：1500 个不可读目录时有界结果的 `io_blocked` 长度为 1000。
- [ ] AC-8 同一 20 000 未跟踪文件仓库中 `libra --json status` 的 `untracked` 长度仍为 20000（CLI 不变）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_status_bounded_test scan_budget_marks_incomplete -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_status_bounded_test scan_budget_bounds_retained_paths -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib command::status::tests::bounded_caps_every_dynamic_array -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --lib command::status::tests::bounded_reports_omitted_per_array -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_status_bounded_test no_omission_matches_cli_shape -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_status_bounded_test bounded_caps_real_renames -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_status_bounded_test bounded_caps_real_io_blocked -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test command_test command::status_test::cli_status_unbounded_unchanged -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] 测试索引：`rg -q '^\| `agent_mcp_status_bounded_test` ' tests/INDEX.md && rg -q 'name = "agent_mcp_status_bounded_test"' Cargo.toml`

**Full-suite trigger:** `T-1: GC-02 意义上的共享 status 收集器与 Cargo.toml [[test]] 行`

**Dependencies:** `MC-37`（发布窗口顺序）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/status.rs`（有界入口与输出上限）、`src/command/status/scan.rs`（共享路径预算）、`src/command/status_untracked.rs`（目录读取改用剩余预算）、`src/command/status/output.rs`（每数组上限与 `omitted_count`）、`tests/agent_mcp_status_bounded_test.rs`（新）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`、`tests/command/status_test.rs`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——新增内部入口，`libra status` 的参数与输出不变；MCP 侧的 `omitted_count` 由 MC-02 在工具文档中说明；`COMPATIBILITY.md`、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 大仓库的 MCP status 在扫描阶段与输出阶段都有界。

**Performance budget:** 保留路径 ≤ 100 000；每个数组 ≤ 1000 条；扫描 O(min(工作树, 预算))。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=status 有界收集（扫描路径预算 + 全部动态数组上限）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=4; prod-files=4; scope=M; deps=MC-37,DEP-MC-03; writeset=序列化于 MC-37; release=independent; split-from=MC-02; exception=N/A`

### Task MC-34: 结果信封的处理结果回报与 `finalize` 后重新计量

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-30 前置拆出（R28）。`mcp_server/output.rs` 的结果信封改为返回 `EnvelopeOutcome { redacted: bool, truncated: bool }`（`redacted` 表示脱敏对任何字段做过替换），并为工具提供 `finalize(outcome, &mut structured)` 回调；回调之后信封**重新序列化并重新计量完整 JSON-RPC 响应**（两份内容），超过 256 KiB 即返回工具错误 `LBR-AGENT-028`，回调之后不再改写内容。另注册仅在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时存在的 `libra_test_finalize`（在 `finalize` 中追加 `pad` 字节的字段，并在结果中回报收到的 `outcome`）。唯一行为轴：「信封处理结果与最终计量」。

**Out of scope:** `review_complete` 的判定（由 MC-30 承接）；脱敏规则与截断规则本身（MC-16、MC-19 已交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 结果信封（MC-16、MC-19 交付） | `src/internal/ai/mcp_server/output.rs` |
| bridge 的 256 KiB 上限先例 | `src/internal/ai/agent_bridge/protocol.rs:47` |

**Acceptance criteria:**

- [ ] AC-1 `libra_test_finalize` 的输入含 fixture secret 时，回报的 `outcome.redacted` 为 `true`。
- [ ] AC-2 输入不含 secret 时回报的 `outcome.redacted` 为 `false`。
- [ ] AC-3 信封截断了可截断字段时回报的 `outcome.truncated` 为 `true`。
- [ ] AC-4 `finalize` 前完整响应距 256 KiB 不足 16 字节、`pad = 64` 时，最终结果为工具错误 `LBR-AGENT-028`。
- [ ] AC-5 同一构造下 `pad = 0` 时，最终响应行字节数 ≤ 262144。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_envelope_test finalize_reports_redacted -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_envelope_test finalize_reports_not_redacted -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_envelope_test finalize_reports_truncated -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_envelope_test finalize_remeasures_over_cap -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_envelope_test finalize_under_cap_fits_wire_limit -- --exact`（new）

**Full-suite trigger:** `T-5: 计划级 GC-MC-01 要求每张实现卡的 C 组全量包含全部 `agent_bridge_*` target 与工具 schema 一致性门（`agent_mcp_conformance_test`），本卡按 T-5 执行全量`

**Dependencies:** `MC-27`（发布窗口顺序）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/output.rs`（`EnvelopeOutcome`、`finalize`、重新计量）、`src/internal/ai/mcp_server/tools/mod.rs`（测试工具）、`tests/agent_mcp_envelope_test.rs`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——信封内部接口；用户可见的 `review_complete` 语义由 MC-30 文档化；`COMPATIBILITY.md`、命令文档、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** `finalize` 不能让最终响应越过 256 KiB；测试工具只在 debug 且 `LIBRA_TEST=1` 时存在。

**Performance budget:** 每次调用多一次序列化计量。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=信封处理结果回报与 finalize 后重新计量; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=2; prod-files=2; scope=M; deps=MC-27,DEP-MC-03; writeset=序列化于 MC-27; release=independent; split-from=MC-30; exception=N/A`

### Task MC-35: 读子进程的内存上限

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-29 拆出（R29）。为每个读子进程施加内存上限（默认 512 MiB；仅在 `cfg(debug_assertions)` 且 `LIBRA_TEST=1` 时可由 `LIBRA_MCP_TEST_WORKER_MEM_MB` 覆盖）：Linux 上在子进程 `exec` 前以 `setrlimit(RLIMIT_AS)` 设硬上限；macOS（无可靠的地址空间硬上限）由父进程每 50 ms 采样子进程常驻内存（`proc_pid_rusage`），超过上限即 `SIGKILL` 其进程组；Windows 以 Job Object 的 `JOB_OBJECT_LIMIT_PROCESS_MEMORY` 设硬上限。超过上限时服务回复工具错误 `LBR-AGENT-028`（`structuredContent.reason = "worker_memory_limit"`），回收后归还槽位。本卡是**纵深防御**：读路径的主要内存上限来自算法预算（MC-31/MC-37 的 diff 预算、MC-33 的 status 扫描与输出预算），本卡防御未预见的分配。保证范围如实限定：Linux 的 `RLIMIT_AS` 与 Windows 的进程级限制作用于读子进程本身，macOS 为采样上限（两次采样之间可超出）；读子进程的只读预检把 worktree I/O 执行器配置为**纯进程内模式**（`src/internal/worktree_io/executor.rs` 的 `WorkerConfig` 新增 `in_process_only`，所有请求走既有的 `run_in_process` 路径，不派生 I/O 辅助进程），因此读子进程内的 status 不产生任何后代进程：超时终止与内存上限都作用于完整的读工作进程树（CLI 的 status 仍使用辅助进程池，行为不变）。另注册仅在 debug 且 `LIBRA_TEST=1` 时存在的 `libra_test_alloc`（在读子进程内逐步分配 `mb` MiB 并保持）。唯一行为轴：「读子进程内存上限」。

**Out of scope:** 读子进程的时间上限与只读预检（MC-29 已交付）；各读路径自身的结果上限（MC-31、MC-33）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 读子进程执行器（MC-29 交付） | `src/internal/ai/mcp_server/exec.rs` |
| status 扫描以 `usize::MAX` 读取目录并先构建路径向量 | `src/command/status_untracked.rs:76`、`:401` |
| diff 引擎先构建完整 `DiffOutput` | `src/command/diff/compare.rs:256`；`src/internal/ai/agent_bridge/vcs.rs:232` |

**Acceptance criteria:**

- [ ] AC-1 上限覆盖为 256 MiB 时，`libra_test_alloc {"mb": 1024}` 得到工具错误 `LBR-AGENT-028`。
- [ ] AC-2 同一结果的 `structuredContent.reason` 等于 `"worker_memory_limit"`。
- [ ] AC-3 同一调用后 1 s 内该读子进程（PID 文件所列）已不存在。
- [ ] AC-4 同一调用中（`libra_test_alloc` 以每 10 ms 8 MiB 的受控速率分配），执行器记录的子进程最大常驻内存 ≤ 上限 256 MiB + 文档化采样容差 128 MiB（macOS 两次采样之间可超出；Linux/Windows 为硬上限）。
- [ ] AC-5 4 个并发的同类调用全部返回该错误后 2 s 内，`libra_test_sleep {"ms": 10}` 成功。
- [ ] AC-6 读子进程内的 `libra_status` 停在 `LIBRA_TEST=1` 暂停点时，该读子进程没有任何后代进程（I/O 请求全部在进程内执行）。
- [ ] AC-7 同一状态下读 deadline 覆盖为 2 s 使调用超时后 1 s 内，以该读子进程为祖先的进程一个也不存在。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_exec_test worker_memory_limit_is_tool_error -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_exec_test worker_memory_limit_reason -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_exec_test worker_over_memory_is_killed -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_exec_test worker_peak_memory_bounded -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_exec_test slots_return_after_memory_kills -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_exec_test read_worker_status_spawns_no_helpers -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_exec_test read_worker_status_tree_gone_after_timeout -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/error-codes.md`：`rg -qF -- 'read worker memory' docs/error-codes.md && rg -qF -- 'worker_memory_limit' docs/error-codes.md`
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含 `docs/error-codes.md` 的 diff，评审结论须确认两张表中 `LBR-AGENT-028` 的补充描述与本卡说明一致

**Full-suite trigger:** `T-5: 计划级 GC-MC-01 要求每张实现卡的 C 组全量包含全部 `agent_bridge_*` target 与工具 schema 一致性门；本卡另改共享读执行器（GC-02）`

**Dependencies:** `MC-29`（写集串行：`exec.rs`、`tools/mod.rs`、`agent_mcp_exec_test.rs`）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/exec.rs`（上限设置与采样）、`src/internal/worktree_io/executor.rs`（`in_process_only`）、`src/internal/ai/mcp_server/tools/mod.rs`（测试工具）、`tests/agent_mcp_exec_test.rs`、`docs/error-codes.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** `docs/error-codes.md` 两张表中 `LBR-AGENT-028` 的描述补充「或 MCP 读子进程超过内存上限（`reason = worker_memory_limit`）」（英文措辞含 `read worker memory` 与 `worker_memory_limit`，验收见「同卡文档门」）；命令文档：N/A（不新增参数或工具，错误码含义以 `docs/error-codes.md` 为准）；`COMPATIBILITY.md`、网站页：N/A（无新参数）

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 读子进程（不含任何后代进程）的意外大分配被终止（Linux/Windows 为硬上限，macOS 为采样上限）；主要上限来自各读路径的算法预算；测试工具只在 debug 且 `LIBRA_TEST=1` 时存在。

**Performance budget:** 每个读子进程 ≤ 512 MiB；macOS 采样周期 50 ms。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=读子进程内存上限; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=7/8; VER=7/8; landing=2; prod-files=2; scope=M; deps=MC-29,DEP-MC-03; writeset=序列化于 MC-29; release=independent; split-from=MC-29; exception=N/A`

### Task MC-36: 工作树 blob 字节的有界读取（保持符号链接与 LFS 语义）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-32 拆出（R29）。在 `src/command/mod.rs` 的 `read_worktree_blob_bytes`（`:359`）旁新增 `read_worktree_blob_bytes_bounded(path, limit)`，与现有函数**同一 blob 语义**：路径为符号链接时返回链接目标字节、绝不跟随（Unix 以 `O_NOFOLLOW` 打开、遇链接改为 `read_link`；Windows 以 `FILE_FLAG_OPEN_REPARSE_POINT`）；其余情形先以不跟随方式**打开一次**句柄，此后分类（`fstat` 判定普通文件）与普通文件正文读取都只使用**这同一个句柄**（LFS 跟踪路径的句柄交给 MC-39 的指针生成）：普通文件从该句柄读取至多 `limit + 1` 字节，超过即返回 `TooLarge`。因此打开之后路径被替换（含替换为指向外部的符号链接）不会改变读取的对象，消除增长/替换竞态。上限对实际返回的那种表示（链接目标、指针或正文）生效。现有 `read_worktree_blob_bytes` 与 CLI 不变。唯一行为轴：「工作树 blob 有界读取」。

**Out of scope:** 对象尺寸 helper（MC-32 已交付）；MCP diff 有界入口（由 MC-31 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现行工作树 blob 读取（符号链接读目标、LFS 生成指针、普通文件 `fs::read`） | `src/command/mod.rs:359-370` |
| LFS 指针按路径哈希与取尺寸 | `src/utils/lfs.rs:64-67`（`generate_pointer_file`）、`:74-80`（`generate_pointer_file_result`） |

**Acceptance criteria:**

- [ ] AC-1 2 MiB 普通文件、上限 1 MiB 时返回 `TooLarge`。
- [ ] AC-2 同一调用从句柄读取的字节数 ≤ 1048577。
- [ ] AC-3 竞态门：文件在打开前为 512 KiB、打开后（`LIBRA_TEST=1` 暂停点）被追加到 3 MiB 时，返回 `TooLarge`。
- [ ] AC-4 指向仓库外 2 MiB 文件的符号链接返回链接目标路径字节（与 `read_worktree_blob_bytes` 相同），而非目标文件内容。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib command::tests::bounded_read_rejects_large_file -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --lib command::tests::bounded_read_large_file_reads_at_most_limit -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib command::tests::bounded_read_rejects_concurrent_growth -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --lib command::tests::bounded_read_symlink_returns_target_bytes -- --exact`（new）

**Full-suite trigger:** `T-1: GC-02 意义上的共享工作树读取 helper`

**Dependencies:** `MC-32`（写集串行：`src/command/mod.rs`）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/mod.rs`（新增 `read_worktree_blob_bytes_bounded`）

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——新增内部 helper，CLI 读取路径、参数与输出不变；`COMPATIBILITY.md`、命令文档、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 有界读取不跟随符号链接，不会读出仓库外内容；超大或增长中的文件不会整体读入内存。

**Performance budget:** 普通文件读取 ≤ 上限 + 1 字节；LFS 哈希读取 ≤ `lfs_hash_limit` + 1 字节（MCP 为 64 MiB）。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=工作树 blob 有界读取（符号链接语义保持）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=MC-32,DEP-MC-03; writeset=序列化于 MC-32; release=independent; split-from=MC-32; exception=N/A`

### Task MC-37: MCP diff 的全局收集预算（patch 字节与文件条目）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-31 拆出（R30）。在 MC-31 的有界入口中，逐文件构造结果时维护两个全局预算：① **patch 字节预算** 256 KiB——累计保留的 patch 字节超过后，其余文件不再读取正文、不再计算 patch（记 `patch_omitted = true`、`omitted_reason = "budget"`）；② **文件条目预算** 1000——已构造 1000 个文件条目后停止构造新条目，只累计 `files_omitted`（结果顶层字段，只有发生省略时出现）。③ **条目整体预算**——每个文件条目（`src/command/diff.rs:480` 的结构，含路径、状态、统计、`hunks` 与内存中的 `raw_diff`）构造后按其**保留字节数**计入一个 1 MiB 的元数据预算——保留字节数 = JSON 序列化后（含转义）的字节数 + `raw_diff` 的字节数（`raw_diff` 在序列化时被跳过，`src/command/diff.rs:486-487`，因此单独计入）+ 保留的 `hunks` 内容字节数，超过即停止构造新条目并累计 `files_omitted`；未保留 patch 的条目在计入前丢弃其 `hunks` 与 `raw_diff`（置空）。因此完整 `DiffOutput` 的内存与序列化大小都有界（patch ≤ 256 KiB，其余 ≤ 1 MiB），不依赖之后 bridge 的页与预算处理。唯一行为轴：「diff 全局收集预算」。

**Out of scope:** 单文件上限、rename 关闭与快照参数（MC-31 已交付）；MCP 工具接线（MC-05）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现行引擎先构建完整 `DiffOutput`，bridge 之后才按页与预算处理 | `src/command/diff/compare.rs:256`；`src/internal/ai/agent_bridge/vcs.rs:232` |

**Acceptance criteria:**

- [ ] AC-1 单元门：2000 个各有 1 KiB patch 的文件经有界入口时，保留的 patch 字节总数 ≤ 262144。
- [ ] AC-2 同一单元门中，未保留 patch 的每个文件 `omitted_reason` 都为 `"budget"`。
- [ ] AC-3 单元门：5000 个改动文件经有界入口时，结果的文件条目数为 1000。
- [ ] AC-4 同一单元门中 `files_omitted` 为 4000。
- [ ] AC-5 单元门：1500 个路径长 4000 字节的改动文件经有界入口时，序列化后的 `DiffOutput` ≤ 1.5 MiB。
- [ ] AC-6 单元门：3000 个路径含大量控制字符（序列化时转义膨胀）的改动文件经有界入口时，序列化后的 `DiffOutput` ≤ 1.5 MiB。
- [ ] AC-7 同一 1500 个长路径文件的 fixture 上运行有界 diff 期间，测试 target 内计数分配器记录的峰值堆字节（测量自调用开始，fixture 已预先生成）≤ 16 MiB（容差含 index 与树载入）。
- [ ] AC-8 同一 3000 个转义路径文件的 fixture 上运行有界 diff 期间，峰值堆字节 ≤ 16 MiB（同一容差）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib command::diff::tests::bounded_patch_budget_caps_bytes -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --lib command::diff::tests::bounded_patch_budget_marks_rest -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib command::diff::tests::bounded_file_budget_caps_entries -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --lib command::diff::tests::bounded_file_budget_counts_omitted -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --lib command::diff::tests::bounded_diff_output_size_with_long_paths -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --lib command::diff::tests::bounded_diff_output_size_with_escaped_paths -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test diff_bounded_alloc_test peak_heap_with_long_paths -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test diff_bounded_alloc_test peak_heap_with_escaped_paths -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] 测试索引：`rg -q '^\| `diff_bounded_alloc_test` ' tests/INDEX.md && rg -q 'name = "diff_bounded_alloc_test"' Cargo.toml`

**Full-suite trigger:** `T-1: GC-02 意义上的共享 diff 引擎与 Cargo.toml [[test]] 行`

**Dependencies:** `MC-31`（写集串行：`src/command/diff.rs`、`compare.rs`）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/diff.rs`、`src/command/diff/compare.rs`、`tests/diff_bounded_alloc_test.rs`（新，含计数 `#[global_allocator]`，单独 target 以免影响其它测试）、`Cargo.toml`（`[[test]]` 行）、`tests/INDEX.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——只影响 MCP 有界入口，CLI 与 bridge 不变；MCP 侧 `files_omitted` 与 `omitted_reason = "budget"` 由 MC-05 在工具文档中说明；`COMPATIBILITY.md`、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 多文件 diff 的结果大小在构造阶段即有界。

**Performance budget:** patch ≤ 256 KiB；条目整体（序列化后）≤ 1 MiB；文件条目 ≤ 1000。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=diff 全局收集预算（patch 字节、条目整体、文件条目）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=2; scope=M; deps=MC-31,DEP-MC-03; writeset=序列化于 MC-31; release=independent; split-from=MC-31; exception=N/A`

### Task MC-38: 当前工作树的 workspace renew 工具（事务绑定、可重放）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-11 拆出（R35，R38 只留 renew）。新增写工具 `libra_workspace_renew`（参数：必填 `operation_id`、`workspace_id`、`lease_fence`，可选 `lease_ttl_ms`（范围 1000..=3600000）；需 `--allow-write`）。renew/release 先按 `workspace_id` 读取 registry 记录，并在调用 store 之前依次校验：其 canonical 路径与 lease 身份（MC-40）等于服务端派生的当前工作树、其 lease owner 等于由可信 MCP actor 派生的 `mcp:mcp~<actor>`（与 bridge 的会话归属检查同义，`src/internal/ai/agent_bridge/workspace.rs:195`），任一不等即工具错误 `LBR-AGENT-032`。期限规则：`lease_ttl_ms` 缺省时取 `agent_mcp_lease` 中该 `workspace_id` 在 claim 时记录的 TTL；调用 `renew_with_conn` 时传入 `max(ttl, 当前到期时间 − now)`，因此 renew 永不缩短既有到期时间。每次 lease 调用在**一个 SQLite 事务**内完成：先写意图与 `pending` operation，再以同一事务连接调用 `src/internal/workspace.rs` 中既有的 `WorkspaceStore::acquire_with_conn`/`renew_with_conn`/`release_with_conn`（`:730`、`:877`、`:959`，已接受任意 `ConnectionTrait`，无需改动 store 与 bridge），并在同一事务内把 operation 记为 `applied`、写入结果；提交前崩溃则整个事务回滚——意图、operation 与 lease 效果都不留下任何行，因此不存在「效果已发生而 operation 仍 pending」的窗口；客户端以同一 `operation_id` 重试时找不到记录，即按该次请求作为新请求执行。提交成功而响应丢失时，记录已存在，重放返回已记录的结果。同一 `operation_id`、同参数的重放返回首次记录的结果，参数不同 → `LBR-AGENT-033`。lease 工具的意图请求另存服务端派生的有界可信工作树身份（MC-40 的 lease 身份 ≤ 128 字节，以及 canonical 根目录的 SHA-256 十六进制摘要，而非路径本身，因此请求大小与路径长度无关）；返回重放结果之前先比较该身份与本次调用所在工作树（同样按 lease 身份与根目录摘要比较），不等即工具错误 `LBR-AGENT-032`（不返回已保存的结果，也不执行），因此在工作树 B 中以工作树 A 的 `operation_id` 重放不会得到 A 的成功结果。对已存在的 lease，renew 使用 claim 时预留的结算额度（MC-06），不新增组、不检查配额；每个 lease 至多 16 次成功的 renew，第 17 次得到工具错误 `LBR-AGENT-035`（`structuredContent.reason = "renew_limit"`，客户端应 release 后重新 claim）；renew 记录从不删除，因此任一 `operation_id` 的重放始终返回其首次结果。所有拒绝条件（工作树/actor 归属、fence、已释放、renew 次数上限）都在写入意图与 operation **之前**判定，被拒调用不写任何行、不占用预留额度。唯一行为轴：「MCP 续期当前工作树的 lease」。

**Out of scope:** claim（MC-11）；release（MC-41）；其它 workspace（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge renew 校验会话归属与 owner/fence | `src/internal/ai/agent_bridge/workspace.rs:179-230` |
| store 续期按 `now + TTL` 设定到期 | `src/internal/workspace.rs:877`（`renew_with_conn`） |
| claim TTL 记录表（MC-03 交付） | `agent_mcp_lease` |

**Acceptance criteria:**

- [ ] AC-1 以 claim 返回的 fence 调用 renew 后 `lease_expires_at` 大于 renew 前的值。
- [ ] AC-2 同一 actor 在工作树 A 中 renew 其在工作树 B 持有的 lease，得到工具错误 `LBR-AGENT-032`。
- [ ] AC-3 另一 actor 在同一工作树中 renew 本 actor 的 lease（给出正确的 `workspace_id` 与 `lease_fence`），得到工具错误 `LBR-AGENT-032`。
- [ ] AC-4 以同一 `operation_id` 重放已成功的 renew，返回的 `lease_expires_at` 等于首次结果中的值。
- [ ] AC-5 在主工作树中 claim 之后以其 fence 调用 renew 成功（`lease_expires_at` 增大）。
- [ ] AC-6 以 `lease_ttl_ms = 3600000` claim，等待 2 s 后不带 `lease_ttl_ms` 调用 renew，新 `lease_expires_at` ≥ renew 调用开始时刻 + 3600000 ms（证明使用了 claim 时的 TTL）。
- [ ] AC-7 以 `lease_ttl_ms = 3600000` claim 后立即以 `lease_ttl_ms = 1000` 调用 renew，renew 后的 `lease_expires_at` 不小于 renew 前的值（显式更短 TTL 不会缩短）。
- [ ] AC-8 在 renew 的 store 写入之后、事务提交之前注入崩溃并重启后，该 lease 的 `lease_expires_at` 等于崩溃前的值。
- [ ] AC-9 同一崩溃并重启后，`agent_mcp_intent` 与 `agent_mcp_operation` 两表中都没有该 `<actor>~<operation_id>` 的行。
- [ ] AC-10 随后以同一 `operation_id`、同参数重试 renew 后，`lease_expires_at` 大于崩溃前的值（重试成功是其前提）。
- [ ] AC-11 同一 actor 在工作树 A 中成功 renew 后，在 linked 工作树 B 中以相同 `operation_id` 与参数重放，得到工具错误 `LBR-AGENT-032`。
- [ ] AC-12 MC-06 的组配额与字节配额都已满时，对已存在 lease 的 renew 成功。
- [ ] AC-13 对同一 lease 第 17 次 renew 得到工具错误 `LBR-AGENT-035`（`reason = "renew_limit"`）。
- [ ] AC-14 在 MC-06 的组配额与字节配额都已满时，对同一 lease 以 20 个新 `operation_id` 重复发出第 17 次 renew，全部被拒后 `agent_mcp_*` 各表行数与 `agent_mcp_usage` 都不变。
- [ ] AC-15 请求 JSON 超过 1 KiB（以超长的 `workspace_id` 构造）的 renew 在记录意图之前得到工具错误 `LBR-AGENT-027`。

**判据规范（非计数正文）:** 门族（15 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`agent_mcp_workspace_lease_test::renew_extends_expiry`；AC-2=`agent_mcp_workspace_lease_test::renew_other_worktree_is_032`；AC-3=`agent_mcp_workspace_lease_test::renew_other_actor_is_032`；AC-4=`agent_mcp_workspace_lease_test::renew_replay_returns_original_expiry`；AC-5=`agent_mcp_workspace_lease_test::renew_in_main_worktree`；AC-6=`agent_mcp_workspace_lease_test::renew_default_uses_claim_ttl`；AC-7=`agent_mcp_workspace_lease_test::renew_explicit_shorter_ttl_never_shortens`；AC-8=`agent_mcp_workspace_lease_test::renew_crash_before_commit_keeps_expiry`；AC-9=`agent_mcp_workspace_lease_test::renew_crash_rolls_back_intent_and_operation`；AC-10=`agent_mcp_workspace_lease_test::renew_retry_after_crash_succeeds`；AC-11=`agent_mcp_workspace_lease_test::renew_replay_in_other_worktree_is_032`；AC-12=`agent_mcp_workspace_lease_test::renew_allowed_at_quota`；AC-13=`agent_mcp_workspace_lease_test::seventeenth_renew_is_renew_limit`；AC-14=`agent_mcp_workspace_lease_test::repeated_denied_renews_add_no_rows`；AC-15=`agent_mcp_workspace_lease_test::renew_oversized_request_is_027`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_extends_expiry -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_other_worktree_is_032 -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_other_actor_is_032 -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_replay_returns_original_expiry -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_in_main_worktree -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_default_uses_claim_ttl -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_explicit_shorter_ttl_never_shortens -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_crash_before_commit_keeps_expiry -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_crash_rolls_back_intent_and_operation -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_retry_after_crash_succeeds -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_replay_in_other_worktree_is_032 -- --exact`（new）
- [ ] AC-12：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_allowed_at_quota -- --exact`（new）
- [ ] AC-13：`source .env.test && cargo test --test agent_mcp_workspace_lease_test seventeenth_renew_is_renew_limit -- --exact`（new）
- [ ] AC-14：`source .env.test && cargo test --test agent_mcp_workspace_lease_test repeated_denied_renews_add_no_rows -- --exact`（new）
- [ ] AC-15：`source .env.test && cargo test --test agent_mcp_workspace_lease_test renew_oversized_request_is_027 -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_workspace_renew' docs/commands/agent.md && rg -qF -- 'renew_limit' docs/commands/agent.md && rg -qF -- 'workspace_id' docs/commands/agent.md && rg -qF -- 'lease_ttl_ms' docs/commands/agent.md && rg -qF -- 'never shortens' docs/commands/agent.md`
- [ ] `docs/commands/agent.md` 的 `libra_workspace_renew` 小节（标题文本为反引号包裹的 `libra_workspace_renew`，至下一个同级或更高级标题）：`python3 -I tests/tools/doc_section_check.py --markdown libra_workspace_renew docs/commands/agent.md '1 KiB' 'LBR-AGENT-027' 'operation_id' 'workspace_id' 'lease_fence' 'lease_ttl_ms'`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_workspace_renew' docs/commands/zh-CN/agent.md && rg -qF -- 'renew_limit' docs/commands/zh-CN/agent.md && rg -qF -- 'workspace_id' docs/commands/zh-CN/agent.md && rg -qF -- 'lease_ttl_ms' docs/commands/zh-CN/agent.md && rg -qF -- '不会缩短' docs/commands/zh-CN/agent.md`
- [ ] `docs/commands/zh-CN/agent.md` 的 `libra_workspace_renew` 小节（标题文本为反引号包裹的 `libra_workspace_renew`，至下一个同级或更高级标题）：`python3 -I tests/tools/doc_section_check.py --markdown libra_workspace_renew docs/commands/zh-CN/agent.md '1 KiB' 'LBR-AGENT-027' 'operation_id' 'workspace_id' 'lease_fence' 'lease_ttl_ms'`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_workspace_renew'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`（`libra_workspace_renew` 小节内：标题为 `####` 加反引号包裹的工具名）：`python3 -I tests/tools/doc_section_check.py --markdown libra_workspace_renew ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md '1 KiB' 'LBR-AGENT-027' 'operation_id' 'workspace_id' 'lease_fence' 'lease_ttl_ms' 'never shortens'`；GC-MC-05 ①–⑤ 交付后线上页面（DOM 小节：自 `id` 为 `libra-workspace-renew` 的标题元素至下一个同级或更高级标题元素，不按行切分）：`curl -fsS https://libra.tools/en/docs/commands/agent | python3 -I tests/tools/doc_section_check.py --id libra-workspace-renew '1 KiB' 'LBR-AGENT-027' 'operation_id' 'workspace_id' 'lease_fence' 'lease_ttl_ms' 'never shortens'`（GC-MC-05 ⑥）
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-5: 计划级 GC-MC-01 要求每张实现卡的 C 组全量包含全部 `agent_bridge_*` target 与工具 schema 一致性门`

**Dependencies:** `MC-11`（写集串行：`tools/workspace.rs`、`agent_mcp_workspace_lease_test.rs`、agent 文档）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/tools/workspace.rs`、`tests/agent_mcp_workspace_lease_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：在以 `####` 加反引号工具名为标题的小节中新增 `libra_workspace_renew`（必填 `operation_id`、`workspace_id`、`lease_fence`；可选 `lease_ttl_ms`，缺省取 claim 时的 TTL；renew 不会缩短既有到期时间（英文措辞 `never shortens`）；每个 lease 至多 16 次 renew，超出为 `LBR-AGENT-035`（`renew_limit`）；请求 JSON 上限 1 KiB，超出在记录前以 `LBR-AGENT-027` 拒绝；只作用于当前工作树与本 actor 的 lease；重放语义）；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A（未新增错误码）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 只能续期当前工作树中本 actor 的 lease；效果与 operation 记录原子提交。

**Performance budget:** 每次调用一个 SQLite 事务。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP 续期当前工作树的 lease（事务绑定、可重放、不缩短）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=15/8@EX-MC-10; VER=15/8@EX-MC-10; landing=1; prod-files=1; scope=M; deps=MC-11,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-11; release=independent; split-from=MC-11; exception=EX-MC-10`

### Task MC-41: 当前工作树的 workspace release 工具（事务绑定、可重放）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-38 拆出（R38）。新增写工具 `libra_workspace_release`（参数：必填 `operation_id`、`workspace_id`、`lease_fence`；需 `--allow-write`）。renew/release 先按 `workspace_id` 读取 registry 记录，并在调用 store 之前依次校验：其 canonical 路径与 lease 身份（MC-40）等于服务端派生的当前工作树、其 lease owner 等于由可信 MCP actor 派生的 `mcp:mcp~<actor>`（与 bridge 的会话归属检查同义，`src/internal/ai/agent_bridge/workspace.rs:195`），任一不等即工具错误 `LBR-AGENT-032`。每次 lease 调用在**一个 SQLite 事务**内完成：先写意图与 `pending` operation，再以同一事务连接调用 `src/internal/workspace.rs` 中既有的 `WorkspaceStore::acquire_with_conn`/`renew_with_conn`/`release_with_conn`（`:730`、`:877`、`:959`，已接受任意 `ConnectionTrait`，无需改动 store 与 bridge），并在同一事务内把 operation 记为 `applied`、写入结果；提交前崩溃则整个事务回滚——意图、operation 与 lease 效果都不留下任何行，因此不存在「效果已发生而 operation 仍 pending」的窗口；客户端以同一 `operation_id` 重试时找不到记录，即按该次请求作为新请求执行。提交成功而响应丢失时，记录已存在，重放返回已记录的结果。同一 `operation_id`、同参数的重放返回首次记录的结果，参数不同 → `LBR-AGENT-033`。lease 工具的意图请求另存服务端派生的有界可信工作树身份（MC-40 的 lease 身份 ≤ 128 字节，以及 canonical 根目录的 SHA-256 十六进制摘要，而非路径本身，因此请求大小与路径长度无关）；返回重放结果之前先比较该身份与本次调用所在工作树（同样按 lease 身份与根目录摘要比较），不等即工具错误 `LBR-AGENT-032`（不返回已保存的结果，也不执行），因此在工作树 B 中以工作树 A 的 `operation_id` 重放不会得到 A 的成功结果。对已存在的 lease，release 使用 claim 时预留的结算额度（MC-06），不新增组、不检查配额（每个 lease 至多一条成功的 release 记录）。所有拒绝条件（工作树/actor 归属、fence、已释放）都在写入意图与 operation **之前**判定；renew 次数上限只作用于 renew，**release 始终可用**（16 次 renew 之后仍可 release），被拒调用不写任何行、不占用预留额度。唯一行为轴：「MCP 释放当前工作树的 lease」。

**Out of scope:** claim（MC-11）、renew（MC-38）；其它 workspace（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| bridge release 校验会话归属与 owner/fence | `src/internal/ai/agent_bridge/workspace.rs:195-268` |
| store 释放 | `src/internal/workspace.rs:959`（`release_with_conn`） |

**Acceptance criteria:**

- [ ] AC-1 同一 actor 在工作树 A 中 release 其在工作树 B 持有的 lease，得到工具错误 `LBR-AGENT-032`。
- [ ] AC-2 另一 actor 在同一工作树中 release 本 actor 的 lease（给出正确的 `workspace_id` 与 `lease_fence`），得到工具错误 `LBR-AGENT-032`。
- [ ] AC-3 以同一 `operation_id` 重放已成功的 release，返回首次的成功结果（不是 `LBR-AGENT-023`）。
- [ ] AC-4 在 release 的 store 写入之后、事务提交之前注入崩溃并重启后，该 lease 在 registry 中仍为存活。
- [ ] AC-5 同一 release 崩溃并重启后，`agent_mcp_intent` 与 `agent_mcp_operation` 两表中都没有该 `<actor>~<operation_id>` 的行。
- [ ] AC-6 随后以同一 `operation_id`、同参数重试 release 后，该 lease 在 registry 中不再存活（重试成功是其前提）。
- [ ] AC-7 同一 actor 在工作树 A 中成功 release 后，在 linked 工作树 B 中以相同 `operation_id` 与参数重放，得到工具错误 `LBR-AGENT-032`。
- [ ] AC-8 MC-06 的组配额与字节配额都已满时，对已存在 lease 的 release 成功。
- [ ] AC-9 对已释放的 lease 以 20 个新 `operation_id` 重复调用 release，全部被拒后 `agent_mcp_*` 各表行数与 `agent_mcp_usage` 都不变。
- [ ] AC-10 以接近 1 KiB 的最大尺寸请求完成 16 次 renew 与 1 次 release 之后，`agent_mcp_usage.payload_bytes` 等于 claim 刚完成时的值（结算全部落在预留额度内）。
- [ ] AC-11 在 canonical 根目录长约 3000 字节的 linked 工作树中依次调用 claim、renew、release，三次都成功。
- [ ] AC-12 linked worktree 的 `worktree_id` 元数据为 4000 字节（不含 `:`）时，在该 worktree 中依次调用 claim、renew、release，三次都成功。
- [ ] AC-13 仓库身份为 3000 字节时，在主工作树中依次调用 claim、renew、release，三次都成功。

**判据规范（非计数正文）:** 门族（13 个，逐一列名；第 i 个门只判定 AC-i 一个谓词，失败即整卡不达标，`--exact` 门按 GC-MC-07 执行）：AC-1=`agent_mcp_workspace_lease_test::release_other_worktree_is_032`；AC-2=`agent_mcp_workspace_lease_test::release_other_actor_is_032`；AC-3=`agent_mcp_workspace_lease_test::release_replay_returns_original_result`；AC-4=`agent_mcp_workspace_lease_test::release_crash_before_commit_keeps_lease`；AC-5=`agent_mcp_workspace_lease_test::release_crash_rolls_back_intent_and_operation`；AC-6=`agent_mcp_workspace_lease_test::release_retry_after_crash_succeeds`；AC-7=`agent_mcp_workspace_lease_test::release_replay_in_other_worktree_is_032`；AC-8=`agent_mcp_workspace_lease_test::release_allowed_at_quota`；AC-9=`agent_mcp_workspace_lease_test::repeated_denied_releases_add_no_rows`；AC-10=`agent_mcp_workspace_lease_test::max_size_settlement_fits_reservation`；AC-11=`agent_mcp_workspace_lease_test::long_worktree_path_settles`；AC-12=`agent_mcp_workspace_lease_test::long_metadata_worktree_id_settles`；AC-13=`agent_mcp_workspace_lease_test::long_repository_id_settles`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_workspace_lease_test release_other_worktree_is_032 -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_workspace_lease_test release_other_actor_is_032 -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_workspace_lease_test release_replay_returns_original_result -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_workspace_lease_test release_crash_before_commit_keeps_lease -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_workspace_lease_test release_crash_rolls_back_intent_and_operation -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_workspace_lease_test release_retry_after_crash_succeeds -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_workspace_lease_test release_replay_in_other_worktree_is_032 -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_workspace_lease_test release_allowed_at_quota -- --exact`（new）
- [ ] AC-9：`source .env.test && cargo test --test agent_mcp_workspace_lease_test repeated_denied_releases_add_no_rows -- --exact`（new）
- [ ] AC-10：`source .env.test && cargo test --test agent_mcp_workspace_lease_test max_size_settlement_fits_reservation -- --exact`（new）
- [ ] AC-11：`source .env.test && cargo test --test agent_mcp_workspace_lease_test long_worktree_path_settles -- --exact`（new）
- [ ] AC-12：`source .env.test && cargo test --test agent_mcp_workspace_lease_test long_metadata_worktree_id_settles -- --exact`（new）
- [ ] AC-13：`source .env.test && cargo test --test agent_mcp_workspace_lease_test long_repository_id_settles -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'libra_workspace_release' docs/commands/agent.md && rg -qF -- 'lease_fence' docs/commands/agent.md && rg -qF -- 'workspace_id' docs/commands/agent.md`
- [ ] `docs/commands/agent.md` 的 `libra_workspace_release` 小节（标题文本为反引号包裹的 `libra_workspace_release`，至下一个同级或更高级标题）：`python3 -I tests/tools/doc_section_check.py --markdown libra_workspace_release docs/commands/agent.md 'operation_id' 'workspace_id' 'lease_fence'`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'libra_workspace_release' docs/commands/zh-CN/agent.md && rg -qF -- 'lease_fence' docs/commands/zh-CN/agent.md && rg -qF -- 'workspace_id' docs/commands/zh-CN/agent.md`
- [ ] `docs/commands/zh-CN/agent.md` 的 `libra_workspace_release` 小节（标题文本为反引号包裹的 `libra_workspace_release`，至下一个同级或更高级标题）：`python3 -I tests/tools/doc_section_check.py --markdown libra_workspace_release docs/commands/zh-CN/agent.md 'operation_id' 'workspace_id' 'lease_fence'`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'libra_workspace_release'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`（`libra_workspace_release` 小节内：标题为 `####` 加反引号包裹的工具名）：`python3 -I tests/tools/doc_section_check.py --markdown libra_workspace_release ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md 'operation_id' 'workspace_id' 'lease_fence'`；GC-MC-05 ①–⑤ 交付后线上页面（DOM 小节：自 `id` 为 `libra-workspace-release` 的标题元素至下一个同级或更高级标题元素，不按行切分）：`curl -fsS https://libra.tools/en/docs/commands/agent | python3 -I tests/tools/doc_section_check.py --id libra-workspace-release 'operation_id' 'workspace_id' 'lease_fence'`（GC-MC-05 ⑥）
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-5: 计划级 GC-MC-01 要求每张实现卡的 C 组全量包含全部 `agent_bridge_*` target 与工具 schema 一致性门`

**Dependencies:** `MC-38`（写集串行：`tools/workspace.rs`、`agent_mcp_workspace_lease_test.rs`、agent 文档）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/tools/workspace.rs`、`tests/agent_mcp_workspace_lease_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：在以 `####` 加反引号工具名为标题的小节中新增 `libra_workspace_release`（必填 `operation_id`、`workspace_id`、`lease_fence`；只作用于当前工作树与本 actor 的 lease；重放返回首次结果）；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A（未新增错误码）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 只能释放当前工作树中本 actor 的 lease；效果与 operation 记录原子提交。

**Performance budget:** 每次调用一个 SQLite 事务。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP 释放当前工作树的 lease（事务绑定、可重放）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=13/8@EX-MC-11; VER=13/8@EX-MC-11; landing=1; prod-files=1; scope=M; deps=MC-38,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-38; release=independent; split-from=MC-38; exception=EX-MC-11`

### Task MC-43: commit 暂存树前提（`expected_tree`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-24 拆出（R59）。增加 `CommitArgs.expected_tree: Option<ObjectHash>`（`#[arg(skip)]`，CLI 不暴露）：`run_commit` 由 index 算出树之后、写提交对象之前与之比较，不等即返回提交错误（fenced 调用映射 `LBR-AGENT-038`）且不写提交对象、ref 与 reflog；相等时提交正使用该树。防止 fence 之后他人 `add` 改变暂存内容。唯一行为轴：「暂存树前提」。

**Out of scope:** HEAD 符号引用前提（MC-24 已交付）；MCP 参数映射（MC-08）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| commit 由 index 构树 | `src/command/commit.rs`（`run_commit`，开工时刷新行号） |

**Acceptance criteria:**

- [ ] AC-1 `expected_tree` 与由 index 算出的树不同时返回错误（单元门：调用前另行 `add` 一个文件）。
- [ ] AC-2 同一不匹配情形下分支 ref 不变。
- [ ] AC-3 `expected_tree` 等于由 index 算出的树时，新提交的树等于 `expected_tree`（提交成功是其前提）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib command::commit::tests::expected_tree_mismatch_errors -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --lib command::commit::tests::expected_tree_mismatch_keeps_ref -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib command::commit::tests::expected_tree_match_commits_that_tree -- --exact`（new）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 commit 核心`

**Dependencies:** `MC-24`（写集串行：`commit.rs`）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/commit.rs`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——`expected_tree` 是 `#[arg(skip)]` 内部字段，`libra commit` 的参数、输出与错误不变；`COMPATIBILITY.md`、命令文档、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 并发 `add` 改变的暂存内容不会进入 MCP 提交。

**Performance budget:** 写提交对象前多一次树 OID 比较，O(1)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=暂存树前提（写提交对象前比较）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=3/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=MC-24,DEP-MC-03; writeset=序列化于 MC-24; release=independent; split-from=MC-24; exception=N/A`

### Task MC-39: LFS 指针的有界生成（同一句柄、原始字节上限）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-36 拆出（R35）。新增 `utils::lfs::generate_pointer_from_reader(handle, lfs_hash_limit)`：对 MC-36 已以不跟随方式打开并以 `fstat` 判定为普通文件的**同一句柄**流式计算 SHA-256 与尺寸，生成指针字节；从句柄读取的原始字节数上限为 `lfs_hash_limit`（MCP 取 64 MiB；仅在 debug 且 `LIBRA_TEST=1` 时可覆盖），超过即停止并返回 `TooLarge`（调用方据此省略 patch）。`read_worktree_blob_bytes_bounded` 对 LFS 跟踪路径改用它。现有按路径的 `generate_pointer_file`（`src/utils/lfs.rs:64-67`，内部经 `generate_pointer_file_result` `:74-80` 按路径哈希与取尺寸）与 CLI 不变。唯一行为轴：「LFS 指针有界生成」。

**Out of scope:** 非 LFS 的工作树有界读取（MC-36 已交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| LFS 指针按路径哈希与取尺寸 | `src/utils/lfs.rs:64-67`（`generate_pointer_file`）、`:74-80`（`generate_pointer_file_result`） |
| 有界读取入口（MC-36 交付） | `src/command/mod.rs`（`read_worktree_blob_bytes_bounded`） |

**Acceptance criteria:**

- [ ] AC-1 LFS 跟踪的 2 MiB 文件，有界读取返回的字节等于 `read_worktree_blob_bytes` 生成的指针字节。
- [ ] AC-2 竞态门：LFS 跟踪的文件在打开之后（`LIBRA_TEST=1` 暂停点）被替换为指向仓库外文件的符号链接时，返回的指针中的 oid 等于原文件内容的 SHA-256。
- [ ] AC-3 LFS 哈希上限覆盖为 2 MiB 时，一个 100 MiB 的 LFS 跟踪文件返回 `TooLarge`。
- [ ] AC-4 同一调用从句柄读取的字节数 ≤ 2097153。
- [ ] AC-5 LFS 跟踪文件在打开后（`LIBRA_TEST=1` 暂停点）由 1 MiB 增长到 10 MiB、哈希上限为 2 MiB 时返回 `TooLarge`。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib command::tests::bounded_read_lfs_returns_pointer -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --lib command::tests::bounded_read_lfs_swap_uses_open_handle -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib command::tests::bounded_read_lfs_hash_cap -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --lib command::tests::bounded_read_lfs_hash_cap_reads_at_most_limit -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --lib command::tests::bounded_read_lfs_growth_hits_cap -- --exact`（new）

**Full-suite trigger:** `T-1: GC-02 意义上的共享 LFS 与工作树读取 helper`

**Dependencies:** `MC-36`（写集串行：`src/command/mod.rs`）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/utils/lfs.rs`（新增 `generate_pointer_from_reader`）、`src/command/mod.rs`（LFS 分支接线）

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——新增内部 helper，CLI 与按路径的 LFS 指针生成不变；`COMPATIBILITY.md`、命令文档、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** LFS 指针只由打开的同一句柄生成，替换为外部链接不会泄露外部文件的哈希与尺寸；哈希读取有上限。

**Performance budget:** LFS 哈希读取 ≤ `lfs_hash_limit` + 1 字节（MCP 为 64 MiB）。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=LFS 指针有界生成（同一句柄、原始字节上限）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=2; prod-files=2; scope=M; deps=MC-36,DEP-MC-03; writeset=序列化于 MC-36; release=independent; split-from=MC-36; exception=N/A`

### Task MC-40: MCP lease 的工作树身份（含主工作树）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-11 前置拆出（R36）。`WorkspaceStore` 的 linked claim 要求非空 worktree id（`src/internal/workspace.rs:1795`），而主工作树的 worktree id 为 `None`（`src/command/worktree.rs:358`）。新增 `mcp_server/tools/workspace.rs::mcp_lease_worktree_id(ctx, conn: &impl ConnectionTrait) -> Result<String, BridgeError>`（`conn` 用于主身份冲突时查询 registry 的 `find_live_linked_with_conn`，`src/internal/workspace.rs:1250`）：linked worktree 返回其 worktree id（由 `util::worktree_instance_id` 生成，字符集只含 `[A-Za-z0-9-]`，`src/utils/util.rs:1209-1225`）；主工作树返回保留身份 `main:<repository_id 的 SHA-256 十六进制>`（69 字节；仓库身份本身无长度上限，`src/internal/workspace.rs:562`，取摘要使身份有界；含 `:`，因此不可能与任何 linked worktree id 相同）。由于 `util::worktree_id_for_gitdir`（`src/utils/util.rs:1177-1190`）会接受 `worktree_id` 元数据文件中的任意非空值，`mcp_lease_worktree_id` 对 linked worktree 读出的 id 再做命名空间校验：含 `:`（因而可能冒用 `main:` 保留身份）即 fail-closed，lease 工具返回工具错误 `LBR-AGENT-032`；不含 `:` 但不满足 `^[A-Za-z0-9-]{1,64}$`（元数据可达 4096 字节，`src/utils/util.rs:1182`）时，lease 身份取 `wt-<该 id 的 SHA-256 十六进制>`（67 字节，字符集仍为 `[A-Za-z0-9-]`），因此任何 lease 身份都 ≤ 128 字节，claim 成功的 lease 之后的 renew/release 请求一定落在 1 KiB 上限内；主工作树 claim 前另查 registry，若存在身份为 `main:<repository_id 的 SHA-256 十六进制>`、路径却不是主工作树根目录的记录，同样以 `LBR-AGENT-032` 拒绝。MC-11/MC-38/MC-41 的 claim、renew、release 都以它作为 lease 身份并在 renew/release 时复核。唯一行为轴：「MCP lease 工作树身份」。

**Out of scope:** lease 工具本身（MC-11、MC-38 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| store 要求 linked claim 携带 worktree id | `src/internal/workspace.rs:1785-1800` |
| 主工作树 id 为 `None` | `src/command/worktree.rs:358-361` |
| linked worktree id 的生成与字符集 | `src/utils/util.rs:1209-1225` |

**Acceptance criteria:**

- [ ] AC-1 单元门：主工作树上下文中 `mcp_lease_worktree_id` 返回 `main:<repository_id 的 SHA-256 十六进制>`。
- [ ] AC-2 单元门：linked worktree 上下文中返回该 worktree 的 id。
- [ ] AC-3 单元门：对 1000 个随机目录名，`util::worktree_instance_id` 的结果都不含 `:`（因此不会与主工作树身份相同）。
- [ ] AC-4 store 层：同一仓库中一个 linked worktree 已有存活 lease 时，以主工作树身份对主工作树根目录做 linked acquire 成功。
- [ ] AC-5 linked worktree 的 `worktree_id` 元数据文件被改写为 `main:<repository_id 的 SHA-256 十六进制>` 时，在该 worktree 中经真实元数据读取路径调用 `mcp_lease_worktree_id` 返回 `LBR-AGENT-032` 错误。
- [ ] AC-6 registry 中存在身份为 `main:<repository_id 的 SHA-256 十六进制>`、路径为某 linked worktree 的 lease 时，主工作树的身份解析返回 `LBR-AGENT-032` 错误。
- [ ] AC-7 linked worktree 的 `worktree_id` 元数据为 4000 字节（不含 `:`）时，`mcp_lease_worktree_id` 返回 `wt-` 加 64 位十六进制的 67 字节身份。
- [ ] AC-8 仓库身份 `libra.repoid` 为 3000 字节时，主工作树的 `mcp_lease_worktree_id` 返回 69 字节的 `main:` 加 64 位十六进制身份。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --lib internal::ai::mcp_server::tools::workspace::tests::main_worktree_lease_id -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --lib internal::ai::mcp_server::tools::workspace::tests::linked_worktree_lease_id -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --lib internal::ai::mcp_server::tools::workspace::tests::linked_ids_never_contain_colon -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --lib internal::ai::mcp_server::tools::workspace::tests::main_identity_acquire_coexists_with_linked -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --lib internal::ai::mcp_server::tools::workspace::tests::reserved_id_in_linked_metadata_fails_closed -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --lib internal::ai::mcp_server::tools::workspace::tests::main_identity_collision_fails_closed -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --lib internal::ai::mcp_server::tools::workspace::tests::long_metadata_id_is_hashed -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --lib internal::ai::mcp_server::tools::workspace::tests::long_repository_id_is_hashed -- --exact`（new）

**Full-suite trigger:** `T-5: 计划级 GC-MC-01 要求每张实现卡的 C 组全量包含全部 `agent_bridge_*` target 与工具 schema 一致性门`

**Dependencies:** `MC-10`（发布窗口顺序）；`DEP-MC-03`

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/tools/workspace.rs`（新增 `mcp_lease_worktree_id`）

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** N/A——内部 helper；用户可见的「主工作树同样可 claim」由 MC-11 写入工具文档；`COMPATIBILITY.md`、网站页、`docs/error-codes.md` 均无变化

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 主工作树身份不可能与 linked worktree 身份冲突；lease 只作用于当前工作树。

**Performance budget:** O(1)。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP lease 工作树身份（含主工作树）; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=MC-10,DEP-MC-03; writeset=序列化于 MC-10; release=independent; split-from=MC-11; exception=N/A`

### Task MC-30: staged diff 审核完整性标志 `review_complete`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 自 MC-27 拆出。`mode = "staged"` 结果增加 `review_complete`，在**最终结果构造完成之后**（含路径过滤、按预算省略 patch、文件页上限、二进制、MC-31 的单文件上限、MC-16 脱敏与 MC-19 截断）计算：仅当未给 `paths`、所有文件都有完整文本 patch（无 `patch_omitted`、无二进制）、文件数未超页上限且无 `files_omitted`（MC-37）、脱敏未发生任何替换且 `truncated` 不为 `true` 时为 `true`，其余一律 `false`。脱敏与截断的事实来自 MC-34 的 `EnvelopeOutcome`：`libra_diff` 在 MC-34 的 `finalize` 回调中写入 `review_complete`（信封已发生任何替换或截断即 `false`）；回调之后信封重新计量且不再改写内容，因此线上最终响应中的 `review_complete` 与其实际内容一致，且不会越过 256 KiB。文档写明：`review_complete = false` 时 MCP 结果不是完整审核依据，须由人在本机 `libra diff --cached` 审核后再提交。唯一行为轴：「审核完整性」。

**Out of scope:** 快照身份字段（MC-27 已交付）；提交时的服务端拒绝（MC-08）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 结果构造中的预算与页上限 | `src/internal/ai/agent_bridge/vcs.rs:236-260` |
| 路径过滤参数（MC-05） | `libra_diff` 的 `paths` |

**Acceptance criteria:**

- [ ] AC-1 暂存改动不含 secret、未给 `paths` 且未截断时，`review_complete` 为 `true`。
- [ ] AC-2 暂存改动含 fixture secret 时 `review_complete` 为 `false`。
- [ ] AC-3 另有暂存文件 `b.txt` 而请求 `paths = ["a.txt"]` 时 `review_complete` 为 `false`。
- [ ] AC-4 某文件的 patch 因预算被省略（`patch_omitted = true`）时 `review_complete` 为 `false`。
- [ ] AC-5 暂存文件数超过页上限时 `review_complete` 为 `false`。
- [ ] AC-6 暂存一个二进制文件时 `review_complete` 为 `false`。
- [ ] AC-7 暂存改动的 patch 超过 256 KiB 致结果 `truncated = true` 时 `review_complete` 为 `false`。
- [ ] AC-8 文件路径本身含 fixture secret（patch 正文不含）时，最终线上响应中的 `review_complete` 为 `false`（信封对任何字段的替换都计入）。
- [ ] ER-06/ER-06a 同卡强制门：下方「同卡文档门」逐文件全部通过；网站页另按 GC-MC-05 ①–⑥ 交付（不计入 AC 上限）。

**Verification:**（本卡特有判据，计入 G-03；`--exact` 门按 GC-MC-07 执行）

- [ ] AC-1：`source .env.test && cargo test --test agent_mcp_diff_test review_complete_true_for_full_clean_patch -- --exact`（new）
- [ ] AC-2：`source .env.test && cargo test --test agent_mcp_diff_test review_complete_false_when_redacted -- --exact`（new）
- [ ] AC-3：`source .env.test && cargo test --test agent_mcp_diff_test review_complete_false_with_path_filter -- --exact`（new）
- [ ] AC-4：`source .env.test && cargo test --test agent_mcp_diff_test review_complete_false_when_patch_omitted -- --exact`（new）
- [ ] AC-5：`source .env.test && cargo test --test agent_mcp_diff_test review_complete_false_over_file_limit -- --exact`（new）
- [ ] AC-6：`source .env.test && cargo test --test agent_mcp_diff_test review_complete_false_for_binary -- --exact`（new）
- [ ] AC-7：`source .env.test && cargo test --test agent_mcp_diff_test review_complete_false_when_truncated -- --exact`（new）
- [ ] AC-8：`source .env.test && cargo test --test agent_mcp_diff_test review_complete_false_when_envelope_redacts_path -- --exact`（new）

**同卡文档门（ER-06/ER-06a，全局强制，不计入 G-03；判定命令退出码 0 为通过）:**

- [ ] `docs/commands/agent.md`：`rg -qF -- 'review_complete' docs/commands/agent.md && rg -qF -- 'libra diff --cached' docs/commands/agent.md`
- [ ] `docs/commands/zh-CN/agent.md`：`rg -qF -- 'review_complete' docs/commands/zh-CN/agent.md && rg -qF -- 'libra diff --cached' docs/commands/zh-CN/agent.md`
- [ ] `COMPATIBILITY.md`：`sed -n '252p' COMPATIBILITY.md | rg -qF -- 'review_complete'`；行数不变：`test "$(libra diff --numstat -- COMPATIBILITY.md | cut -f1-2)" = "$(printf '1\t1')"`（提交前执行）
- [ ] 网站源文件 `agent`：`rg -qF -- 'review_complete' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md && rg -qF -- 'libra diff --cached' ../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`；GC-MC-05 ①–⑤ 交付后线上页面：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/agent)" && printf '%s' "$PAGE" | rg -qF -- 'review_complete' && printf '%s' "$PAGE" | rg -qF -- 'libra diff --cached'`（GC-MC-05 ⑥）
- [ ] 语义复核：本卡 ER-05 Codex review 的输入包含上列每个文档文件的 diff，评审结论须逐文件确认其内容与本卡 `Docs and compatibility impact` 的内容说明一致（网站页另由 GC-MC-05 ④ 的 Claude Code PASS 覆盖）

**Full-suite trigger:** `T-5: 计划级 GC-MC-01 要求每张实现卡的 C 组全量包含全部 `agent_bridge_*` target 与工具 schema 一致性门（`agent_mcp_conformance_test`），本卡按 T-5 执行全量`

**Dependencies:** `MC-34`（消费 `EnvelopeOutcome` 与 `finalize`；写集与 MC-27 串行：`tools/repo.rs`、`agent_mcp_diff_test.rs`、agent 文档）；`DEP-MC-03`；`DEP-MC-02`（网站交付，GC-MC-05）

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/mcp_server/tools/repo.rs`、`tests/agent_mcp_diff_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`

**Docs and compatibility impact:** 受影响文件（逐文件交付，验收见「同卡文档门」）：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`。内容：`libra_diff` staged 结果增加 `review_complete` 及其为 `false` 的全部条件，写明此时须在本机 `libra diff --cached` 人工审核；`COMPATIBILITY.md` 第 252 行追加一句；开发文档：N/A。不受影响：`docs/development/commands/agent.md`、`docs/error-codes.md`——N/A，理由见上述内容说明（未新增错误码，ADR-MC-08）。

**Rollback mode:** `immutable-release`

**Migration and rollback:** `N/A`

**Security and privacy:** 部分、过滤或脱敏的 diff 不会被标为完整审核依据。

**Performance budget:** O(文件数) 标志计算。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=staged diff 结果是否构成完整审核依据; recovery=immutable-release，发新版本修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=MC-34,DEP-MC-03,DEP-MC-02; writeset=序列化于 MC-34; release=independent; split-from=MC-27; exception=N/A`

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | 表名集合守卫、表归属守卫、HTTP 平台判定、commit 内部 CAS | `source .env.test && cargo test --lib internal::ai::agent_bridge::ingress::tests`；`source .env.test && cargo test --lib internal::config_ownership`；`source .env.test && cargo test --lib internal::mutable_state_ownership`；`source .env.test && cargo test --lib internal::ai::mcp_server`；`source .env.test && cargo test --lib command::commit::tests::expected_parent` |
| 集成 | stdio 核心、请求 span（MC-22）、hook 拒绝策略（MC-23）、工具结果一致性、脱敏、资源上限、status、diff、写会话、checkpoint 读、commit 竞态与工具、workspace 读/lease、trace、HTTP、HTTP 准入、modern | `source .env.test && cargo test --test agent_mcp_stdio_test --test agent_mcp_span_test --test agent_mcp_conformance_test --test repo_hook_policy_test --test agent_mcp_envelope_test --test agent_mcp_limits_test --test agent_mcp_exec_test --test agent_mcp_status_bounded_test --test diff_bounded_alloc_test --test agent_mcp_status_test --test agent_mcp_diff_test --test agent_mcp_write_session_test --test agent_mcp_write_quota_test --test agent_mcp_checkpoint_read_test --test commit_fenced_race_test --test agent_mcp_commit_test --test agent_mcp_workspace_read_test --test agent_mcp_workspace_lease_test --test agent_mcp_trace_test --test agent_mcp_trace_checkpoint_test --test agent_mcp_http_test --test agent_mcp_http_admission_test --test agent_mcp_rate_test --test agent_mcp_modern_test` |
| 兼容 | bridge 原样（全部 `agent_bridge_*`）；help EXAMPLES；命令文档 Examples 段；CLI `--json` 字节稳定 | `source .env.test && cargo test --test agent_bridge_protocol_test --test agent_bridge_stdio_test --test compat_help_examples_banner --test compat_command_docs_examples_section`；`command_test` 的 `command::agent_workspace_test::workspace_{list,show}_json_byte_stable`、`command::agent_checkpoint_test::{session,checkpoint}_{list,show}_json_byte_stable` |
| 迁移 | 建表、Harness 表不变、fresh 与升级一致、中断、幂等重放、down、GC-13、旧二进制 | `source .env.test && cargo test --test agent_bridge_migration_test`；MC-03 的旧二进制命令门 |
| 安全 | 分表隔离与同名标识；actor 作用域；写工具默认隐藏；脱敏；loopback/令牌/Origin/Host；transcript 不外泄；lease 工作树复核 | `source .env.test && cargo test --test agent_bridge_source_isolation_test --test agent_mcp_checkpoint_read_test --test agent_mcp_diff_test --test agent_mcp_http_test --test agent_mcp_trace_test --test agent_mcp_workspace_lease_test` |
| 可靠性 | 重放（含 HEAD 前进后）、重启、跨工作树写锁、checkpoint 崩溃收敛（含链接补写）、commit 崩溃后 quarantine、commit 竞态 CAS | `source .env.test && cargo test --test agent_mcp_write_session_test --test agent_mcp_commit_test --test commit_fenced_race_test` |
| 性能 | 结果上限与截断、读 deadline、HTTP 准入 16/64/503、请求体 413、分页上限 | MC-19 AC-1/3/4、MC-05 AC-7/8、MC-12 AC-4/5、MC-20 AC-4/5、MC-13 AC-32/33（413）、MC-17 AC-1..3（最终树准入；MC-13 AC-34..36 的临时模式门只是 MC-13 版本的历史发布证据，已由 MC-17 移除）、MC-21 AC-1..8 |
| live/gated | 真实 WorkBuddy / Grok Bot；上一发布版二进制读新库 | MC-15（手工，不在默认套件）；MC-03 旧二进制命令门（需网络下载上一发布版 artifact） |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| MC-01 | 用户需求（stdio）；GAP-MC-01/04 | `src/command/agent/mcp.rs`、`src/internal/ai/mcp_server/` | agent EN/zh/网站、development、COMPATIBILITY | `agent_mcp_stdio_test::*` |
| MC-22 | 自 MC-01 拆出（可观测性） | `mcp_server/{stdio,protocol}.rs` | development | `agent_mcp_span_test::*` |
| MC-16 | GAP-MC-04（自 MC-01 拆出） | `mcp_server/output.rs` | agent EN/zh/网站、development | `agent_mcp_envelope_test::*` |
| MC-32 | R28（pack delta 头不是对象尺寸） | `src/command/mod.rs` | N/A（内部 helper） | `command::tests::object_size_*` |
| MC-35 | R29（读子进程扫描/diff 内存无界） | `mcp_server/exec.rs`、`mcp_server/tools/mod.rs` | `docs/error-codes.md`（`LBR-AGENT-028` 补充） | `agent_mcp_exec_test::worker_*`、`slots_return_after_memory_kills` |
| MC-36 | R28/R29/R30（工作树增长与替换竞态；符号链接语义） | `src/command/mod.rs` | N/A（内部 helper） | `command::tests::bounded_read_rejects_*`、`bounded_read_large_file_*`、`bounded_read_symlink_*` |
| MC-39 | R30/R32/R35（LFS 同一句柄与哈希上限） | `src/utils/lfs.rs`、`src/command/mod.rs` | N/A（内部 helper） | `command::tests::bounded_read_lfs_*` |
| MC-33 | R28/R30（status 扫描与全部动态数组无界） | `src/command/status.rs`、`status/scan.rs`、`status_untracked.rs`、`status/output.rs` | N/A（内部入口；MCP 文档由 MC-02） | `agent_mcp_status_bounded_test::*`、`command::status::tests::bounded_*`、`command::status_test::cli_status_unbounded_unchanged` |
| MC-37 | R29/R30（完整 diff 结果须在构造阶段有界） | `src/command/diff.rs`、`src/command/diff/compare.rs` | N/A（MCP 文档由 MC-05） | `command::diff::tests::bounded_*_budget_*`、`bounded_diff_output_size_with_long_paths` |
| MC-34 | R27/R28（信封处理结果须回报且 finalize 后重新计量） | `mcp_server/output.rs` | N/A（内部接口） | `agent_mcp_envelope_test::finalize_*` |
| MC-31 | R25/R27/R29（MCP diff 的内存、行数、全局预算与 rename 后处理须有界） | `src/command/diff.rs`、`src/command/diff/compare.rs`、`agent_bridge/vcs.rs` | N/A（服务端入口；MCP 文档由 MC-05 承接） | `command::diff::tests::bounded_*`、`command::diff_test::cli_rename_detection_unchanged`、`agent_mcp_exec_test::real_bounded_diff_times_out`、`agent_mcp_exec_test::bounded_diff_works_in_sha256_repo` |
| MC-29 | R24/R27/R28（超时的读工作须被强制终止；读子进程只读预检） | `mcp_server/exec.rs`、`mcp_server/tools/mod.rs`、`src/command/agent/mcp.rs`、`src/cli.rs` | agent EN/zh/网站、development | `agent_mcp_exec_test::blocking_*`、`timed_out_worker_is_killed`、`slots_return_after_workers_killed`、`read_worker_*` |
| MC-19 | GAP-MC-04（自 MC-16 拆出） | `mcp_server/output.rs` | agent EN/zh/网站、development | `agent_mcp_limits_test::*` |
| MC-21 | 规范 tools 安全要求（工具调用速率限制） | `mcp_server/rate.rs` | agent EN/zh/网站、development | `agent_mcp_rate_test::*` |
| MC-02 | 用户需求；GAP-MC-01 | `mcp_server/tools/repo.rs` | agent EN/zh/网站、COMPATIBILITY | `agent_mcp_status_test::*` |
| MC-03 | GAP-MC-02 | `sql/migrations/`、`src/internal/db/migration.rs`、`src/internal/{config_ownership,mutable_state_ownership}.rs` | migrations README、deepseek-harness tracing | `agent_bridge_migration_test::mcp_*`、两个表归属守卫、旧二进制命令门 |
| MC-04 | GAP-MC-02/03 | `src/internal/ai/agent_bridge/`、`src/command/agent/bridge.rs` | deepseek-harness tracing | `agent_bridge_source_isolation_test::*`、`internal::ai::agent_bridge::ingress::tests::harness_table_names_unchanged`、结构守卫命令 |
| MC-05 | GAP-MC-04 | `mcp_server/tools/repo.rs` | agent EN/zh/网站、COMPATIBILITY | `agent_mcp_diff_test::*` |
| MC-06 | 用户需求（checkpoint）；GAP-MC-05/06 | `mcp_server/{session.rs,tools/checkpoint.rs}`、`src/command/agent/mcp.rs` | agent EN/zh/网站、development、COMPATIBILITY | `agent_mcp_write_session_test::*` |
| MC-07 | GAP-MC-06 | `mcp_server/tools/{checkpoint,repo}.rs` | agent EN/zh/网站、COMPATIBILITY | `agent_mcp_checkpoint_read_test::*` |
| MC-18 | GAP-MC-07（原子 HEAD 前提） | `src/command/commit.rs`、`agent_bridge/vcs.rs` | N/A（内部字段） | `command::commit::tests::expected_parent_*`、`internal::ai::agent_bridge::vcs::tests::commit_create_fenced_maps_stale`、`command::commit_test::commit_help_hides_expected_parent`、`commit_fenced_race_test::*` |
| MC-23 | R14/R18（hook 派生进程可在写锁释放后继续运行，含脱离进程组者） | `src/internal/repo_hooks.rs` | N/A（内部作用域；用户可见拒绝由 MC-08 文档化） | `repo_hook_policy_test::*` |
| MC-24 | R16（同 OID 分支切换可让提交落到错误分支） | `src/command/commit.rs`、`agent_bridge/vcs.rs` | N/A（内部字段） | `commit_fenced_race_test::same_oid_*`、`command::commit::tests::expected_head_ref_*` |
| MC-43 | R17/R59（并发 `add` 改变暂存内容；自 MC-24 拆出） | `src/command/commit.rs` | N/A（内部字段） | `command::commit::tests::expected_tree_*` |
| MC-28 | R22/R23（未出生分支须有显式前提且在发布该前提的卡内测试） | `src/command/commit.rs`、`agent_bridge/vcs.rs` | N/A（内部类型） | `commit_fenced_race_test::unborn_*`、`command::commit::tests::expected_unborn_*` |
| MC-25 | R18/R20/R21（快照身份须与提交前提一致含分支、只读调用不得写对象、未出生 HEAD） | `src/internal/tree_plumbing.rs`、`mcp_server/tools/repo.rs` | agent EN/zh/网站、`COMPATIBILITY.md` | `agent_mcp_status_test::status_*` |
| MC-26 | R21（`staged_changes` 须与 `index_tree` 同源：intent-to-add、仅 mode 变化） | `mcp_server/tools/repo.rs` | agent EN/zh/网站、`COMPATIBILITY.md` | `agent_mcp_status_test::status_*` |
| MC-30 | R24/R25/R28（部分、过滤或脱敏的 staged diff 不得被当作完整审核） | `mcp_server/tools/repo.rs`（消费 MC-34 的 `EnvelopeOutcome`/`finalize`） | agent EN/zh/网站、`COMPATIBILITY.md` | `agent_mcp_diff_test::review_complete_*` |
| MC-08 | 用户需求（commit）；GAP-MC-05/07 | `mcp_server/tools/commit.rs` | agent EN/zh/网站、COMPATIBILITY | `agent_mcp_commit_test::*` |
| MC-27 | R22/R28（审核的 staged patch 须绑定将提交的树，且经有界入口） | `src/command/diff/compare.rs`、`src/command/diff.rs`、`mcp_server/tools/repo.rs` | agent EN/zh/网站、`COMPATIBILITY.md` | `agent_mcp_diff_test::diff_staged_*` |
| MC-10 | 用户需求（workspace）；GAP-MC-08 | `src/command/agent/workspace.rs`、`mcp_server/tools/workspace.rs` | agent EN/zh/网站、COMPATIBILITY | `agent_mcp_workspace_read_test::*`、`command::agent_workspace_test::workspace_*_json_byte_stable` |
| MC-11 | GAP-MC-08；R34/R35（lease 重试与崩溃须可判定） | `mcp_server/tools/workspace.rs`（使用既有 `WorkspaceStore::*_with_conn`） | agent EN/zh/网站、`COMPATIBILITY.md` | `agent_mcp_workspace_lease_test::claim_*` |
| MC-38 | R35/R37/R38（renew 事务绑定、重放、TTL 契约与 actor 边界） | `mcp_server/tools/workspace.rs` | agent EN/zh/网站、`COMPATIBILITY.md` | `agent_mcp_workspace_lease_test::renew_*` |
| MC-41 | R38（release 事务绑定、重放与 actor 边界） | `mcp_server/tools/workspace.rs` | agent EN/zh/网站、`COMPATIBILITY.md` | `agent_mcp_workspace_lease_test::release_*` |
| MC-42 | R42..R48（已于 R48 并入 MC-06：写记录容量的计数与上限须与第一个写工具同版本生效） | 见 MC-06 | 见 MC-06 | `agent_mcp_write_quota_test::*`（MC-06） |
| MC-40 | R36（主工作树的 worktree id 为空，linked claim 无法覆盖主工作树） | `mcp_server/tools/workspace.rs` | N/A（内部 helper；用户可见行为由 MC-11 文档化） | `internal::ai::mcp_server::tools::workspace::tests::*` |
| MC-12 | 用户需求（trace）；GAP-MC-09 | `src/command/agent/session.rs`、`mcp_server/tools/trace.rs` | agent EN/zh/网站、COMPATIBILITY | `agent_mcp_trace_test::*`、`command::agent_checkpoint_test::session_*_json_byte_stable` |
| MC-20 | GAP-MC-09（自 MC-12 拆出） | `src/command/agent/checkpoint.rs`、`mcp_server/tools/trace.rs` | agent EN/zh/网站、COMPATIBILITY | `agent_mcp_trace_checkpoint_test::*`、`command::agent_checkpoint_test::checkpoint_*_json_byte_stable` |
| MC-13 | 用户需求（`--transport http`、Grok）；GAP-MC-10 | `mcp_server/http.rs`、`src/command/agent/mcp.rs` | agent EN/zh/网站、development、COMPATIBILITY | `agent_mcp_http_test::*`、`internal::ai::mcp_server::http::tests::*` |
| MC-17 | GAP-MC-10（自 MC-13 拆出） | `mcp_server/http.rs` | agent EN/zh/网站、development | `agent_mcp_http_admission_test::*` |
| MC-14 | 规范 `2026-07-28`；GAP-MC-11 | `mcp_server/{protocol,modern,stdio,http}.rs` | agent EN/zh/网站、development、COMPATIBILITY | `agent_mcp_modern_test::*` |
| MC-15 | 用户需求（WorkBuddy/Grok）；GAP-MC-12 | 本计划证据段 | N/A | 手工证据 + 两条命令 |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 本计划 Codex review PASS 并推送 | 计划提交 SHA | N/A（文档） |
| M1 | MC-01、MC-22、MC-16、MC-19、MC-29、MC-35、MC-32、MC-36、MC-39、MC-31、MC-37、MC-33、MC-21、MC-02..MC-05 `done/complete`：stdio 只读可用（含读子进程的时间与内存上限、有界 diff 与 status、限流与 span），存储分表 | 十七个 patch 版本、D-MC-1..4 | immutable-release / forward-only：前滚修复 |
| M2 | MC-06、MC-07、MC-18、MC-23、MC-24、MC-43、MC-28、MC-25、MC-26、MC-27、MC-34、MC-30、MC-08、MC-10、MC-40、MC-11、MC-38、MC-41、MC-12、MC-20 `done/complete`：写会话、checkpoint、commit（含原子前提、一致快照与 hook 拒绝）、workspace、trace | 二十个 patch 版本 | immutable-release：前滚修复 |
| M3 | MC-13、MC-17、MC-14、MC-15 `done/complete` + CLOSE-MC | 三个 patch 版本、联调证据、收口 nextest 摘要 | immutable-release：前滚修复 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| checkpoint 行写入后、关联链接写入前崩溃 | operation `pending`、checkpoint 行存在、链接缺失 | 重启收敛：幂等补写链接后记 `applied`（MC-06 AC-32/33） | 链接缺失却记为成功、重复 checkpoint |
| checkpoint 行写入前崩溃 | operation `pending` | 重启收敛为 `failed`，以意图中的原请求重放再创建（MC-06 AC-34/35） | 永久阻塞、参数摘要漂移 |
| commit 效果后、回执前崩溃 | operation `pending`，仓库已变化 | 重启置为 `quarantined`；重放返回 `LBR-AGENT-035`（MC-08 AC-15、MC-06 AC-36/37）；客户端核对后以新 `operation_id` 与当前 HEAD 重试 | 误标 failed 后重复执行、静默成功 |
| commit 的 fence 之后、ref 写入之前 HEAD 被 CLI/Harness 移动 | 已写对象，无 ref 变化 | MC-18 事务内 CAS 拒绝 → `LBR-AGENT-038`（MC-18 AC-6..8）；未引用对象由既有 gc 回收 | 覆盖他人提交 |
| MC-03 migration 中途失败 | 事务回滚 | 修复后重跑（MC-03 AC-6） | Harness 表被改动 |
| 写入停住超过 120 s | operation `pending`，进程退出；写入期限退出由 MC-06 AC-26/27/30/31 验证；MCP 提交不运行 hook（MC-23；MC-08 AC-16..21 验证拒绝），因此不存在 hook 派生进程 | 下次启动置为 `quarantined`（MC-06 AC-26/27/30/31） | 无界持锁 |
| HTTP 进程被 SIGKILL | 同上；actor 锁与仓库写锁随进程释放 | 重启后按上述规则收敛 | 令牌写入日志、绑定非 loopback |
| stdio 客户端断开 | 无 | EOF 后退出（MC-01 AC-19） | 子进程残留（MC-15 AC-5） |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| MCP 不提供 restore | 低：用户需用 CLI 恢复 | ADR-MC-09、DEFER-MC-07 | — |
| HTTP 在 MC-17 之前只能串行 | 低：吞吐受限 | MC-13 临时模式有界（并发 1、忙即 503）；MC-17 提升 | MC-13、MC-17 |
| 客户端无法设置 `cwd`，在错误目录启动 | 中：启动即退出 | 文档示例写 `cwd`；MC-15 实测；DEFER-MC-03 | MC-01、MC-15 |
| `--allow-write` 进程级审批过宽 | 高：受信客户端可直接提交 | 默认只读；annotations；文档要求只对受信客户端开启 | MC-06 |
| 客户端重试时不复用 `operation_id` | 中：可能重复 checkpoint | `operation_id` 必填并在文档说明重试规则；commit 另有 HEAD CAS | MC-06、MC-08 |
| 令牌泄露（远程部署） | 高 | 0600、不进日志、不接受 query、文档要求轮换与 TLS 终端 | MC-13 |
| 脱敏漏网（redactor 未识别的 secret 形态） | 中 | 复用捕获管线同一 redactor；新增形态随 redactor 演进 | MC-01、MC-05 |
| 规范继续演进 | 中 | 版本集合单一事实源；新版本新卡 | MC-14 |
| 升级时旧 `agent bridge` 进程仍在运行 | 中 | 文档要求先停止；旧二进制无法再打开新库 | MC-03 |
| 与其它在途计划改同一文件 | 中 | DEP-MC-01、DEP-MC-03 串行 | 全部 |
| 联调环境不可得 | 中：MC-15 阻塞 | DEP-MC-04/05 登记；不以其它客户端替代 | MC-15 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 读工具 | O(请求) | stdio 串行 | deadline 30 s，完整响应 ≤ 256 KiB | MC-19 AC-1/3/4 |
| diff | O(改动) | — | patch ≤ 256 KiB（截断） | MC-05 AC-7/8 |
| HTTP 请求 | O(请求) | MC-13 临时：并发 1、忙即 503；MC-17：并发读 16、等待 64、超出 503；MC-21（核心，HTTP 上为 429）：10 次/秒、突发 20；写串行 | 体 ≤ 256 KiB（413） | MC-13 AC-32/33、AC-37..39（429 映射）、MC-17 AC-1..8、MC-21 AC-1..8（MC-13 AC-34..36 为历史证据） |
| 列表类工具 | O(limit) | keyset 分页 | 上限沿用 CLI | MC-10 AC-4、MC-12 AC-4/5、MC-20 AC-4/5 |
| 写入期限 | 每次写入 ≤ 120 s | — | 超时进程退出 | MC-06 AC-26/27/30/31 |
| 启动收敛 | O(本 actor pending 行) | 每次写进程启动一次 | — | MC-06 AC-31..37 |
| MCP 写记录（意图、operation、checkpoint、链接、lease、MCP 创建的 `workspace_record`） | 每个写 `operation_id` 一组，`operation_id` ≤ 128 字节，`request_json`/`result_json` 各 ≤ 256 KiB | 全仓库 ≤ 10000 组（不分 actor），不自动删除；用量计数单行 O(1) | 记录载荷合计 ≤ 512 MiB（不含 SQLite 页、索引、WAL）；超限 `LBR-AGENT-035`（`write_quota`），清理见 DEFER-MC-10 | MC-06（`operation_id` 长度门与容量门） |
| 分表 migration | O(1)（只建空表） | 一次性 | — | MC-03 AC-1 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md`：新增公开工具或参数的卡（MC-01、MC-02、MC-05..MC-08、MC-10..MC-14、MC-20、MC-25..MC-27、MC-30、MC-38、MC-41）在第 252 行 `agent` 行内追加，不增删行；MC-03、MC-04、MC-15..MC-19、MC-21..MC-24、MC-28、MC-29、MC-31..MC-37、MC-39、MC-40、MC-43 为 N/A。
- [ ] `docs/commands/agent.md` 与 `docs/commands/zh-CN/agent.md` 已同步（MC-01、MC-16、MC-19、MC-02、MC-05..MC-08、MC-10..MC-14、MC-17、MC-20、MC-21、MC-25..MC-27、MC-29、MC-30、MC-38、MC-41）。
- [ ] `docs/development/commands/agent.md` 已同步（MC-01、MC-22、MC-16、MC-19、MC-29、MC-21、MC-06、MC-13、MC-17、MC-14）。
- [ ] agent 文档 EN/zh 与网站 agent 页已写明 MCP 提交在存在提交 hook 时拒绝（`hooks_present`，MC-08）。
- [ ] 网站页 `agent.en.md` 已按 GC-MC-05 交付（同 EN 文档卡）。
- [ ] `docs/development/tracing/deepseek-harness.md` 与 `sql/migrations/README.md` 已同步（MC-03、MC-04）。
- [ ] `docs/error-codes.md`：不新增 `LBR-*`；MCP 复用注记与 `028`/`029`/`036` 行的 MCP 含义已写入（MC-01、MC-19、MC-21 同卡文档门：`rg -qF -- 'libra agent mcp' docs/error-codes.md && rg -qF -- 'MCP tool result' docs/error-codes.md && rg -qF -- 'MCP tool call' docs/error-codes.md && rg -qF -- 'read worker memory' docs/error-codes.md && rg -qF -- 'write_quota' docs/error-codes.md`）。
- [ ] `tests/INDEX.md` 与 `Cargo.toml` `[[test]]` 已同步（每张卡的新 target）。
- [ ] `plan-long.md` 日期计划索引已登记本计划，完成后更新状态。

## 实施证据汇总

（执行阶段按卡追加：A/B/C/D 门结果、run ID、版本、SHA-256、全量 nextest 计数、MC-15 联调证据。成稿时为空。）

## Codex review log

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 | 全文（7 卡草稿） | FAIL | P1×18：source 标识隔离、重启重放、diff 脱敏、Windows 令牌、迁移兼容与 GC-13 清单、workspace 路径/TaskCopy、checkpoint 无目标、HTTP 纪元路由、`2025-03-26` batch、legacy 版本头归属、错误分层矛盾、G-03 计数、写集相交、漏列测试文件、不可执行的测试名、网站交付门、性能门、客户端联调——在 v2 修订处理（修订历史 2026-10-19 10:38:26 UTC） | P2×4 已修 | 输入 SHA-256 `fc65e4be628ddad325f623eddf04e08a4245633eb8b504fbc9225eeb5f0ef1b2`；read-only |
| R2 | 全文（12 卡 v2） | FAIL | P1×10：同名标识、pending 误判、缺省 `operation_id`、renew/release 工作树、旧二进制门、AC 计数、EX 多轴、`S` 标注、lib 测试名、checkpoint diff 无来源——在 v3 修订处理（修订历史 2026-10-19 11:01:16 UTC） | P2×2 已修 | 输入 SHA-256 `3dcca908e702eb54e3eb6a43b3e31b4bb545773bd3a04e0f51f8f24263d29e53`；read-only |
| R3 | 全文（15 卡 v3） | FAIL | P1×10：跨 actor 读取/恢复 checkpoint → 三处查找复核 `mcp~<actor>` 并加门；写锁不跨 linked worktree → 锁放共享存储并加跨工作树门；checkpoint 收敛未补链接 → 收敛器先幂等补写链接并加崩溃门；HEAD 前进后重放摘要漂移 → 意图表 `agent_mcp_intent` 复用首次请求并加门；commit/restore 崩溃门晚于发布 → 并入 MC-08/MC-09，删除原崩溃卡；漏表归属注册表且超 12 文件 → 拆成 MC-03 migration 与 MC-04 source 重构并登记两个注册表；EX 用于 migration 卡 → 迁移卡不用 EX，A 组强制门与 GC-13 门按 ER-04 列为不计数强制门，EX-MC-02 改挂 implementation 卡 MC-04；AC 多表断言计数 → 改为集合/列表相等单谓词，结构守卫改为可复制零命中命令并计数；旧二进制门 → 下载上一发布版 artifact 实开新库；ER-06a 逐文件门 → 每卡生成逐文件判定命令——在 v4 修订处理（修订历史 2026-10-19 11:29:32 UTC） | P2×2（migration 版本示例、WorkBuddy 进程身份）已修 | 输入 SHA-256 `f86d103c92b16d65e8391f9aa2c992a6882ccebd19e972be52751e94c7025bba`；read-only |
| R4 | 全文（15 卡 v4） | FAIL | P1×9：跨 actor 仓库写竞态 → 仓库写锁 + 跨 actor 门；MC-15 覆盖来源早于其改动 → CLOSE-MC；网站卡缺 DEP-MC-02 → 补齐；旧二进制命令非平台通用且无前置断言 → 脚本化；结构守卫可在 `libra diff` 失败时误判 → 先判退出码；AC 低计 → 拆分重计；landing 计数错 → 自动计算；文档门只查关键字 → 逐文件多 token + 语义复核；缺通知静默门 → 新增——在 v5 修订处理（修订历史 2026-10-19 11:38:22 UTC） | P2×1（预检二进制）→ GC-MC-08 | 输入 SHA-256 `7e3190558bde375a4c923b4f2c9d005d27aabafb44f8263efd8471629f6b37ea`；read-only |
| R5 | 全文（15 卡 v5） | FAIL | P1×5：MCP 写锁不覆盖 CLI/Harness → commit 改用事务内 CAS（新卡 MC-18），restore 窗口登记 DEFER-MC-07 并文档警告，ADR-MC-06 明确锁的范围；MC-01/MC-13 多轴 → 拆出 MC-16（结果信封）与 MC-17（HTTP 准入）；与 `CLAUDE.md` 全量口径冲突 → GC-MC-09 两者同时执行；MC-15 进程门可空过 → 两阶段脚本；意图键跨 actor 未测 → 明确键为 actor 限定 id 并加门——在 v6 修订处理（修订历史 2026-10-19 11:51:37 UTC） | P2×2（版本精确比较、基线版本说明）已修 | 输入 SHA-256 `ef8846de9d2212d1786b81f1774048b89a46277b3aaeb2feb170b40e9ce4f540`；read-only |
| R6 | 全文（18 卡 v6） | FAIL | P1×8：restore 可覆盖并发写入 → 撤销 restore 工具（DEFER-MC-07）；HTTP 先于容量上限发布 → MC-13 临时串行模式；MC-16 多轴 → 拆出 MC-19；EX-MC-06 跨两个查询族 → 拆出 MC-20；commit 原子性缺竞态门 → MC-18 确定性竞态门；`label` 无可观察效果 → 删除；span 无归属 → MC-01；Grok 门未证明真实调用 → 随机探针分支——在 v7 修订处理（修订历史 2026-10-19 12:04:33 UTC） | P2×1（MC-17 现行锚点）已修 | 输入 SHA-256 `6f1a51b304213ea0589e647cd2eb513ae96c68d95dab42b092d77b1e94537850`；read-only |
| R7 | 全文（19 卡 v7） | FAIL | P1×8：MCP 提交未走 fenced 写入 → 分发器按 source 路由 + 端到端竞态门；MC-13 等待队列无界 → 忙即 503；256 KiB 未覆盖完整响应 → 以完整响应计量 + 服务端 blob 测试；仓库级 session 的工作树作用域未定义 → `worktree_id` NULL + 跨工作树门；EX-MC-06 任务列含两卡 → 拆成 EX-MC-06/07；MC-15 修改用户仓库 → 一次性探针仓库；Grok 关联不足 → 探针名不入提示词 + span `tool` 字段 + 单一服务；MC-15 无现行锚点 → 补——在 v8 修订处理（修订历史 2026-10-19 12:14:54 UTC） | — | 输入 SHA-256 `42a5ad0df2412c698c1ed218b1f95b86f034ee4e8016e2c801e28667ee86cd2e`；read-only |
| R8 | 全文（19 卡 v8） | FAIL | P1×6：stdio 探测后回退被拒 → 允许 discover 后 initialize 并加门；缺失 modern 头未测 → 四道门；WorkBuddy 门无返回截取 → 命令比对；探针清理不可执行 → 固定目录 + 清理命令；AC 复合谓词低计 → 拆分；MC-14 无现行锚点 → 补——在 v9 修订处理（修订历史 2026-10-19 12:29:01 UTC） | — | 输入 SHA-256 `c388c21f44c66d69f666fcc8ddd665495d730449bea1d33c7e26a3265a994612`；read-only |
| R9 | 全文（19 卡 v9） | FAIL | P1×5：EX 判据规范未逐门列名 → 逐一列名；span/成功门复合 → 拆分；WorkBuddy 关闭门变量跨 shell 丢失 → 显式传入；modern 缺版本头未定义 → 400 + `-32020` 两门；`cargo test --all` 作为完成门违反 ER-14 → 依 `CLAUDE.md:192-194` 改为仅诊断——在 v10 修订处理（修订历史 2026-10-19 12:38:48 UTC） | P2×1（锚点）已修 | 输入 SHA-256 `3b0a1f4b29b17127779493d1fca87bf811b64a6e585d76f02f52ed9bfcdd374b`；read-only |
| R10 | 全文（19 卡 v10） | FAIL | P1×4：span 记录客户端可控值 → 净化字段并加门；不一致头缺 400 门 → 补；`resultType` 只测一处 → 覆盖 discover 与工具成功/错误；Harness 隔离只测两个读面 → 补四个——在 v11 修订处理（修订历史 2026-10-19 12:50:49 UTC） | P2×2（Base64 `Mcp-Name`、自审摘要）已修 | 输入 SHA-256 `e9567fd88e08dbd6a6c57dc5b79215c443f8aac06e2ed2c008b914aec40b2906`；read-only |
| R11 | 全文（19 卡 v11） | FAIL | P1×3：结果无 `outputSchema` 一致性门 → 注册契约 + 注册表级 `jsonschema` 测试（GC-MC-10）；HTTP 无调用速率限制 → 新卡 MC-21；写入可无限持锁 → 120 s 硬期限、进程退出、quarantine——在 v12 修订处理（修订历史 2026-10-19 13:16:51 UTC）；首次运行因模型容量错误中断，按 ER-10 重试一次后得到本结论 | P2×2（MCP event 语义、Base64 断言）已修 | 输入 SHA-256 `63245c8f26caf7d1916af08fe8d86baf02f81ec93a7b8a60bd384fc47720c73e`；read-only |
| R12 | 全文（20 卡 v12） | FAIL | P1×5：一致性测试不分分支 → 按分支断言；HTTP 先于限流发布 → 限流改为核心并前移；写入期限未覆盖真实 commit → MC-08 hook 阻塞三门（EX-MC-08，用户批准）；迁移的用户文档缺失 → MC-03 加 EN/zh/网站升级说明；限流未测持续负载 → 加持续负载门——在 v13 修订处理（修订历史 2026-10-19 14:02:49 UTC） | P2×1（`initialize` 豁免门）已修 | 输入 SHA-256 `179a2b08e931ed3142f31d12c033da72d8eab5333c8fdeca48a89146b29b5936`；read-only |
| R13 | 全文（20 卡 v13） | FAIL | P1×4：后续卡使 MC-01 门失效 → 改为版本稳定的谓词；MC-17 后 MC-13 临时用例会失败 → MC-17 同卡移除并加守卫；一致性门缺有状态工具的前置条件 → 每工具一致性夹具；门命令用了不兼容的已安装 `libra` → GC-MC-08 统一 `PATH`——在 v14 修订处理（修订历史 2026-10-19 14:31:58 UTC）；前两次运行因模型容量错误中断，按 ER-10 退避重试 | P2×2（`initialize` 豁免说明、`axum` 锚点）已修 | 输入 SHA-256 `a0ee5b798833f19c880d64ac4eea31cde5fe48b7852414c5359ab83acccac0cd`；read-only |
| R14 | 全文（20 卡 v14） | FAIL | P1×3：MC-01 混入 tracing 轴 → 拆出 MC-22；写入期限后 hook 可继续运行 → 新卡 MC-23（期限覆盖 + 进程组 `SIGKILL`）并在 MC-08 加清理门；HTTP 令牌无强度要求 → 256 bit 下限与三门——在 v15 修订处理（修订历史 2026-10-19 14:44:03 UTC） | P2×2（探针清理核对标记、测试矩阵改指 MC-17）已修 | 输入 SHA-256 `47d0ec0654a1711fe177843799a2b4fe10be8777f73e1ee4ac310ce35fab0c64`；read-only |
| R15 | 全文（22 卡 v15） | FAIL | P1×3：hook 可在锁释放后存活 → 绝对期限 + 保留回收时间 + 晚启动门；令牌只校验长度 → `--generate-token` 与周期/全零拒绝；repository-hooks 网站页无交付门 → GC-MC-05 覆盖全部所列页面——在 v16 修订处理（修订历史 2026-10-19 14:54:53 UTC） | P2×2（准入与限流顺序、tracing 先例说明）已修 | 输入 SHA-256 `dbae1142db4fda4c20cc75227cfe6e27620f6feb4c4a012069eb6652738cdd63`；read-only |
| R16 | 全文（22 卡 v16） | FAIL | P1×6：Windows 上 hook 可在锁释放后存活 → `--allow-write` 限 Unix（DEFER-MC-08）；全长弱令牌可通过 → 只接受 `--generate-token` 的 v1 格式；同 OID 分支切换绕过前提 → 新卡 MC-24；复合谓词低计 → 拆分（MC-17 保持 8）；MC-04 守卫拒绝必要 import 且缺 pipefail → 全限定路径 + awk 守卫；`--generate-token` 与传输必选冲突 → 独立互斥模式——在 v17 修订处理（修订历史 2026-10-19 15:09:30 UTC） | — | 输入 SHA-256 `6d76f7edec7112005c08c8b7de7b34ae9d914fd5bb608928e63b99d4faf636c5`；read-only |
| R17 | 全文（23 卡 v17） | FAIL | P1×5：手工令牌经公开校验段仍可弱 → 服务自生成 `--token-out`；并发 `add` 可改变暂存内容 → `index_tree`/`expected_tree` 前提；hook 派生进程可脱离或在正常结束后存活 → 正常结束也杀组、保证收窄并登记 DEFER-MC-09；复用 028/029 而错误码目录未更新 → 三卡同卡改 `docs/error-codes.md`；MC-23 AC-1、MC-21 AC-5 复合 → 拆分/聚合——在 v18 修订处理（修订历史 2026-10-19 15:22:57 UTC） | — | 输入 SHA-256 `61325d0f2ab1817a32405c60a8911f4b3455ed4d27b80737ddec955f1f601915`；read-only |
| R18 | 全文（23 卡 v18） | FAIL | P1×5：status 与 `index_tree` 非同一快照 → 由不可变树得出 `head`/`staged_changes` 并加并发 `add` 门；脱离进程组的 hook 派生进程仍可越过写锁 → MCP 提交不运行 hook（存在即拒绝）；成功后杀组会改变 CLI/Harness 行为 → MC-23 改为仅作用域内拒绝 + 回归门；D-MC-1 未检查 merge SHA 上的 `base.yml` → 补齐（含 CLOSE-MC）；完成判据仍称 `docs/error-codes.md` N/A → 改为要求注记；P2：ADR-MC-09 缺 `expected_tree` → 补齐——在 v19 修订处理（修订历史 2026-10-19 15:35:12 UTC） | — | 输入 SHA-256 `db9238268898c90ae1292a136ebf429fc64fdd4b3d45d75a3ef0f9b7dcdd3d13`；read-only |
| R19 | 全文（23 卡 v19） | FAIL | P1×2：`--token-out` 拒绝已存在路径导致同命令行无法重启 → 合格文件原子替换 + 重启门 + 交接流程；MC-23 回归门未覆盖 advisory `post-commit` → 增加 CLI/Harness 两门；P2：MC-13 临时模式门范围错误 → 按用例名计算——在 v20 修订处理（修订历史 2026-10-19 15:47:52 UTC） | — | 输入 SHA-256 `042cb1cb0303a1b831b869d80b5e38cd6ba59ddae2cfd8814b57b9a70b338aed`；read-only |
| R20 | 全文（23 卡 v20） | FAIL | P1×2：只读 `libra_status` 会写树对象 → 内存计算树 OID 并加对象库不变门；未出生 HEAD 无定义 → `head = null`、空树基准并加门（两项拆为 MC-25 以守 G-03）；P2：M2 版本数 → 十一——在 v21 修订处理（修订历史 2026-10-19 15:57:57 UTC） | — | 输入 SHA-256 `68e9500908896a3430fd8fade3f765f7ef7d08da60baa697028cf493e7aac4e5`；read-only |
| R21 | 全文（24 卡 v21） | FAIL | P1×2：快照缺分支身份，status 与 commit 之间同 OID 切分支可提交到未审核分支 → `head_ref` + 必填 `expected_head_ref`（入口与事务内比较）+ 切分支门；`staged_changes` 未与写入器同一过滤（intent-to-add） → 同源条目、比较 mode 并加三门（拆出 MC-26）；P2：收口文档清单缺 MC-25 → 补 MC-25/MC-26——在 v22 修订处理（修订历史 2026-10-19 16:06:20 UTC） | — | 输入 SHA-256 `03348e9844f0f3f0a78a44fa7664800febfb677d9a20a13f1cb9fecdc4e4b959`；read-only |
| R22 | 全文（25 卡 v22） | FAIL | P1×5：未出生 HEAD 无提交前提 → `ExpectedParent::Unborn` + 三门；审核的 staged patch 未绑定提交树 → 新卡 MC-27；CLOSE-MC 可能推送陈旧本地 `main` → 干净切换、快进、推送后核对；MC-26 漏 GC-MC-01 全量 → T-5；登记缺失 → 实际登记并改正表述——在 v23 修订处理（修订历史 2026-10-19 16:16:12 UTC） | — | 输入 SHA-256 `286160d4246d6cf144da43abe7fa8e3d4257bc91de6ef397a8aa8931138b91ee`；read-only |
| R23 | 全文（26 卡 v23） | FAIL | P1×4：`libra_commit` 先于 diff 绑定发布 → MC-27 移到 MC-08 前、提交门移入 MC-08；MC-27 写集无法以既有快照生成 patch → 抽出 `diff_index_snapshot` 并加并发重新暂存两门；CLOSE-MC 第①步与 MC-15 未提交证据冲突 → 先同步再写证据；MC-24 发布 `Unborn` 却无本卡测试 → 拆出 MC-28；P2：自审卡数 → 27——在 v24 修订处理（修订历史 2026-10-19 16:27:36 UTC） | — | 输入 SHA-256 `b63b597d0a9f8c482dd58843c0dd651e7cea79fc815e502a3a5a63b31255f58e`；read-only |
| R24 | 全文（27 卡 v24） | FAIL | P1×3：MC-27 写集未含重新载入 index/HEAD 的 diff 引擎 → 加入 `compare.rs` 并传入已捕获快照；读 deadline 只是协作计时、阻塞读工作无界 → 新卡 MC-29（有界执行器、取消检查点）；脱敏后的 staged patch 可被当作完整审核 → `review_complete` 两门——在 v25 修订处理（修订历史 2026-10-19 16:37:38 UTC） | — | 输入 SHA-256 `65dc7d9adc921dfdbb1da808c8e96dc5e8d3718749be82555639dfb82311d403`；read-only |
| R25 | 全文（28 卡 v25） | FAIL | P1×4：过滤/省略的 staged diff 可标为完整 → MC-30 在最终结果后计算并加六门；读 deadline 无法取消真实 diff 内部计算 → MC-31 检查点 + 单文件硬上限 + 真实 diff 端到端门；`status` 与快照可不同源 → `status_consistent` 两门；`server/discover` 缺 `ttlMs`/`cacheScope` → 三门；P2：令牌长度措辞——在 v26 修订处理（修订历史 2026-10-19 16:52:40 UTC） | — | 输入 SHA-256 `6a358391a41b859b968c773205064c81e1c27a1d06d61fdf1b2de16fd60f1f4d`；read-only |
| R26 | 全文（30 卡 v26） | FAIL | P1×3：单文件行数上限仍不足以界定整组 `Diff::diff` → 逐文件引擎 + 1 MiB 字节上限 + 对抗性单文件门；MC-04 改动 bridge 内部与「原样」冲突 → 使用者裁决「对外契约不变」并写入 ADR-MC-02；`review_complete` 缺信封截断门 → 补门；P2：`status_consistent` 名不副实 → 改名并加工作树门——在 v27 修订处理（修订历史 2026-10-20 03:30:38 UTC） | — | 输入 SHA-256 `8b0d62d902cb70f1d5d26279114ef98e46d4f334454d72dc6c13aebf26fdb95a`；read-only |
| R27 | 全文（30 卡 v27） | FAIL | P1×5：字节上限在加载之后 → 分配前尺寸预检；rename 后处理无预算 → MCP 有界入口关闭 rename；`spawn_blocking` 丢失 hash kind、非 diff 读可占满槽位 → 改为可强制终止的读子进程（正常 CLI 预检）；`review_complete` 无信封信号 → `EnvelopeOutcome` + `finalize`；P2：追溯表陈旧——在 v28 修订处理（修订历史 2026-10-20 03:42:29 UTC） | — | 输入 SHA-256 `558710955cddf02f05dcc7ba37161e29ce225643e5e3181cf50d49adbc3b37ac`；read-only |
| R28 | 全文（30 卡 v28） | FAIL | P1×6：staged 快照绕过有界入口 → 只经有界入口并加两门；pack delta 头不是对象尺寸 → MC-32 delta 感知尺寸；读子进程走常规预检会迁移/修复/升级检查 → 只读预检四门；工作树 stat 后读取存在增长竞态 → 单句柄有界读取；status 无峰值内存上限 → MC-33 有界收集；finalize 可越过上限 → MC-34 重新计量；P2×3：追溯行、恢复引用、锚点——在 v29 修订处理（修订历史 2026-10-20 03:54:36 UTC） | — | 输入 SHA-256 `ac338b470f3ba49bebbe846cbe0fa3d4f3bafc2d11c2c7abe7be1dca231dc04d`；read-only |
| R29 | 全文（33 卡 v29） | FAIL | P1×3：有界工作树读取会跟随符号链接且丢失 LFS 语义 → MC-36 保持 blob 语义；status 上限作用于无界扫描之后 → MC-35 读子进程内存硬上限；逐文件上限不约束完整 diff → 全局收集预算 + MC-35；P2×2：MC-31 AC-1 不自足、追溯与写集不一致——在 v30 修订处理（修订历史 2026-10-20 04:05:34 UTC） | — | 输入 SHA-256 `5091d32f36bc268babea128aaae6b83f41baf0981548b93f4f37afe2df649deb`；read-only |
| R30 | 全文（35 卡 v30） | FAIL | P1×5：LFS 按路径哈希可被替换为外部链接 → 同一句柄；内存上限不覆盖进程树、采样可超出 → MC-35 改为纵深防御，主上限改为算法预算；status 未覆盖全部动态数组 → 16 个数组逐一列名并加真实 fixture 门；`omitted_count` 与 CLI 等价门冲突 → 只在省略时出现；MC-31 超 G-03 → 拆出 MC-37；P2：diff 元数据无界 → 文件条目预算——在 v31 修订处理（修订历史 2026-10-20 04:15:21 UTC） | — | 输入 SHA-256 `1efe78af6caddb0bd54185ba9c718a9f9deafd6843d427547862f922f5240f15`；read-only |
| R31 | 全文（36 卡 v31） | FAIL | P1×4：两客户端缺省同一 actor 会互相阻塞 → 写权限必须显式 actor；status 辅助进程不在上限与终止范围内 → 读子进程内纯进程内 I/O；`DiffOutput` 条目的 `hunks`/`raw_diff` 与转义未计入 → 条目整体预算；第二个 HTTP 实例会替换存活令牌 → 先绑定再发布；P2：MC-21 粒度记录——在 v32 修订处理（修订历史 2026-10-20 04:25:47 UTC） | — | 输入 SHA-256 `8349a4c438faf3988e5dc2dece2537456de3d983a6214aec37a76e9bdf6630e9`；read-only |
| R32 | 全文（36 卡 v32） | FAIL | P1×4：不同端口的第二实例仍可替换令牌 → 令牌路径独占锁；`raw_diff` 未计入预算 → 保留字节计数；LFS 哈希读取无上限 → 原始字节上限；预检后出现的 `post-commit` 被静默跳过 → `post_commit_skipped` 明示；P2×2：macOS 峰值门过强、`lfs.rs` 锚点——在 v33 修订处理（修订历史 2026-10-20 04:33:38 UTC） | — | 输入 SHA-256 `9396a07e2e1242fc124b3ac020e544ee0ee5a076bf751358a66e42bda053291f`；read-only |
| R33 | 全文（36 卡 v33） | FAIL | P1×3：令牌锁在可写目录中可被替换或重定向 → 私有目录 + 不跟随打开校验；`post_commit_skipped` 无回传路径 → `CommitOutcome` 经 `run_commit`/`vcs` 回传并入写集；HTTP 不支持版本缺 `-32022` 体 → 三门；P2：diff 峰值内存未测 → 计数分配器门——在 v34 修订处理（修订历史 2026-10-20 04:41:43 UTC） | — | 输入 SHA-256 `2714174ac74ba7aa9a6b4c54ace9eef77fe16484242033d843c4fff7aadf5ebf`；read-only |
| R34 | 全文（36 卡 v34） | FAIL | P1×4：MCP 写锁可跟随符号链接 → `acquire_private_lock`；actor `repo` 与仓库锁冲突 → `mcp/actors/`；lease 重试不幂等 → `operation_id` 重放与收敛；HTTP `initialize` 绕过 modern 校验 → 先路由纪元——在 v35 修订处理（修订历史 2026-10-20 04:51:16 UTC） | — | 输入 SHA-256 `b932fd036457f78792a946510e3564fd7c9446c49595f870891b38b1e18e56b2`；read-only |
| R35 | 全文（36 卡 v35） | FAIL | P1×4：lease 收敛可把旧 lease 当作新 claim 结果 → 效果与 operation 同事务提交；ADR-MC-09 与 MC-11 矛盾 → 同步；modern HTTP `initialize` 缺 404 门 → 补门；复合谓词低计（MC-11/MC-36/MC-02） → 拆分并拆出 MC-38/MC-39——在 v36 修订处理（修订历史 2026-10-20 04:58:38 UTC） | — | 输入 SHA-256 `f280b2881467ed044f344d31a2f9e34a71fe21cc2ef30b5ce11d9815597b42b9`；read-only |
| R36 | 全文（38 卡 v36） | FAIL | P1×3：主工作树无法 claim → MC-40 保留身份；MC-15 要求把绝对路径写入证据 → 本机 env 文件；MC-33 AC-5 复合 → 单一谓词；P2×2：lease 写集指向不存在路径 → 既有 `*_with_conn`；成功定义未写 lease 重放契约 → 补写——在 v37 修订处理（修订历史 2026-10-20 05:08:14 UTC） | — | 输入 SHA-256 `90ec9582f6333e0bf4670831c4c052aee5e667078f2121c9d0ba69559d2efae3`；read-only |
| R37 | 全文（39 卡 v37） | FAIL | P1×2：主工作树身份可被 linked 元数据冒用 → 命名空间校验与冲突 fail-closed；renew 无期限契约可缩短 lease → 缺省取 claim TTL 且不缩短；P2：提交前崩溃表述与事务边界矛盾 → 改写——在 v38 修订处理（修订历史 2026-10-20 05:17:05 UTC） | — | 输入 SHA-256 `75813da82b2cf10aa390f36c5c3f057fe6116e2a4fb90af6eed5ae6e2f013e49`；read-only |
| R38 | 全文（39 卡 v38） | FAIL | P1×3：重放结果与 claim TTL 无持久化 → MC-03 增加 `result_json` 与 `agent_mcp_lease`；renew/release 未校验 actor → owner 校验并加跨 actor 门（release 拆为 MC-41）；文档未写 `lease_ttl_ms` 契约 → 三处文档与文档门；P2：MC-40 返回类型 → 可失败——在 v39 修订处理（修订历史 2026-10-20 05:26:20 UTC） | — | 输入 SHA-256 `93e130d184df2fac83dcc5f620e9ecc5f1f05ee56024cf37c511106b68070147`；read-only |
| R39 | 全文（40 卡 v39） | FAIL | P1×3：崩溃门未证明意图与 operation 回滚、renew 无崩溃门 → 每个工具加回滚与重试门（经使用者批准 EX-MC-09/10）；显式更短 TTL 未测 → 加门；lease 文档缺 `workspace_id` 与 TTL 契约的网站门 → 补齐；P2：release 归属引用 → MC-41——在 v40 修订处理（修订历史 2026-10-20 05:36:54 UTC） | — | 输入 SHA-256 `2f4463a3a8d6c0b41eb62df078b7dea69d9986f2a6151f567de46ae2da9ff315`；read-only |
| R40 | 全文（40 卡 v40） | FAIL | P1×2：`outputSchema` 根缺 `type: object` → 补并加两个 legacy schema 校验门；claim 崩溃门未覆盖 `agent_mcp_lease` → 两门；P2×2：lease 文档门未按小节检查完整参数 → 小节门；恢复矩阵引用 → MC-15 AC-5——在 v41 修订处理（修订历史 2026-10-20 05:44:54 UTC） | — | 输入 SHA-256 `5289dae673e19b2247521069acfe38202afac8af0c35173fe75926775e05cb13`；read-only |
| R41 | 全文（40 卡 v41） | FAIL | P1：lease 重放可绕过当前工作树检查 → 意图保存可信工作树身份并在重放前复核（三门）；P2×2：线上页面按行 `awk` 可越界、claim 线上门未查参数 → DOM 小节检查脚本与自检——在 v42 修订处理（修订历史 2026-10-20 05:53:59 UTC） | — | 输入 SHA-256 `380a608608b9dd78e6d8df6d992928628c6d76110e6825a59dd0c5174eb22bf7`；read-only |
| R42 | 全文（40 卡 v42） | FAIL | P1×2：令牌路径可替换另一服务的锁文件 → 保留命名空间锁 + 路径限制；写记录无界增长 → 配额与保留期；P2：源文件小节门只在 `####` 处结束 → Markdown 模式按同级或更高级标题结束——在 v43 修订处理（修订历史 2026-10-20 06:03:26 UTC） | — | 输入 SHA-256 `c211a93648f9e373fa6179ad9cae97592cc10df0fb7ca2aa75c478831edde628`；read-only |
| R43 | 全文（40 卡 v43） | FAIL | P1×5：重启可覆盖无关 0600 文件 → 哈希匹配才替换；配额不覆盖 checkpoint/链接 → operation 组配额与字节配额；过期 `operation_id` 复用会绑定旧 checkpoint → 整组删除；配额与保留期未写入文档 → 四处文档与门；MC-06 复合谓词 → 拆分——在 v44 修订处理（修订历史 2026-10-20 06:14:31 UTC） | — | 输入 SHA-256 `a9b985707ec74b62c6fd8594acb0515941b8d7e928f396f11e85b02cab2396b7`；read-only |
| R44 | 全文（40 卡 v44） | FAIL | P1×7：令牌轮换非崩溃安全 → 两阶段哈希；registry 行与多 actor 绕过配额、删除破坏 lease 续期、配额与写会话混轴 → 改为 MC-42 仓库级聚合上限且不删除；大 `operation_id` 使响应超限 → 128 字节上限；复合谓词 → 随撤回消失；P2×2：保留期文档（随撤回消失）、MC-14 措辞——在 v45 修订处理（修订历史 2026-10-20 06:26:45 UTC） | — | 输入 SHA-256 `8ee55bf0a97929da0ac952e2aff64b95195e7cea102df5663176a06252650ba0`；read-only |
| R45 | 全文（41 卡 v45） | FAIL | P1×6：首次令牌创建非崩溃安全 → 同一两阶段；配额阻塞存活 lease 的结算 → renew/release 豁免且每 lease 有界；结果字节在写后才知 → 写前预留结果上界；字节求和扫描 → 单行用量计数；registry 计入无测试 → claim/release 循环门；写集指向不存在的文件 → `session.rs`；P2：字符集不一致 → 统一并加门——在 v46 修订处理（修订历史 2026-10-20 06:34:12 UTC） | — | 输入 SHA-256 `90d2d6298f73e6e809205a4224ef9bb60e67df195e6b71b1ae4b0e62ddea0c21`；read-only |
| R46 | 全文（41 卡 v46） | FAIL | P1×5：旧版本记录未计入 → 回填；失败组不计 → 意图同事务计入；字节配额仍可阻塞结算 → claim 预留结算额度；删除 renew 记录破坏重放 → 每 lease 至多 16 次 renew、不删除；首次创建缺文件 fsync → fsync 顺序门；P2：MC-42 计数——在 v47 修订处理（修订历史 2026-10-20 06:44:49 UTC） | — | 输入 SHA-256 `81588e5c62e26527c50ef8779ec5e957db8f2b64ed6c367d3548512df519898c`；read-only |
| R47 | 全文（41 卡 v47） | FAIL | P1×4：旧写入者在回填后绕过计数 → 计数前移到 MC-06、取消回填；轮换 fsync 顺序错误 → 改正并加门；网站推送可夹带无关提交 → 领先提交校验；回填字节未验 → 随回填取消；P2×2：「物理上限」措辞、安装脚本门可在部分响应下通过——在 v48 修订处理（修订历史 2026-10-20 06:53:55 UTC） | — | 输入 SHA-256 `9353344bd1560a0d7a78b541d6e8713727976af57cf5051edf9170ec73331697`；read-only |
| R48 | 全文（41 卡 v48） | FAIL | P1×4：上限晚于第一个写工具发布 → MC-42 并入 MC-06；被拒的 lease 调用仍写记录 → 拒绝先于意图并加门；令牌状态记录可撕裂 → 独立状态文件原子替换；网站推送使用可移动的本地分支 → 私有分支固定提交——在 v49 修订处理（修订历史 2026-10-20 07:06:10 UTC） | — | 输入 SHA-256 `b5a761a27c6fb0a653f81b682702de330a91593155f54f8166ae04948b147b68`；read-only |
| R49 | 全文（40 卡 v49） | FAIL | P1×2：首次令牌写入中途崩溃留下半截文件 → 临时文件 + `link` 原子发布；网站推送可夹带他人未提交文档 → 编辑前路径干净与暂存精确预检脚本——在 v50 修订处理（修订历史 2026-10-20 07:27:40 UTC；首次运行遇模型容量错误，按 ER-10 退避后重跑） | — | 输入 SHA-256 `7201ac9471ac54f2c102efba1a81990390ac3433b89400c3a33f44b3acbcfff1`；read-only |
| R50 | 全文（40 卡 v50） | FAIL | P1×5：首次创建仍有直接创建措辞 → 统一为 `link`；暂存内容可被并发改动 → 私有临时克隆；私有分支违反网站 `AGENTS.md` → 不建分支；结算额度低估 → 每对合计 2 KiB 并强制；MC-01 预检自检未计数 → 计入 AC——在 v51 修订处理（修订历史 2026-10-20 07:40:33 UTC） | — | 输入 SHA-256 `09ddfb5b3ecc16d63ecfe4c6e9fd6a82e5d2ddd668540f66c94ecfa6fdd0e96d`；read-only |
| R51 | 全文（40 卡 v51） | FAIL | P1×6：长路径使结算请求超限 → 根目录摘要；网站源文件门检查的不是提交的检出 → 回到相邻仓库；ADR-MC-07 与状态文件矛盾 → 同步；暂存内容未与审核快照比较 → 逐字节比较；私有克隆绕过 ER-06a 前置检查 → 按 ER-06a Libra 路径；MC-01 豁免范围未随新门更新 → 撤回预检脚本门——在 v52 修订处理（修订历史 2026-10-20 08:00:57 UTC；首次运行遇模型容量错误，退避后重跑） | — | 输入 SHA-256 `8d19c1d8c4261fb372df900c840b9c8a5391b38857a527d17a33fe571d8e48c3`；read-only |
| R52 | 全文（40 卡 v52） | FAIL | P1×4：超长 linked id 使结算请求超限 → 哈希有界身份；暂存路径比较形式不一致 → 仓库相对路径；审核补丁落盘违反 v2.12 → 摘要；renew 1 KiB 上限未入文档 → 补文档与门——在 v53 修订处理（修订历史 2026-10-20 08:12:05 UTC） | — | 输入 SHA-256 `1296f1e0cef14c4a0af58578011e3026ee70f688746bbe93c22d5cbaf8c2e623`；read-only |
| R53 | 全文（40 卡 v53） | FAIL | P1×4：主工作树身份无界 → 仓库身份摘要；提交缺 `-m` 在非交互下中止 → 写明消息；MC-15 状态文件 → 会话环境变量；renew 线上门缺新上限 → 补 token——在 v54 修订处理（修订历史 2026-10-20 08:24:11 UTC） | — | 输入 SHA-256 `fe825d32c9d7d93535cc777d3f1f6150960b693a63fb302ed4249f0305ec7b3c`；read-only |
| R54 | 全文（40 卡 v54） | FAIL | P1×3：网站交付检查把命令失败当成干净或相等 → pipefail 与先赋值再比较、故障注入演练；CHECK 按字符而非字节计 → BLOB 长度与写前字节校验；线上页面门缺配额 → 补 token；P2：错误码文档受影响/N/A 矛盾 → 改正——在 v55 修订处理（修订历史 2026-10-20 08:36:46 UTC） | — | 输入 SHA-256 `4b07e9323bd3f67ed3666f574ec3eba08a9180d08410eaad8e6697df56c35683`；read-only |
| R55 | 全文（40 卡 v55） | FAIL | P1×2：提交后 `rev-parse` 比较在双双失败时相等 → 先赋值非空再比较；release 被列入 renew 上限拒绝条件 → 只作用于 renew；P2：锚点过期 → 刷新——在 v56 修订处理（修订历史 2026-10-20 08:45:04 UTC） | — | 输入 SHA-256 `6d746ecf0e1e254b6645d7c04785fc362b1c454db8d399f50b201ad8c923a565`；read-only |
| R56 | 全文（40 卡 v56） | FAIL | P1：MC-40 AC-4 复合 → 单一谓词；P2：diff 锚点过期 → 刷新——在 v57 修订处理（修订历史 2026-10-20 09:10:05 UTC；首次运行遇模型容量错误，退避后重跑） | — | 输入 SHA-256 `532e5c3fae0774ff9b6fc41c64e76d6c08c09352c76d304ba9c9d069ca433c4e`；read-only |
| R57 | 全文（40 卡 v57） | FAIL | P1：分支查询失败成为空模式而误通过 → 先赋值非空并加故障注入门；P2：Grok 重试导致「恰为一条」误失败 → 至少一条并关联——在 v58 修订处理（修订历史 2026-10-20 09:22:12 UTC） | — | 输入 SHA-256 `9719d9d2a8061d553d20b42f2a347929297436e62bdb7c5d87fd844b4bbe3f1a`；read-only |
| R58 | 全文（40 卡 v58） | FAIL | P1×2：MC-15 的脚本缺失（编辑脚本缺陷所致）→ 恢复；分支查询以 `;` 连接、打印后失败仍通过 → `&&` 串联与新的故障注入——在 v59 修订处理（修订历史 2026-10-20 09:28:43 UTC） | — | 输入 SHA-256 `d42fa1b3410bcd31b33ae5d3ee2a1d5fcbfc07d636ae64f20b8585f74fdc5b7e`；read-only |
| R59 | 全文（40 卡 v59） | FAIL | P1×3：探针清理与分支检查可在命令失败时通过 → 先赋值检查再比较并加故障注入；两客户端「互不阻塞」与写锁矛盾 → 如实描述并加同仓库并发门；MC-24 合并两条前提 → 拆出 MC-43；P2：DEFER-MC-10 指向已并入的卡 → 改指 MC-06——在 v60 修订处理（修订历史 2026-10-20 09:41:03 UTC） | — | 输入 SHA-256 `699ca47a719629a9701561040416ea12b0f6105826ebc058cc97765619eb3f30`；read-only |
| R60 | 全文（41 卡 v60） | FAIL | P1×4：暂存检查可在「打印后失败」时通过 → 先检查赋值；HTTPS 探测接受失败请求 → 检查赋值与超时；首次启动令牌锁标识未定义 → canonical 父目录 + 文件名；关闭门把查询失败当作退出 → 区分 ESRCH；P2×2：MC-43 归属引用、写串行归属——在 v61 修订处理（修订历史 2026-10-20 09:48:55 UTC） | — | 输入 SHA-256 `231bd75de6857de3fd85af411b2740fdfa118dcc11b251b7d06a0eb44d859db3`；read-only |
| R61 | 全文（41 卡 v61） | FAIL | P1×3：令牌锁随 `LIBRA_HOME` 分裂 → 锁放令牌父目录；网站同步可改写无关页 → `--only`；HTTP 请求体无接收期限 → 10 s 期限与准入时点；P2：PR CI ref 标注 → 合成 merge ref 与 head SHA——在 v62 修订处理（修订历史 2026-10-20 10:00:45 UTC） | — | 输入 SHA-256 `2a1bbce5e541fe91f8a7e17126f60ef3abcafe038d2480e124a2fc1dbf5fc7df`；read-only |
| R62 | 全文（41 卡 v62） | FAIL | P1×4：请求体期限可被逐帧重置且不覆盖头部 → 连接层头部期限 + 绝对请求体期限；准入前读取无并发上限 → 连接准入；`tower-http` 非直接依赖 → 改用并声明 `hyper`/`hyper-util`；令牌锁公式不一致 → 统一——在 v63 修订处理（修订历史 2026-10-20 10:09:01 UTC） | — | 输入 SHA-256 `c13bf1a4ac3bf0866a29431a5b2c898f818973171744334cc3bc23d91e20f9b5`；read-only |
| R63 | 全文（41 卡 v63） | FAIL | P1×3：HTTP/2 绕过连接准入 → 只服务 HTTP/1.1；MC-02 超 G-03、豁免卡低计复合判据 → 以成功为前提的单一谓词；P2：ADR-MC-03 依赖表述 → 改为不引入 MCP SDK——在 v64 修订处理（修订历史 2026-10-20 10:17:45 UTC） | — | 输入 SHA-256 `9c7e5e77015b5050da762154319411ab103abf07483dbbbd303af35e50f41125`；read-only |
| R64 | 全文（41 卡 v64） | **PASS** | P0=0、P1=0；P2×1：HTTP 内存预算未计 Hyper 读缓冲 → 修正预算表述（R65 对 `max_buf_size` 方案提出 P1 后撤回该方案，R66 确认最终表述） | P2 已修 | 输入 SHA-256 `7562e304594148bfe5909fd4ed78c51f7ae6b7173c19455907d8993ec1818c77`；read-only |
| R65 | 差异确认（v64→v65） | FAIL | P1：`max_buf_size` 在 Hyper 1.8.1 下可被超长头部绕过（hyper#4081）→ 撤回该方案，只修正预算表述 | — | read-only |
| R66 | 差异确认（R64 PASS 快照 → 最终版） | **PASS** | P0=0、P1=0、P2=0：MC-13 预算表述准确（容量估计，非硬上限），与 R64 快照相比只改该句与日志行 | — | 对照快照 SHA-256 `7562e304594148bfe5909fd4ed78c51f7ae6b7173c19455907d8993ec1818c77`；read-only |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-MC-01 | MCP 工具 `review.run`、session/event ingress、transcript 导出（含 raw）、`push`、agent checkpoint rewind、TaskCopy workspace | 外部进程拉起、Harness 专属捕获语义、敏感数据面 | 用户提出具体需求且有审批模型 | 新日期计划 |
| DEFER-MC-03 | `--repo <path>` 选择仓库而不依赖 cwd | status/diff 复用路径依赖进程 cwd | MC-15 证明客户端无法设置 `cwd` | 新卡 |
| DEFER-MC-04 | 每令牌独立 actor 与多令牌 | 本期单令牌即进程授权 | 多客户端共享一个服务端的需求 | 新卡 |
| DEFER-MC-05 | Windows 上的 HTTP 传输（令牌文件 DACL 校验） | `validate_token_file_perms` 在非 Unix 不校验 | 有 Windows 远程部署需求 | 新卡 |
| DEFER-MC-07 | MCP restore 工具（原 MC-09）及其与 CLI/Harness 等非 MCP 写入者之间的原子互斥（仓库级写锁或工作树快照 CAS） | Libra 无跨命令的仓库写锁，restore 无法在检查与写入之间排除并发写入者，不满足「互不干扰」 | 仓库级写锁计划落地，或用户接受文档警告下的非原子 restore | 新日期计划 |
| DEFER-MC-08 | 非 Unix 平台上的 `--allow-write` | 本计划写路径端到端门（写锁、写入期限退出、hook 拒绝）只在 Unix 上验证；Windows CI 只运行 scoped-input 夹具 | 有 Windows 写入需求，且写锁/期限退出/hook 拒绝门在 Windows runner 上落地 | 新卡 |
| DEFER-MC-09 | 在 MCP 提交中运行仓库 hook | 进程组无法约束主动 `setsid`/守护化的派生进程；安全运行需 cgroup/Job Object 级的全派生进程隔离，并在写锁释放前证明全部回收 | 使用者需要在带提交 hook 的仓库中经 MCP 提交 | 新卡 |
| DEFER-MC-10 | MCP 写记录的清理命令 | 自动删除会破坏重放、checkpoint 读取与 lease 续期（R43/R44 结论），清理需要显式的运维语义（按 actor/时间删除并处理存活 lease） | MC-06 的仓库级写记录上限（10000 组或 512 MiB 记录载荷）在实际使用中被触及 | 新卡 |
| DEFER-MC-06 | 按仓库状态精确收敛崩溃中断的 commit（替代 quarantine） | 需要可靠的效果指纹（提交身份、工作树快照），本期以显式不确定态 fail-closed | quarantine 在联调或使用中造成明显负担 | 新卡 |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [ ] 所有任务卡满足粒度规则 `G-*`：无 L/XL、无碎片卡、无未登记合并发布、实现写集冲突已用顺序边消解；超限列均带有效 `EX-MC-*`；「任务卡粒度审计表」已填齐。
- [ ] MC-01..MC-08、MC-10..MC-41、MC-43 均（MC-42 已并入 MC-06） `Lifecycle=done` 且 `Acceptance=complete`（除 MC-15 外的实现卡 D-MC-1..4 绿；MC-15 的 C/D 来自 CLOSE-MC）；MC-09 已撤销（DEFER-MC-07）。
- [ ] 所有任务的 Verification、强制门与同卡文档门已按 GC-MC-07 运行并记录结果。
- [ ] 全量收口门：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿（CLOSE-MC；`cargo test --all` 仅按 GC-MC-09 诊断）。
- [ ] 全量收口门暴露的 Bug 已前滚修复并重跑全量至全绿；既有失败附计划基线 `85dcc505` 上的复现证据并登记 `FIX-*` 或 `DEFER-*`。
- [ ] docs/compat/test-index/网站更新完成；`docs/error-codes.md` 的 MCP 注记三项均在（同上 `rg` 命令退出码 0）。
- [ ] MC-03 的 forward-only 恢复已用 `agent_bridge_migration_test` 验证；其余卡的 immutable-release 降级指引已写入文档。
- [ ] 每卡 Codex review 最终 PASS，P0/P1 全部关闭。
- [ ] 每张实现卡 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新，`gh release create v<version>` 已触发 `release.yml` 并取得 D-MC-3/4 证据。
- [ ] 「修订历史」记录全部规范性变更。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md` 与 `plan-status.md` 已同步。
