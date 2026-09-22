# `libra rev-list`

列出从某个修订可达的提交对象。

## 概要

```bash
libra rev-list [OPTIONS] [SPEC]
```

## 说明

`libra rev-list` 会将修订输入解析为提交，遍历可达历史，应用可选的父提交数量过滤和计数/限制过滤，并按从新到旧的顺序打印提交 ID。省略 `<SPEC>` 时，命令默认为 `HEAD`。输出格式可以通过 `--parents` 增加父提交 ID，也可以通过 `--timestamp` 增加提交者时间戳。`--reverse` 将输出翻转为从旧到新（在提交限制之后应用）。

浅克隆中，`.libra/shallow` 列出的提交会被当作根，因此 `--count HEAD` 只统计已获取的历史。损坏的 `.libra/shallow` 会 fail-closed（`LBR-REPO-002`）。`--parents` 仍打印对象里记录的父提交 ID。

## 选项

| 标志 | 说明 |
|------|-------------|
| `-n <N>`, `--max-count <N>` | 排序后最多输出 `N` 个提交。 |
| `--skip <N>` | 输出或计数前跳过前 `N` 个提交。 |
| `--reverse` | 反转所选提交的输出顺序。先应用提交限制（`--max-count`/`--skip`），再反转结果。 |
| `--all` | 以所有 ref（分支、远程跟踪分支和标签）和当前 HEAD 为遍历起点，叠加于任何显式 `<SPEC>`。 |
| `--date-order` | 按提交者日期顺序（最新优先）显示提交。作为 Libra 既有默认顺序的 no-op 接受。与 Git 不同，Libra 不额外施加 topo「父提交不先于其子提交」约束（仅在提交者日期发生偏斜时可观察到差异）。 |
| `--count` | 只打印过滤后的提交数量。 |
| `--merges` | 只打印至少有两个父提交的 merge commit。 |
| `--no-merges` | 排除至少有两个父提交的 merge commit。 |
| `--min-parents <N>` | 只打印至少有 `N` 个父提交的提交。 |
| `--max-parents <N>` | 只打印最多有 `N` 个父提交的提交。 |
| `--parents` | 在每个提交后打印父提交 ID。 |
| `--timestamp` | 在每个提交前打印提交者时间戳，字段顺序与 Git 的 `timestamp commit [parents...]` 一致。 |
| `--boundary` | 额外打印处于前沿的边界提交——被列出提交的、自身未被列出的父提交（被 `^spec`/范围起点排除，或在 `--max-count`/`--skip` 切割之外），每个以 `-` 前缀。通常置于列出提交之后；在 `--reverse` 下整个输出流被反转，因此边界提交会置于最前。边界提交经同一渲染路径输出，因此 `--parents`/`--children`/`--timestamp` 元数据会保留（两个与 Git 一致的合并细节：`--first-parent --parents` 下未被遍历的第二父边界以裸 `-id` 输出；`--children` 下边界提交的子提交从输出集派生）。`--count` 会把边界提交计入总数。 |
| `--objects` | 在提交行之后，额外列出从打印的提交可达的、去重后的 tree 与 blob 对象，每个为 `<oid> <path>`。每个提交的根 tree 输出为 `<oid> `（尾随空格、空路径）；子 tree 与 blob 带其工作区相对路径。对象按 pre-order 遍历（先 tree 本身，再按 tree 顺序遍历每个条目，遇到子 tree 立即递归），与 `git rev-list --objects` 一致。对象行始终跟在提交/边界流之后，不受 `--reverse` 重排。从**被排除**提交（如 `A..B` 的 `A` 侧、或 `^rev`）可达的对象被视为 uninteresting 并省略，故范围只列出包含侧新增的对象。`-- <pathspec>` 限定会把遍历裁剪到通往 pathspec 路径上的 tree 以及其下全部内容（根 tree 始终保留）。gitlink/子模块（`160000`）条目被跳过。包含侧的缺失/损坏 tree 是硬错误（`LBR-REPO-002`），不会静默截断列表。`--count --objects` 把对象计入总数，但不能与 `--left-right`/`--cherry-mark`/`--cherry` 同用（对象无 side）。 |
| `--objects-edge` | 隐含 `--objects`，并额外以 `-` 前缀打印被排除的边界提交（前沿），供 pack 构建器作为 edge 处理。 |
| `--objects-edge-aggressive` | 作为 `--objects-edge` 的别名接受。Git 的 aggressive 变体会标记更多 edge 提交以构建更薄的 pack；Libra 输出相同的边界前沿（有意收窄）。 |
| `<SPEC>` | 要从中枚举的修订。默认为 `HEAD`。 |

## 常用命令

```bash
libra rev-list
libra rev-list HEAD
libra rev-list --count HEAD
libra rev-list -n 5 HEAD
libra rev-list --reverse HEAD
libra rev-list --all
libra rev-list --date-order HEAD
libra rev-list --boundary main..feature
libra rev-list --skip 5 --max-count 10 HEAD
libra rev-list --merges HEAD
libra rev-list --no-merges HEAD
libra rev-list --min-parents 1 --max-parents 1 HEAD
libra rev-list --max-parents 0 HEAD
libra rev-list --parents HEAD
libra rev-list --timestamp --parents HEAD
libra rev-list HEAD~1
libra rev-list refs/remotes/origin/main
libra rev-list --objects HEAD
libra rev-list --objects-edge main..feature
libra --json rev-list HEAD
```

## 人类可读输出

默认输出为每行一个提交 ID。父提交数量过滤会在 `--skip`、`--max-count` 和 `--count` 前应用。使用 `--parents` 时，每行格式为 `commit parent...`；使用 `--timestamp` 时，每行格式为 `timestamp commit`；组合使用时为 `timestamp commit parent...`。使用 `--count` 时，输出仍为单行十进制数量，并忽略这些输出格式标志。

```text
abc1234def5678901234567890abcdef12345678
def5678901234567890abcdef12345678abc1234
```

```text
1715788800 abc1234def5678901234567890abcdef12345678 def5678901234567890abcdef12345678abc1234
1715702400 def5678901234567890abcdef12345678abc1234
```

## 结构化输出

使用 `--boundary` 时，前沿提交位于单独的 `boundary[]` 数组（为空时省略）。使用 `--objects`（或 `--objects-edge[-aggressive]`）时，可达的 tree/blob 对象位于单独的 `objects[]` 数组（`{ "oid", "path" }`，为空时省略），去重且顺序与人类输出一致；根 tree 的 `"path"` 为空。`--objects-edge[-aggressive]` 还会以 `-` 前缀填充 `boundary[]` 的 edge 提交。

```json
{
  "ok": true,
  "command": "rev-list",
  "data": {
    "input": "HEAD",
    "commits": [
      "abc1234def5678901234567890abcdef12345678",
      "def5678901234567890abcdef12345678abc1234"
    ],
    "total": 2,
    "count_only": false,
    "parents": false,
    "timestamp": false,
    "merges": false,
    "no_merges": false,
    "min_parents": null,
    "max_parents": null,
    "max_count": null,
    "skip": 0
  }
}
```

当存在 `--parents` 或 `--timestamp` 时，`commits[]` 仍保留纯提交 ID 列表以兼容既有消费者，`entries[]` 提供人类输出所用的可选元数据。

```json
{
  "ok": true,
  "command": "rev-list",
  "data": {
    "input": "HEAD",
    "commits": [
      "abc1234def5678901234567890abcdef12345678"
    ],
    "entries": [
      {
        "commit": "abc1234def5678901234567890abcdef12345678",
        "parents": [
          "def5678901234567890abcdef12345678abc1234"
        ],
        "timestamp": 1715788800
      }
    ],
    "total": 1,
    "count_only": false,
    "parents": true,
    "timestamp": true,
    "merges": false,
    "no_merges": false,
    "min_parents": null,
    "max_parents": null,
    "max_count": 1,
    "skip": 0
  }
}
```

## 参数对比：Libra vs Git vs jj

| 功能 | Libra | Git | jj |
|---------|-------|-----|----|
| 默认目标 | `HEAD` | `HEAD` | 当前修订 |
| 修订导航 | `HEAD~1`、标签、远程引用 | 相同 | revsets |
| 计数与限制 | `--count`、`-n` / `--max-count`、`--skip` | 相同 | revset 函数 |
| 父提交数量过滤 | `--merges`、`--no-merges`、`--min-parents`、`--max-parents` | 相同 | revset 谓词 |
| 父提交输出 | `--parents` | 相同 | revset/template 输出 |
| 时间戳输出 | `--timestamp` | 相同 | template 输出 |
| JSON 输出 | `--json` | 无 | 无 |
| 排序 | 最新优先 | 可达性顺序 | 取决于 revset |

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 无效目标引用 | `LBR-CLI-003` | 129 |
| 无法读取仓库元数据 | `LBR-IO-001` | 128 |
| 存储的引用/对象损坏 | `LBR-REPO-002` | 128 |
