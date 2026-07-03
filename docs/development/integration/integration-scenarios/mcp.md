# `libra mcp` 独立命令拆分计划

> Status: draft
> Last updated: 2026-06-23
> Scope: 从 `libra code --stdio` / `--mcp-stdio` 中拆出 MCP protocol/tools/resources，形成独立 `libra mcp --stdio` 命令。Agent 外部调度仍走 WebSocket/Web API；MCP 不作为 Agent turn 控制面。

## Decision

`libra mcp` 是独立的 MCP protocol 命令族，不挂在 `libra code` 下。

目标是把 MCP client 集成面和 Code UI / Agent control 面拆开：

- `libra code`：Web Code UI + AgentRuntime，负责 message submit、interaction respond、cancel、observe、snapshot、controller lease。
- `libra mcp --stdio`：MCP tools/resources/protocol transport，服务 Claude Desktop 等 MCP client。
- `libra code-control`：TUI automation shim 的遗留命令；Web-only 后按 agent 迁移计划删除或改为纯 Web API shim。

## Final CLI Contract

最终保留：

```bash
libra mcp --stdio
libra mcp --stdio --cwd <path>
libra mcp --stdio --repo <path>
```

最终不再支持：

```bash
libra code --stdio
libra code --mcp-stdio
```

`libra code --stdio` / `--mcp-stdio` 必须 fail fast，并提示迁移到 `libra mcp --stdio`。该提示不得暗示 MCP 可以 submit/respond/cancel Agent turn。

## Boundaries

- `libra mcp --stdio` 不接受 Agent turn submit/respond/cancel/observe 请求。
- MCP resources/tools 可以继续暴露只读或受权限控制的 VCS / diagnostics / context 能力，但 mutating tools 必须经过既有 `McpAuthorizer`、tool policy、redaction 和 audit。
- 若未来支持 MCP notification/source，它只能作为 bounded event source 进入 runtime queue，且默认不注入 chat；外部主动调度 Agent 仍走 WebSocket/Web API。
- MCP stdio 独占 stdin/stdout；不得输出 warning、banner 或非 JSON-RPC 文本污染协议。
- `code-control --stdio` 不是 MCP server，不能复用为 `libra mcp` 实现。

## MCP 对象模型与引用完整性

本节约束 MCP 中针对 `git-internal` typed AI objects 的工具面。其它 MCP tool surface（例如 Memory）可以使用各自文档定义的事实源，但仍必须遵守本文的 stdio transport、authz、redaction、audit 与非 Agent 控制面边界。

针对 typed AI objects，MCP 的持久化与 list/read 路径对齐 `git-internal` 的事件溯源（event-sourced）对象模型（完整对象模型见 [`docs/ai/object-model-reference.md`](../ai/object-model-reference.md)）：

- 核心对象 `Intent` / `Task` / `Run` 不可变（immutable）；生命周期由 `IntentEvent` / `TaskEvent` / `RunEvent` 重建。`list_intents` / `list_tasks` / `list_runs` 与 `libra://context/active` 均根据最新事件推导状态。
- MCP 的 create/update 流程发出对应的生命周期事件，而非原地修改对象；`PatchSet` 无 `apply_status`，验收/拒绝（acceptance/rejection）通过 `Decision` 与 run 事件表达；`Provenance` 使用结构化 `parameters`（含 `temperature` / `max_tokens`）。

为避免悬空的工作流图，MCP 的 create 流程在存在 history manager 时校验被引用的 ID 与关系完整性：

- 必须存在的引用：`Run` 的 `task_id` / `plan_id` / `context_snapshot_id`；`patchset` / `evidence` / `tool-invocation` / `provenance` / `decision` 的 `run_id`；`Task` 的 `intent_id` / `parent_task_id` / `dependencies`；`Intent` 修订的 `parent_ids`；`Decision` 的 `chosen_patchset_id`。
- `Evidence.patchset_id` 与 `Decision.chosen_patchset_id` 必须引用归属于**同一 `run_id`** 的 patchset。
- 当所选 plan 的 intent 与 task 绑定的 intent 不一致时，`Run.plan_id` 被拒绝；当父 plan 归属于不同 intent 时，`Plan.parent_plan_ids` 被拒绝。
- `update_intent` 在查找前对 `intent_id` 归一化，`uuid:<id>` 与纯 UUID 均被一致接受。

> 本节并入了原 `mcp-upgrade-report.md`（`git-internal` 0.6→0.7 事件模型迁移）中长期有效的契约；一次性的迁移日志、提交记录与验证结果不再单独保留。

## Tool Surfaces

`libra mcp --stdio` 可以暴露多类有界工具面，但它们共享同一 transport 与安全边界：

- **Typed AI workflow tools**：操作 `Intent` / `Task` / `Run` / `Evidence` / `Decision` 等 `git-internal` typed AI objects，遵守本文的对象模型与引用完整性规则。
- **VCS / diagnostics / context tools**：暴露只读或受权限控制的仓库、诊断与上下文能力；任何 mutating 行为都必须经过 `McpAuthorizer`、tool policy、redaction 和 audit。
- **Memory tools**：由 [`docs/development/memory.md`](./memory.md) 定义，作为 bounded data tools 挂在 `libra mcp --stdio` 下。Memory 的事实源是 `refs/libra/memory*` 上的普通 Git history 加可重建 SQLite 投影，而不是 `git-internal` typed AI object。

Memory MCP tools 的 mutating 操作（例如 `memory_remember`、`memory_confirm`、`memory_resolve`、`memory_revoke`、`memory_forget`、`memory_onboard`）必须经过 `McpAuthorizer`、tool policy、redaction 和 audit；只读操作必须执行 scope、namespace、actor 可见性与 sensitivity 门禁。MCP 不得因为暴露 Memory tools 而获得 Agent turn submit/respond/cancel/observe 能力。

## Implementation Plan

1. 新增 CLI 子命令：在 `src/cli.rs` 注册 `mcp`，在 `src/command/mcp.rs` 或等价模块定义 `McpArgs`。
2. 抽出 stdio runner：从 `src/command/code.rs::execute_stdio(args: &CodeArgs)` 提取协议无关 helper，例如 `run_mcp_stdio(working_dir: &Path) -> CliResult<()>`。
3. 工作目录解析：`McpArgs` 支持 `--cwd` / `--repo` 或等价路径解析，默认使用当前目录。
4. 旧入口迁移：`libra code --stdio` / `--mcp-stdio` 改为 usage error，错误文案指向 `libra mcp --stdio`。
5. 兼容矩阵：新增 `COMPATIBILITY.md` 条目，保证 `cargo test --test compat_matrix_alignment` 不因新命令漂移。
6. 命令文档：新增 `docs/commands/mcp.md`，并更新 `docs/commands/code.md` / `docs/commands/code-control.md` 中的 stdio 边界说明。
7. 测试索引：若新增 integration target，更新 `tests/INDEX.md`；若沿用既有 MCP e2e target，更新测试说明即可。

## Verification

- `libra mcp --help` 显示 stdio 用法。
- MCP stdio integration test 覆盖新命令入口。
- `libra code --stdio` 非零退出，并包含 `libra mcp --stdio` 迁移提示。
- 新 MCP stdio test 断言 Agent turn 控制面不从 MCP stdio 暴露。
- Memory MCP tools 不暴露 Agent turn submit/respond/cancel/observe；mutating Memory tools 必须经过 `McpAuthorizer`，只读 tools 必须执行 scope / namespace / actor 可见性检查。
- MCP stdio 模式下的 Memory tools 不输出非 JSON-RPC 文本，且 `SecretLike`、`private:<actor-ref>` 与 `forget` redacted 内容不得越权返回。
- Memory tool 参数 schema 与 `libra memory ... --json` 输出保持兼容；新增字段 additive，重命名 / 删除 / enum 字面量改变必须更新 compat tests。
- `cargo test --test compat_matrix_alignment` 通过。
- `rg -n "libra code --stdio|--mcp-stdio|MCP/stdio mode" docs/commands README.md tests` 只剩迁移说明或无结果。

## Relationship To Agent Plan

[`docs/development/code-agent-runtime.md`](code-agent-runtime.md) 只负责 Web-only AgentRuntime / ControlAdapter 收敛：TUI 不再作为生产操作入口，MCP 不作为 Agent 调度面。`libra mcp` 的 CLI grammar、stdio transport、compat docs 和 MCP e2e 验收全部由本文跟踪。
