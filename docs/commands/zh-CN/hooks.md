# `libra hooks`

由外部 AI 代理 hook 配置调用的内部入口点，用于将生命周期事件（会话开始、提示提交、工具使用、模型更新、压缩、停止、会话结束）捕获到 libra 会话存储中。操作者几乎不会直接输入 `libra hooks ...`，由 `libra agent enable` 安装的 hook 配置会引用这些子命令。

## 概要

```
libra hooks claude   {session-start|prompt|tool-use|model-update|compaction|stop|session-end}
libra hooks codex    {session-start|prompt|tool-use|permission-request|compaction|stop|session-end|subagent-start|subagent-end}
libra hooks gemini   <event>   # 拒绝并给出提示：gemini 已是 uninstall-only（AG-17）
```

## 说明

`libra hooks` 是由 Claude Code / Codex hook 配置调用的**隐藏**（clap 中 `hide = true`）兼容性表面。每次调用都会从 stdin 读取单个 hook 事件 payload（JSON），根据提供商特定 schema 验证它，并将脱敏后的投影记录到外部代理捕获存储（`agent_session` / `agent_checkpoint` + `refs/libra/traces`）。

该命令被隐藏，因为：

- 它不是面向用户的 CLI 契约的一部分；它必须继续能被 hook 配置调用，而这些配置格式由上游提供商（Claude Code / Codex）拥有，不由 Libra 拥有。如果将其视为公共表面，就需要在 Libra 侧冻结 JSON payload schema，但这不可能，因为提供商可在任意版本中更改 payload。
- 它产生的事件会被 `libra agent session list`、`libra agent checkpoint *` 和 `libra agent doctor` 读取。用于检查已捕获会话的公共表面是 `agent` 子命令（[agent.md](agent.md)），不是 `hooks`。

`libra hooks claude <verb>` 是 `libra agent enable --agent claude-code` 写入项目 `.claude/settings.json` 的稳定调用面；`libra hooks codex <verb>`（AG-19）是 `libra agent enable --agent codex` 写入 `$CODEX_HOME/hooks.json` 的稳定调用面。两者都记录到 AgentTraces 捕获存储（`refs/libra/traces`）；claude 历史上路由到 `refs/libra/intent` 写入器的行为已由 Task A6.5 本地采集 smoke 收敛（该 smoke 要求已安装 hook 的采集出现在 `libra agent session/checkpoint list` 中）。Codex 额外转发原生子代理边界（`subagent-start` / `subagent-end`）。

安装器会为每条受管命令追加隐藏且受限的 `--capture-budget-ms` 参数：它由 provider handler timeout 推导，并为 terminal pending-finalizer 清理保留最多 1000ms（历史的一秒 handler 保留可用的 `500ms` capture slice；默认 Claude `10s → 9000`、Codex 普通事件 `30s → 29000`）。Codex `SessionEnd` 受宿主上限约束，即使其他 Codex timeout 更长也固定为 `3s → 2000`。这不是公开调参 flag；旧配置可通过重新运行 `libra agent enable --agent <provider>` 原位刷新。runtime 会在读取 hook stdin 前将该值换算为一对单次 deadline 时钟，限制数据库等待，且不会重锚 terminal receipt 的原始 wall-clock deadline。打开仓库数据库与其它仓库命令相同：先自动应用待处理的 schema 迁移（每个版本原子执行，锁等待受 hook 数据库时间片约束），较新 Libra 写入的数据库以下表所列不含路径的 `LBR-IO-001`（安装较新 Libra）拒绝。该预算约束可杀 helper、数据库和其它 deadline-aware 阶段；对当前每条 hook 独立进程而言，它不是严格端到端 wall-clock 保证。source 分类、canonicalize、安全打开和 rewind 都在 descriptor 交接前同步发生，NFS/FUSE stall 无法被强制取消。

只有可信的 `SessionEnd` 会创建用于 autonomous recovery 的 durable pending artifact handoff。后续
`SessionStart` 会检查有界索引队列，并可能启动 detached、source-free recovery worker；hook 本身不会等待
该 worker。其它生命周期事件（包括 turn-level `Stop`）不会创建该 artifact。因此 recovery 保存的是最近
符合条件的 terminal snapshot，而不是每个中间 turn：如果 provider 不会重新投递被中断的 turn，该 turn
可能没有 checkpoint。`libra agent doctor --repair` 仍是可检查的前台恢复路径。
无法认证或无法匹配当前 catalog receipt、已被 supersede、所属 session 已 quarantine，或已耗尽原始重试预算的候选项会保留在 quarantine 中，并退出自动 replay 队列。临时数据库/截止时间故障以及仍有 live writer 的尝试会保留 pending，等待后续 hint。quarantine evidence 仍可通过 `libra agent doctor` 检查；显式 repair 仍受原 terminal receipt policy 限制。
worker 只选择当前 worktree 的候选项；属于其他 worktree 的 artifact 会保留 pending，等待该 worktree 的 hint。
workspace identity 和 lease fence 也是同一选择边界的一部分；同一 worktree 中属于其他 workspace 的 artifact
同样会保留 pending，不会被当前 worker 隔离。
pending/quarantine 的有界保留容量由同一 repository 的所有 worktree 共享；队列满时，新增 terminal artifact
可能无法保留，应先运行 `libra agent doctor` 检查和处理队列。

当 terminal snapshot 仍作为未发布的 recovery artifact 保留时，同一 `SessionEnd` 的原生重投会以不含敏感内容、可重试的消息非零退出，而不会再次采集：它不重读 provider source、不预留 coverage，完成交由 `SessionStart` worker 或 `libra agent doctor --repair`。保留中的 artifact 所持有的 coverage claims 在该重放完成或 session 被显式 erase 前不会被后续 writer 接管，因此 resumed session 的后续 checkpoint 会跳过这些 turn，同一 session 的 import 会报告 partial coverage。可信 `SessionEnd` 若从与 session 已记录 working directory 不同的目录投递，会以显式 incomplete 非零退出：不保留 artifact，也不提示 recovery worker。来自该目录的非终态事件仍为辅助性质。

当前 Claude 与 Codex command handler 还会写入 provider 支持的
`statusMessage: "libra capture"` 标记。刷新或停用时，只有该标记、直接的绝对
command-path grammar、forwarded verb，以及 handler timeout/budget 对都精确匹配的
handler 才会被认定为 Libra 所有。未标记 command（无论裸 `libra`、标准命名还是重命名）
在停用时一律不认领；只有显式 enable 且 matcher-less 的直接绝对 legacy/canonical command
的 executable 与本次选择的 binary 完全相同，才会迁移为带标记形态，之后才能由停用移除。

Codex 按 matcher-group 与 handler 的位置记录 hook trust。移除位于后续用户 group 之前的
纯 Libra group 时，Libra 会保留空占位，确保用户已有 approval 的位置不变。如果一个
matcher-less group 混有 Libra 托管 handler 与用户 handler，enable 和 disable 会在改写
`hooks.json` 或 `config.toml` 前拒绝：先将两者拆到独立 group（或手动删除 Libra handler），
然后重新批准剩余的用户 hook。

Codex 的 enable/disable 将 `$CODEX_HOME/hooks.json` 与 `config.toml` 视为一次耦合更新。
Libra 以 `$CODEX_HOME` 锁串行自己的命令，在发布任一文件前准备两者，并检测每次替换前
可观察到的编辑。运行 `libra agent enable` 或 `disable --agent codex` 时请不要编辑这两个
文件：任意编辑器无法参与可移植的原子“比较并替换”。若并发编辑错误报告了非明文完整性恢复
日志路径，请保留该日志，先手动解决编辑冲突，移除所报告的日志后再重试；日志只包含有限的事务
元数据（schema 版本、操作、阶段和恢复围栏）及域隔离的文件指纹，不含原始设置快照。未知指纹
状态会刻意保持零写入，而非自动修复。

受管 Session Capture 当前要求 Unix host 才能以安全的 descriptor-relative、no-replace
publication 初始化 repository-private deduplication key。non-Unix platform 会在创建任何
key file 前拒绝 capture，并给出可操作的诊断，而不会弱化该安全边界。这个固定能力失败
对 Codex 绝不静默：nonterminal `libra hooks codex` callback（以及历史
`libra agent hooks codex` alias）会向 stderr 输出不含路径的 Unix-host remedy 并以
`0` 退出；`SessionEnd` 则携带同一 remedy 以非零退出。fail-closed 的已安装 Claude
表面与隐藏的 `libra agent hooks` Claude Code / OpenCode 别名对每个事件都以非零退出，
并携带同一条不含路径的 remedy 与 `LBR-UNSUPPORTED-001`，绝不输出通用的“retry the hook”
信息。这个狭窄的能力例外不会放宽
历史 alias 对 malformed envelope 的 fail-closed 处理。它只适用于在 Libra 仓库内触发的
回调：在任何 Libra 仓库之外触发的回调在所有平台（无论是否为 Unix）上都遵循下文的仓库外契约。

Codex 回调在存在可信终态边界之前对宿主 Agent 属于辅助采集：malformed/unbound callback 与 nonterminal capture failure 只记录经净化的 tracing 警告并成功退出；此时没有可信 session/scope 可写入 terminal receipt。一旦 scope-bound `SessionEnd` 已通过验证，Libra 必须持久化或观察到 durable completion/pending recovery evidence；若做不到（例如受限数据库锁或 finalizer 失败），会以非零退出，而不会静默确认已丢失的终态边界。成功的 terminal retry 可以为 `libra agent doctor` 保留 pending，也可以正常完成；已有 catalog reservation 后的狭义 nonterminal checkpoint/maintenance-lock 失败仍可仅在已有 session 上留下不含敏感内容、可重试的诊断。

在任何 Libra 仓库之外的工作目录触发的回调没有可绑定的 scope，绝不会成为可信终态边界：已安装的 Codex 表面会确认该回调并以 `0` 退出（包括 `SessionEnd`），且不创建任何仓库状态；已安装的 Claude 表面与隐藏的 `libra agent hooks` 别名对格式正确的 frame 返回固定、不含路径的仓库未找到错误（`LBR-REPO-001`，退出码 128），malformed frame 仍会先以 `LBR-AGENT-008` 拒绝。已损坏的活动仓库（例如无法读取的 linked worktree `commondir`）不视为“仓库之外”：它仍是可信失败，且 fail-closed 表面（已安装的 Claude、隐藏的 `libra agent hooks` Claude Code / OpenCode / Codex 别名，以及两个 gemini 入口）返回与其他所有命令的仓库 preflight 相同的固定、不含路径的稳定错误码，绝不返回通用的 `LBR-INTERNAL-001` capture 失败：

| 已损坏的活动仓库 | 稳定错误码（退出码 128） | 消息中的修复建议 |
|------------------|--------------------------|------------------|
| 无法解析 storage（detached、迁移中或损坏的 linked worktree） | `LBR-REPO-003` | 在主 worktree 运行 `libra worktree repair --confirm <worktree-path>`（或重新添加该 worktree） |
| 仓库数据库缺失 | `LBR-REPO-002` | 恢复仓库的 `.libra` storage |
| 仓库数据库无法打开 | `LBR-IO-001` | 若由更新版本的 Libra 写入则安装更新的 Libra，否则恢复仓库 storage |
| 无法读取 `core.objectformat` | `LBR-IO-001` | 修复仓库数据库 |
| `core.objectformat` 不受支持 | `LBR-REPO-002` | 修复 `core.objectformat` |

优先级依次为：malformed frame（`LBR-AGENT-008`）与仓库之外的调用（`LBR-REPO-001`），其次是 non-Unix 能力拒绝（`LBR-UNSUPPORTED-001`），然后是上述仓库类别，最后是通用 capture 失败。已安装的 Codex 表面保持自身的退出策略：nonterminal 回调被确认（退出码 `0`），可信 `SessionEnd` 以非零退出——活动仓库损坏时给出与上述相同的仓库错误码与修复建议，其余情况给出通用终态诊断。

`libra hooks gemini <verb>` 不再摄入：gemini 已是 uninstall-only（AG-17），降级前安装的过时 hook 配置会得到指向 `libra agent remove gemini` 的可操作错误，而不是静默捕获数据。在任何 Libra 仓库之外，它（以及隐藏的 `libra agent hooks gemini` 别名）改为返回固定的仓库未找到错误（`LBR-REPO-001`，退出码 128）；在已损坏的活动仓库中则返回上表所列不含路径的 `LBR-REPO-003` / `LBR-REPO-002` / `LBR-IO-001`（数据库按普通仓库命令的方式打开：自动应用待处理迁移，并拒绝较新 Libra 写入的 schema）。

## 只读 / 不可变安装

hook 入口不参与 Libra 的自动升级机制（issue #502）。hook 宿主可能在 Libra 安装目录位于只读文件系统（不可变容器、CI 沙箱、只读挂载）的环境中运行，此时自动升级的启动恢复门无法获取 `<install-dir>/.libra-upgrade.lock`，会把 `auto-upgrade recovery could not complete` 警告泄漏进宿主的 hook 诊断。`libra hooks <provider> <event>` 因此同时跳过启动恢复门和 `upgrade.mode=auto` 检查：不尝试创建 `.libra-upgrade.lock`、不产生自动升级警告，也不向配置的 hook budget 增加自动升级工作。普通（非 hook）命令不受影响，保留既有自动升级行为——崩溃的安装事务仍由下一条常规命令恢复。

hook 入口（包括隐藏的 `libra agent hooks` 别名）在参数解析后立即分派，因此还会绕过常规命令路径中的另外三个步骤：

- **Operation 记录。** hook 回调绝不会被记录为 operation：它不会出现在 `libra op log` 中，也绝不会成为 `libra op undo` / `libra op restore` 的目标。其 capture 状态记录在 capture catalog（`agent_session` / `agent_checkpoint`，见 `libra agent session list` 与 `libra agent checkpoint list`）以及 `refs/libra/traces` 上。Operation view 仍会快照该 ref，但 `libra op undo` / `op redo` / `op revert` / `op restore` 会让 `refs/libra/traces` 以及其它 Libra 拥有的 capture / history 分支（`intent`、`libra/intent`）保持当前值，而不会把它们回退到 view 中的值。因此在被恢复的 view 之后捕获的检查点仍可从该 ref 到达，后续回调也会继续扩展同一条链。
- **全局配置 schema 策略。** 由更新版本 Libra 写入的全局配置库绝不会阻塞回调，分派器也不会为其输出 schema 警告。写入 checkpoint 对象的回调仍可能输出存储层唯一的一条回退警告（忽略较新的全局存储配置，改用本地存储）。
- **Object-index repair 重放。** 回调不会重放待处理的 durable object-index repair marker，由下一条普通仓库命令重放。仓库数据库本身仍按上文所述打开：自动应用待处理 schema 迁移，并拒绝较新 Libra 写入的 schema。

捕获失败在不暴露 hook 载荷的前提下保持可观测：Codex 路径记录一条不含敏感信息、使用 closed-vocabulary reason 的 `agent.hook.ingest` tracing 警告。仅当较晚的狭义失败路径已拥有 capture reservation 时，才尝试留下可重试的 catalog 诊断；通用 ingress 失败绝不会为了记录诊断而伪造 session。可信 `SessionEnd` 若无法创建其必需 evidence，会显式返回给 provider，而不会被这一辅助策略掩盖。另见 `docs/development/plan/plan-20260904.md` 中 CX-03 跟踪的 hook 调用可观测性工作。

无效 ingress 绝不会持久化：已安装的 Claude 表面及隐藏的 `libra agent hooks` 别名会对大小 / UTF-8 / JSON / schema / reported cwd / transcript 路径无效的 frame 以 `LBR-AGENT-008`（退出码 128）拒绝，且不回显 stdin。两个路径类字段会在打开 scope 或存储前限制为 4096 字节。已安装的 Codex 表面保持宿主安全的确认策略（退出码 0），但只输出经净化的诊断，且不会为无效 frame 创建 session 或 checkpoint。

要启用捕获，对 supported roster 中的 agent 运行 `libra agent enable --agent <name>`；这会安装提供商 hook 配置。要禁用捕获，运行 `libra agent disable --agent <name>`。

## 提供商和事件

提供商的事件分类有意不同。下表的命令 verb 是稳定的 hook 目标；多个原生 provider
事件名可能会复用同一个 capture verb。

### Claude Code

Claude Code 识别以下生命周期 verb：

| Verb | 触发条件 |
|------|---------|
| `session-start` | 新会话打开（提供商启动或 `/new` slash） |
| `prompt` | 用户提交提示（UserPromptSubmit hook） |
| `tool-use` | 工具调用（PreToolUse / PostToolUse hook） |
| `model-update` | turn 内模型切换 |
| `compaction` | 提供商压缩其内存上下文 |
| `stop` | 用户在 turn 中途按 Esc / 点击 Stop 按钮 |
| `session-end` | 会话干净关闭 |

对 Claude Code，`tool-use` verb 会**同时**为 `PreToolUse`（在工具运行前触发——
提供更早的活跃信号）和 `PostToolUse`（在工具返回后触发）安装；两者都映射到 `ToolUse`
生命周期事件——经 capture/traces 路径刷新活动会话的活跃（last-event）状态，但**不**
物化 committed checkpoint（checkpoint 仅在 `stop` / `session-end` 落盘）。Claude 不注册
任何 `Subagent*` 边界事件（只有 Codex 发出原生子 agent 边界），因此 Claude 磁盘上的子
agent 内容被捕获为 `unresolved`。

### Codex

Codex 有 11 个原生事件名，并折叠为 9 个命令 verb。它没有 `ModelUpdate` 事件：为
Codex 配置 `model-update` 不受支持，可能作为未知 callback 被跳过。`PermissionRequest`
以及两个原生子 agent 边界是 Codex 专有的 capture 事件。

| Codex 事件名 | 安装的命令 verb |
|--------------|----------------|
| `SessionStart` | `session-start` |
| `UserPromptSubmit` | `prompt` |
| `PreToolUse`、`PostToolUse` | `tool-use` |
| `PermissionRequest` | `permission-request` |
| `PreCompact`、`PostCompact` | `compaction` |
| `Stop` | `stop` |
| `SessionEnd` | `session-end` |
| `SubagentStart` | `subagent-start` |
| `SubagentStop` | `subagent-end` |

每个 AgentTraces 事件都会从 stdin 读取其提供商特定 JSON payload，通过已验证 ingress，并经 capture coordinator 仅持久化脱敏后的 catalog/checkpoint 投影。`AgentTraceEvent` JSONL 仍是独立的 legacy `HookTarget::AiIntent` 兼容路径；已安装的 Claude/Codex hook 不以该 JSONL event 作为其 capture store。Codex 会确认 malformed/unbound callback 与辅助的 nonterminal capture failure；可信 `SessionEnd` 的持久化失败为非零，避免静默丢失终态边界。Claude 保持其原有的验证失败状态。提供商 hook 将安装器所有的受限 deadline 应用于可杀 helper 和其它 deadline-aware 阶段。如上所述，descriptor 交接前的同步 host filesystem 工作可能超过该预算，operator 不得将它当作端到端 hard timeout。

## 选项

除了全局选项（`--json`、`--quiet` 等）外，`libra hooks` 不接受任何标志。事件种类由位置子命令路径选择。

## 示例

```bash
# Claude Code SessionStart hook（典型 hook 配置调用）
libra hooks claude session-start

# Claude Code UserPromptSubmit hook
libra hooks claude prompt

# Claude Code PreToolUse / PostToolUse hook
libra hooks claude tool-use

# Claude Code Stop hook
libra hooks claude stop

# Claude Code SessionEnd hook
libra hooks claude session-end

# Codex SessionStart hook（AG-19 采集路径）
libra hooks codex session-start

# Codex SubagentStart hook（原生子代理边界）
libra hooks codex subagent-start

# Gemini hooks 会被拒绝并给出提示（uninstall-only，AG-17）：
#   libra hooks gemini <event>  ->  'libra agent remove gemini'
```

由 `libra agent enable --agent claude` 安装的 Claude Code hook 配置大致如下：

```json
{
  "hooks": {
    "SessionStart": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude session-start --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude prompt --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "PreToolUse": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude tool-use --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "PostToolUse": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude tool-use --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "Stop": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude stop --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "SessionEnd": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude session-end --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}]
  }
}
```

## 相关命令

- `libra agent enable` / `libra agent disable`：安装 / 卸载调用 `libra hooks` 的提供商 hook 配置。
- `libra agent status`：显示捕获覆盖范围和最近的 hook 时间戳。
- `libra agent session list` / `libra agent checkpoint list`：检查由 `libra hooks` 记录的事件。
- `libra agent doctor`：诊断 hook 安装问题。

## 退出码

| 代码 | 含义 |
|------|---------|
| `0` | Codex 事件已记录、被跳过，或在 malformed/unbound ingress、在任何 Libra 仓库之外触发的回调（包括 `SessionEnd`）、辅助的 nonterminal capture failure 后已确认；其他提供商在正常跳过时也可能返回成功 |
| `1` | 保留给普通非 hook CLI 失败；malformed hook input 不使用此退出码 |
| `128` | 非 Codex hook 以 `LBR-AGENT-008` 拒绝 malformed ingress、在 Libra 仓库之外被调用（`LBR-REPO-001`）、遇到已损坏的活动仓库（`LBR-REPO-003` / `LBR-REPO-002` / `LBR-IO-001`）、遇到其他致命 capture 错误，或可信 Codex `SessionEnd` 无法持久化 durable recovery evidence、是仍保留中 terminal artifact 的重投，或来自与 session 已记录 working directory 不同的目录 |
