# `libra hooks`

由外部 AI 代理 hook 配置调用的内部入口点，用于将生命周期事件（会话开始、提示提交、工具使用、模型更新、压缩、停止、会话结束）捕获到 libra 会话存储中。操作者几乎不会直接输入 `libra hooks ...`，由 `libra agent enable` 安装的 hook 配置会引用这些子命令。

## 概要

```
libra hooks claude   {session-start|prompt|tool-use|model-update|compaction|stop|session-end}
libra hooks codex    {session-start|prompt|tool-use|model-update|compaction|stop|session-end|subagent-start|subagent-end}
libra hooks gemini   <event>   # 拒绝并给出提示：gemini 已是 uninstall-only（AG-17）
```

## 说明

`libra hooks` 是由 Claude Code / Codex hook 配置调用的**隐藏**（clap 中 `hide = true`）兼容性表面。每次调用都会从 stdin 读取单个 hook 事件 payload（JSON），根据提供商特定 schema 验证它，并将脱敏后的投影记录到外部代理捕获存储（`agent_session` / `agent_checkpoint` + `refs/libra/traces`）。

该命令被隐藏，因为：

- 它不是面向用户的 CLI 契约的一部分；它必须继续能被 hook 配置调用，而这些配置格式由上游提供商（Claude Code / Codex）拥有，不由 Libra 拥有。如果将其视为公共表面，就需要在 Libra 侧冻结 JSON payload schema，但这不可能，因为提供商可在任意版本中更改 payload。
- 它产生的事件会被 `libra agent session list`、`libra agent checkpoint *` 和 `libra agent doctor` 读取。用于检查已捕获会话的公共表面是 `agent` 子命令（[agent.md](agent.md)），不是 `hooks`。

`libra hooks claude <verb>` 是 `libra agent enable --agent claude-code` 写入项目 `.claude/settings.json` 的稳定调用面；`libra hooks codex <verb>`（AG-19）是 `libra agent enable --agent codex` 写入 `$CODEX_HOME/hooks.json` 的稳定调用面。两者都记录到 AgentTraces 捕获存储（`refs/libra/traces`）；claude 历史上路由到 `refs/libra/intent` 写入器的行为已由 Task A6.5 本地采集 smoke 收敛（该 smoke 要求已安装 hook 的采集出现在 `libra agent session/checkpoint list` 中）。Codex 额外转发原生子代理边界（`subagent-start` / `subagent-end`）。

`libra hooks gemini <verb>` 不再摄入：gemini 已是 uninstall-only（AG-17），降级前安装的过时 hook 配置会得到指向 `libra agent remove gemini` 的可操作错误，而不是静默捕获数据。

要启用捕获，对 supported roster 中的 agent 运行 `libra agent enable --agent <name>`；这会安装提供商 hook 配置。要禁用捕获，运行 `libra agent disable --agent <name>`。

## 提供商和事件

Claude Code 与 Codex 暴露相同的七个 Claude-Code 风格生命周期事件（Codex 额外转发 `subagent-start` / `subagent-end`）：

| 事件 | 触发条件 |
|-------|---------|
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

每个事件都会从 stdin 读取其提供商特定 JSON payload，运行脱敏流水线（secrets / tokens / 文件内容 >256 KiB），并将一条 `AgentTraceEvent` JSONL 记录追加到活动会话存储中。除非 payload 解析失败，否则 hook 返回退出码 0；提供商 hook 绝不能阻塞在 Libra 侧处理上。

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
    "SessionStart": [{"command": "libra hooks claude session-start"}],
    "UserPromptSubmit": [{"command": "libra hooks claude prompt"}],
    "PreToolUse": [{"command": "libra hooks claude tool-use"}],
    "PostToolUse": [{"command": "libra hooks claude tool-use"}],
    "Stop": [{"command": "libra hooks claude stop"}],
    "SessionEnd": [{"command": "libra hooks claude session-end"}]
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
| `0` | 事件已记录（或因捕获被禁用 / 会话未知而静默跳过） |
| `1` | stdin payload 未通过 schema 验证；hook 调用方可能显示警告，但提供商 hook 流程会将其视为非致命 |
| `128` | 处理任何 payload 前发生致命初始化错误 |
