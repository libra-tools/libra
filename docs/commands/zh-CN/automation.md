# `libra automation`

管理当前仓库的 AI 自动化规则。

## 概要

```bash
libra automation list
libra automation run [--rule <id>] [--now <rfc3339>] [--live]
libra automation history [--limit <n>]
```

## 说明

`libra automation` 读取仓库自动化配置，评估 cron 风格规则，并将执行历史记录到仓库数据库中。默认情况下，`run` 使用 dry-run 执行器，因此 shell 动作只会被计划和记录，不会生成外部命令进程。只有在确实应该运行已配置动作时，才传递 `--live`。

**Linked worktree**：在统一的 Code/Agent 配置 resolver 落地前，所有 `libra automation` 子命令在 linked worktree 中一律 fail-closed——`automations.toml` 会被从 linked worktree 的本地 gitdir 读取，而那里没有任何仓库配置，命令将静默看到空规则集。VCS 事件 dispatch（commit/add/branch/switch/push 触发）在 linked worktree 中同样被禁用，且每条命令输出一次 warning 而非静默跳过。请改在 main worktree 运行。

## 子命令

| 子命令 | 说明 |
|------------|-------------|
| `list` | 验证并列出已配置的自动化规则 |
| `run` | 运行到期规则，或用 `--rule <id>` 运行一个具名规则 |
| `history` | 显示最近的自动化运行历史 |

## 选项

| 标志 | 子命令 | 说明 |
|------|------------|-------------|
| `--rule <id>` | `run` | 运行单个规则，不管其触发器是否到期 |
| `--now <rfc3339>` | `run` | 使用模拟的当前时间评估到期规则 |
| `--live` | `run` | 执行通过安全预检的 shell 动作 |
| `--limit <n>` | `history` | 要显示的历史行数；默认为 `20` |

## 人类可读输出

`list` 为每条规则打印一行制表符分隔的记录：

```text
<rule-id>	<trigger-kind>	<action-kind>
```

`run` 为每个结果打印一行制表符分隔的记录：

```text
<rule-id>	<status>	<message>
```

`history` 包含完成时间戳：

```text
<finished-at>	<rule-id>	<status>	<message>
```

## JSON 输出

`--json` 使用命令特定信封：

- `automation.list`
- `automation.run`
- `automation.history`

示例：

```json
{
  "ok": true,
  "command": "automation.run",
  "data": {
    "results": []
  }
}
```

## 示例

```bash
# 验证 .libra/automation.toml 中的规则并列出它们
libra automation list

# 计划到期规则但不运行它们（dry-run 是 run 的默认行为）
libra automation run

# 按 id 运行单个规则，不管其 cron 触发器是否到期
libra automation run --rule my-rule

# 评估 cron 触发器时模拟一个特定当前时间
libra automation run --now 2026-05-23T12:00:00Z

# 实际生成通过安全预检的 shell 动作
libra automation run --live

# 显示最近 50 条自动化历史记录
libra automation history --limit 50

# 面向代理的结构化 JSON 信封
libra automation --json list
libra automation --json run
```

`libra automation --help` 会渲染同一横幅，因此文档和 CLI 表面保持同步（跨命令 `--help` EXAMPLES 推出，见 `docs/development/commands/_general.md` 条目 B）。

## 说明

- `run` 和 `history` 需要 Libra 仓库，因为历史存储在 `.libra/libra.db` 中。
- 配置验证错误会在规则运行前暴露。
- `--live` 是有意显式的；dry-run 仍是默认值。
