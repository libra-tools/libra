# `libra bisect`

使用二分搜索找出引入 bug 的提交。

## 概要

```
libra bisect start [<bad>] [--good <commit>] [--first-parent]
libra bisect bad [<rev>]
libra bisect good [<rev>]
libra bisect reset [<rev>]
libra bisect skip [<rev>]
libra bisect log
libra bisect run <cmd> [<args>...]
libra bisect view
```

## 说明

`libra bisect` 通过提交历史执行二分搜索，找出引入回归或 bug 的具体提交。用户将提交标记为 “good”（工作正常）或 “bad”（包含 bug），bisect 会系统性 checkout 两者之间的提交，直到识别出第一个坏提交。

Bisect 会话以 `bisect start` 开始，它保存当前 HEAD 和分支，以便稍后恢复。然后用户标记边界：一个“bad”提交（bug 存在）以及一个或多个“good”提交（bug 不存在）。Bisect 使用 BFS 遍历在提交图中计算 good 和 bad 之间的中点，checkout 该提交，并等待用户测试和标记。该过程重复，每次将搜索空间减半，直到找到罪魁提交。

Bisect 状态持久化在 SQLite 数据库的 `bisect_state` 表中，使会话能跨进程重启存活。该状态跟踪原始 HEAD、bad 提交、所有 good 提交、skipped 提交、当前测试提交、估计剩余步骤以及会话是否已完成。

当 bisect 识别出罪魁提交时，它会打印提交详情并将会话标记为完成。然后用户必须运行 `bisect reset` 结束会话，并将 HEAD 恢复到原始位置。

Libra 不管理子模块内容。如果 checkout bisect 候选或 reset 目标需要移除或替换已跟踪的 gitlink（模式 `160000`），且对应目录非空，命令会在更改 HEAD、索引或工作树前以 `LBR-CONFLICT-002` 拒绝。请先将目录内文件安全移到其他位置再重试；Libra 创建的空目录占位符可以正常移除。初次 `bisect start` 仍要求工作树干净，嵌套的未跟踪或忽略文件也在检查范围内，因此可能在此前置检查之前就被干净性检查拒绝，且不会创建会话。在已有会话中，`good`、`bad`、`skip` 或 `reset` 操作被此前置检查拒绝时，已保存的 bisect 状态也保持不变。

## 选项

### 子命令：`start`

开始新的 bisect 会话。保存当前 HEAD 和分支以便之后恢复。

| 参数 / 标志 | 说明 |
|-----------------|-------------|
| `<bad>` | 可选的立即标记为 bad 的提交。省略时，稍后使用 `bisect bad`。 |
| `--good` / `-g` | 可选的立即标记为 good 的提交。省略时，稍后使用 `bisect good`。 |
| `--first-parent` | 遇到合并提交时仅沿首父前进，将 bisect 限制在主线历史（合并入的侧分支不贡献可测提交）。 |

```bash
# 不带初始标记开始
libra bisect start

# 使用已知 bad（当前 HEAD）和 good 提交开始
libra bisect start HEAD --good v1.0

# 使用特定 bad 提交开始
libra bisect start abc1234 --good def5678

# 仅在首父（主线）历史上 bisect，忽略合并入的侧分支
libra bisect start HEAD --good v1.0 --first-parent
```

### 子命令：`bad`

将当前或给定提交标记为 bad（包含 bug）。如果 good 和 bad 提交都已知，bisect 会立即计算下一个中点并 checkout。

| 参数 | 说明 |
|----------|-------------|
| `<rev>` | 要标记为 bad 的提交。默认为当前 HEAD。 |

```bash
# 将当前提交标记为 bad
libra bisect bad

# 将特定提交标记为 bad
libra bisect bad abc1234
```

### 子命令：`good`

将当前或给定提交标记为 good（不包含 bug）。如果 good 和 bad 提交都已知，bisect 会计算下一个中点并 checkout。

| 参数 | 说明 |
|----------|-------------|
| `<rev>` | 要标记为 good 的提交。默认为当前 HEAD。 |

```bash
# 将当前提交标记为 good
libra bisect good

# 将特定提交标记为 good
libra bisect good def5678
```

### 子命令：`reset`

结束 bisect 会话，并将 HEAD 恢复到原始位置（`bisect start` 前 checkout 的分支或提交）。如果提供 `<rev>`，HEAD 会恢复到该提交，而不是原始位置。

| 参数 | 说明 |
|----------|-------------|
| `<rev>` | 可选的重置目标提交，替代原始 HEAD。 |

```bash
# 结束 bisect 并恢复原始 HEAD
libra bisect reset

# 结束 bisect 并转到特定提交
libra bisect reset main
```

### 子命令：`skip`

跳过当前提交并移动到下一个候选。适合当前提交无法测试的情况（例如无法编译）。被跳过提交会从后续中点计算中排除。如果跳过太多提交，bisect 可能无法精确缩小罪魁范围。

| 参数 | 说明 |
|----------|-------------|
| `<rev>` | 要跳过的提交。默认为当前 HEAD。 |

```bash
# 跳过当前提交
libra bisect skip

# 跳过特定提交
libra bisect skip abc1234
```

### 子命令：`log`

显示 bisect 日志，列出当前会话期间做出的所有 good、bad 和 skipped 标记。

```bash
libra bisect log
```

### 子命令：`run`

在每个 bisect 步骤运行命令，并根据退出码自动派发 `good` / `bad` / `skip`。该命令会在每个候选提交处调用，bisect 会推进到收敛（或候选耗尽）。

`bisect run` 需要一个已同时具备 bad 边界和至少一个 good 边界的活动会话，因此请用 `libra bisect start <bad> --good <good>` 开始，或在调用自动化前手动标记两个边界。

| 参数 | 说明 |
|----------|-------------|
| `<cmd> [<args>...]` | 要执行的命令。第一个 token 是可执行文件；后续内容原样转发。允许并透传 `--`（例如 `libra bisect run cargo test -- --ignored`）。 |

退出码语义（与标准 `git bisect run` 对齐）：

| 退出码 | 标记 / 动作 |
|-----------|---------------|
| `0` | `good` |
| `1`-`124`, `126`-`127` | `bad` |
| `125` | `skip`（无法测试此提交） |
| `128` 及以上 | 以致命 `BISECT_RUN_FAILED` 错误终止 bisect |

被信号杀死也会以致命错误终止 bisect。

```bash
# 用 cargo test 驱动 bisect
libra bisect run cargo test --test foo

# 将标志透传给底层测试命令
libra bisect run cargo test -- --ignored

# 使用自定义 shell 脚本
libra bisect run bash -c 'cargo build && ./target/debug/repro'
```

### 子命令：`view`（别名：`visualize`）

显示当前 bisect 状态：good / bad 边界、当前 HEAD、剩余候选和任何 skipped commits。`visualize` 是 `view` 的别名：git 的 `bisect visualize` 启动 GUI（gitk）或分页器，而 Libra 终端原生，打印相同的文本状态摘要。

```bash
libra bisect view
libra bisect visualize   # view 的别名
```

如果没有进行中的 bisect，返回致命错误（`NOT_IN_BISECT`）。

## JSON / Machine 输出

`libra bisect` 对所有子命令支持全局 `--json` 和 `--machine`。两种模式在成功时输出单个 `bisect` 命令信封；`--machine` 使用同一信封作为紧凑单行，并抑制人类进度。

公共字段：

| 字段 | 说明 |
|-------|-------------|
| `action` | `start`、`mark`、`skip`、`reset`、`log`、`view`、`run` 之一。 |
| `status` | 状态转换时存在：`started`、`waiting_for_good`、`waiting_for_bad`、`testing`、`converged` 或 `all_skipped`。 |
| `bad` / `good` / `current` | 当前 bisect 边界和候选的完整提交 ID。 |
| `remaining` / `steps` | 已知时的候选数量和估计剩余搜索步骤。 |
| `first_bad` | 会话收敛时的完整提交 ID。 |

示例：

```json
{
  "ok": true,
  "command": "bisect",
  "data": {
    "action": "view",
    "head": "901abcd...",
    "good": ["abc1234..."],
    "bad": "def5678...",
    "current": "901abcd...",
    "remaining": 1,
    "completed": false
  }
}
```

## 常用命令

```bash
# 开始 bisect 会话
libra bisect start

# 将当前版本标记为坏
libra bisect bad

# 标记已知好的版本
libra bisect good v1.0

# 测试 checkout 的提交，然后标记它
# （在这里运行你的测试）
libra bisect good    # 如果测试通过
libra bisect bad     # 如果测试失败

# 跳过无法测试的提交
libra bisect skip

# 查看 bisect 日志
libra bisect log

# 结束会话
libra bisect reset

# 使用已知边界快速开始
libra bisect start HEAD --good abc1234
```

## 人类可读输出

**`bisect start`**：

```text
Bisect session started.
```

**`bisect start <bad> --good <good>`**（带两个标记）：

```text
Bisect session started.
Bisecting: N revisions left to test (roughly M steps)
[abc1234] commit message here
```

**`bisect bad`** / **`bisect good`**（缩小范围）：

```text
Bisecting: N revisions left to test (roughly M steps)
[abc1234] commit message here
```

**`bisect bad`** / **`bisect good`**（找到罪魁）：

```text
abc1234def5678901234567890abcdef12345678 is the first bad commit
commit abc1234def5678901234567890abcdef12345678
Author: Alice <alice@example.com>
Date:   Mon Jan 15 10:30:00 2024 -0800

    introduce the bug here
```

**`bisect skip`**：

```text
Bisecting: N revisions left to test (roughly M steps)
[def5678] next candidate commit message
```

**`bisect log`**：

```text
# bad: [abc1234] broken commit message
# good: [def5678] working commit message
# skip: [ghi9012] untestable commit
```

**`bisect reset`**：

```text
Bisect session reset. HEAD restored to original position.
```

**`bisect run`**（收敛）：

```text
Bisecting: 5 candidates remaining
Running cargo test --test foo at abc1234... PASS (good)
Bisecting: 2 candidates remaining
Running cargo test --test foo at def5678... FAIL (bad)
Bisecting: 1 candidate remaining
Running cargo test --test foo at 901abcd... FAIL (bad)
Converged: first bad commit is 901abcd
3 steps, 0 skipped
```

**`bisect view`**：

```text
Bisecting between abc1234 (good) and def5678 (bad)
HEAD: 901abcd
Remaining: 1 candidate
Skipped: (none)
```

## 设计理由

### 为什么 bisect 不隐藏？

尽管在一些早期设计中被列为隐藏命令，`libra bisect` 是完全可见的子命令。用于回归定位的二分搜索是基础调试工作流，对人类用户和 AI 代理都有益。隐藏它会降低可发现性，没有明显收益。该命令稳定，并遵循其他 Libra 命令的相同模式。

### `bisect run` 如何处理退出码？

`bisect run` 镜像标准 `git bisect run`，以保持 AI 代理和 CI 集成直接。退出码契约为：

- `0` -> 标记 `good` 并推进。
- `1`-`124` 或 `126`-`127` -> 标记 `bad` 并推进。
- `125` -> `skip`（提交无法测试，例如无法编译）并推进。
- `128` 及以上 -> 致命：终止 bisect 并暴露 `BISECT_RUN_FAILED`，使调用方可以响应。被信号杀死（例如 SIGINT）同样处理。

完整命令行会原样透传，因此 `libra bisect run cargo test -- --ignored` 会将 `--ignored` 转发给测试命令，而不是解析为 `bisect` 标志。这通过 `cmd` 参数上的 `trailing_var_arg` + `allow_hyphen_values` 实现。

对于在进程内评估结果、并偏好显式控制每一步的 AI 代理，手动标记（`bisect good` / `bisect bad`）仍是推荐路径。

### First-parent bisect

默认情况下 Libra 的 bisect 使用 BFS 遍历完整提交图，对所有拓扑都正确。`bisect start --first-parent` 镜像 `git bisect --first-parent`：遇到合并提交时只沿首父前进，使合并入的侧分支不贡献可测提交，从而在含大量 merge 的工作流中把搜索收窄到主线。该标志记录在 bisect 会话状态中，后续 `good`/`bad`/`skip` 步骤会持续保持在首父历史上，直到 `bisect reset`。

### 为什么 SQLite 状态持久化？

Bisect 会话可能跨越数小时或数天，因为用户要测试每个候选。将状态存储在 SQLite `bisect_state` 表中，确保会话能跨进程重启、编辑器关闭和系统重启存活。Git 使用 `.git/BISECT_*` 扁平文件实现相同持久性，但结构更少。SQLite 提供事务写入和以编程方式查询状态的能力，这对 AI 代理集成很有价值。

### 为什么 `reset` 接受可选 `<rev>`？

有时用户想结束 bisect 会话，但转到与开始位置不同的提交。例如，找到罪魁后，他们可能想 reset 到引入 bug 前的提交。可选 `<rev>` 参数提供这种灵活性，无需在 reset 后单独 checkout。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 开始会话 | `bisect start [<bad>] [--good <commit>]` | `bisect start [<bad> [<good>...]]` | N/A |
| 标记 bad | `bisect bad [<rev>]` | `bisect bad [<rev>]` | N/A |
| 标记 good | `bisect good [<rev>]` | `bisect good [<rev>]` | N/A |
| Reset | `bisect reset [<rev>]` | `bisect reset [<commit>]` | N/A |
| Skip | `bisect skip [<rev>]` | `bisect skip [<rev>...]` | N/A |
| 显示 log | `bisect log` | `bisect log` | N/A |
| 自动 run | `bisect run <cmd> [<args>...]` | `bisect run <script>` | N/A |
| 显示当前状态 | `bisect view` / `bisect visualize` | `bisect visualize`（GUI / log） | N/A |
| 自定义术语 | 不支持（已延后，见 compatibility/declined.md D7） | `bisect terms` / `--term-old` / `--term-new` | N/A |
| 重放会话 | 不支持（已延后，见 compatibility/declined.md D6） | `bisect replay <logfile>` | N/A |
| 可视化（GUI） | `bisect visualize`（`view` 的别名；打印文本状态，无 GUI） | `bisect visualize` | N/A |
| 仅 first-parent | `bisect start --first-parent` | `--first-parent` | N/A |
| 多个 good commits | 通过重复 `bisect good` | `start` 的位置参数 | N/A |
| 状态存储 | SQLite（`bisect_state` 表） | 扁平文件（`.git/BISECT_*`） | N/A |

注意：jj 没有 bisect 命令。需要二分调试的 jj 用户必须使用外部工具或手动 checkout commits。这是 jj 功能集中的空白，Libra 对此进行了补足。

## 错误处理

| 代码 | 条件 |
|------|-----------|
| `LBR-REPO-001` | 不是 libra 仓库 |
| `LBR-REPO-003` | 仓库中没有提交 |
| `LBR-REPO-003` | `bisect run` 在 good/bad 边界选择候选前调用 |
| `LBR-CLI-002` | Bisect 会话已在进行中（用于 `start`） |
| `LBR-CLI-002` | 没有进行中的 bisect 会话（用于 `bad`、`good`、`skip`、`log`） |
| `LBR-CLI-003` | 找不到提交（无效 rev 参数） |
| `LBR-CLI-003` | Bad 提交是 good 提交的祖先（无效范围） |
| `LBR-CONFLICT-001` | 未提交更改会被 checkout 覆盖 |
| `LBR-CONFLICT-002` | 需要移除或替换已跟踪 gitlink 的非空目录（退出码 128）。请先将目录内文件安全移到其他位置再重试；Libra 创建的空目录占位符可以移除。 |
| `LBR-IO-001` | 无法从数据库读取 bisect 状态 |
| `LBR-IO-002` | 无法将 bisect 状态保存到数据库 |
| `LBR-IO-002` | 无法创建 bisect_state 表 |
| `LBR-BISECT-001` | 在活动 bisect 会话外调用 `bisect view` 或 `bisect run`（`NOT_IN_BISECT`） |
| `LBR-BISECT-002` | `bisect run` 命令退出码 >= 128 或被信号杀死（`BISECT_RUN_FAILED`） |
| `LBR-BISECT-003` | `bisect run` 因没有候选提交而无法推进（`BISECT_NO_CANDIDATES`） |
