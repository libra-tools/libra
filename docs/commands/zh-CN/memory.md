# `libra memory`

检查并重建 Libra 确定性的 Agent 研发历程记忆投影。

## 概要

```bash
libra memory status
libra memory list
libra memory show <episode-id>
libra memory rebuild
libra memory status --allow-stale
```

## 说明

`libra memory` 读取并重建仓库级、零权威的 Agent 研发事实投影。每条
`memory_episode` 都是仓库已提交事实（commit/change、agent session、agent
run、bridge operation）的纯函数，因此投影不持有任何权威状态，并且随时可以
通过 `libra memory rebuild` 重新派生（GC-DM-01）。

所有子命令都是**只读**的——它们绝不写 `operation` 行。

**新鲜度（fail-closed，ADR-DM-10）：** 投影只在其最后一次 `rebuild` 时刻是
最新的。新的 commit、session 或 run 会把底层事实推进到已存指纹之外，使投影
变陈旧。除非你传入 `--allow-stale`，否则 `status`、`list`、`show` 会拒绝从
陈旧投影作答并退出 `128`、返回 `LBR-MEMORY-001`。传入 `--allow-stale` 时，
JSON 信封会标记 `"stale": true`，人类可读输出会打印横幅；不传时，投影会在
下一次 `libra memory rebuild` 时刷新。

## 子命令

| 子命令 | 说明 |
|--------|------|
| `status` | 报告投影新鲜度 / horizon 状态与持久化计数器 |
| `list` | 列出当前窗口内的已派生记忆 Episode |
| `show` | 渲染单条已派生记忆 Episode |
| `rebuild` | 从当前事实重新派生零权威投影 |

## 选项

| 标志 | 子命令 | 说明 |
|------|--------|------|
| `--allow-stale` | `status`、`list`、`show` | 即使投影陈旧也作答（ADR-DM-10）；在 `--json` 中标记 `"stale": true`，在人类可读输出中打印横幅 |
| `--reveal` | `show` | 显示完整 Episode 正文（默认只显示占位提示） |
| `--json` | 全部 | 结构化 JSON 信封（全局标志） |

## 人类可读输出

`status`（新鲜时）逐个打印各项持久化值。

```text
schema_version:   1
selector_version: 1
rules_version:    1
horizon_truncated: false
revoked_count:     0
aged_out_count:    0
rebuilt_at:        1791541568
```

投影陈旧且传入 `--allow-stale` 时，会先打印横幅：

```text
memory projection is stale
schema_version:   1
...
```

`list` 每条 Episode 打印一行制表符分隔：`episode_id`、`source_kind` 与
`title`。

`show` 打印该 Episode 的身份、`outcome` 与 `title`。`--reveal` 包含完整
正文；不传时正文被省略。

## JSON 输出

`--json` 使用命令特定的信封：

- `memory`（status / list / show / rebuild）

每个信封携带 `data` 对象。`status` 使用冻结的读取契约（DM-11）：

```json
{
  "ok": true,
  "command": "memory",
  "data": {
    "schema_version": 1,
    "stale": false,
    "selector_version": 1,
    "rules_version": 1,
    "horizon_truncated": false,
    "revoked_count": 0,
    "aged_out_count": 0,
    "rebuilt_at": 1791541568
  }
}
```

`list` 返回 `data.episodes` 数组；`show` 返回单个 Episode 对象；`rebuild`
返回重建报告（`projected`、`horizon_truncated`、`revoked_count`、
`aged_out_count`）。

## 示例

```bash
# 显示投影新鲜度 / horizon 状态
libra memory status

# 列出窗口内的已派生记忆 Episode
libra memory list

# 显示单条已派生记忆 Episode
libra memory show <episode-id>

# 重建零权威投影（GC-DM-01）
libra memory rebuild

# 从陈旧投影作答（默认 fail-closed）
libra memory status --allow-stale

# 面向 agent 的结构化 JSON 信封
libra --json memory status
```

`libra memory --help` 会渲染同样的横幅，使文档与 CLI 表面保持一致（跨切面
`--help` EXAMPLES 推出，见 `docs/development/commands/_general.md` 条目 B）。

## 备注

- 该命令需要 Libra 仓库，因为投影位于 `.libra/libra.db`，且需要仓库 id 来
  限定 Episode 范围。
- `memory rebuild` 在分支 tip 的首父链中读取最多 `memory.horizon`
  （默认 `5000`）个提交；`revoked_count` 与 `aged_out_count` 是这次对账中
  被删除的行数（GC-DM-01）。
- 投影的 `--json` 绝不泄漏主机绝对路径（ER-11）；代码路径以仓库相对字节串
  形式输出。
