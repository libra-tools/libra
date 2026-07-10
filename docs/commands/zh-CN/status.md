# `libra status`

显示工作树状态。

**别名：** `st`

## 概要

```
libra status [OPTIONS] [pathspec]...
```

## 说明

`libra status` 显示工作树和暂存区状态：哪些文件已暂存到下一次提交，哪些有尚未暂存的修改，哪些未跟踪。它还报告当前分支、detached HEAD 状态和 upstream tracking 信息。

该命令计算 HEAD、索引和工作树之间的 diff，将文件分类到 staged、unstaged 和 untracked 类别。它支持多种输出格式：人类可读长格式（默认，也可用 `--long` 显式选择）、短格式（`--short`）、机器可读 porcelain 格式，以及供代理消费的结构化 JSON。可选 pathspec 会限制 staged、unstaged、unmerged、ignored 和 untracked 输出；它们使用共享 pathspec 引擎，支持 `:(top)`、`:(exclude)`、`:(icase)`、`:(literal)`、`:(glob)` magic。进行中的 merge 仍作为全局仓库状态报告，即使所选 pathspec 隐藏了所有冲突路径，`--exit-code` 也会保持 dirty，直到继续或中止该 merge。

在 merge、rebase、cherry-pick 冲突期间，未合并的 index stage 条目会按冲突输出，而不会被误报为未跟踪文件。porcelain v1/短格式使用 Git 风格 XY 码（例如 `UU conflict.txt`）；porcelain v2 输出带 stage mode 与 object id 的 `u <XY> ...` 记录。

已跟踪符号链接按链接本身比较，而不是跟随目标文件。链接目标字节或文件类型变化会显示为修改；dangling symlink 仍被视为存在的工作树路径，不会误报为删除。

## 选项

### `<pathspec>...`

将 status 输出限制为匹配路径。除 `:(top)` 外，pathspec 相对于当前工作目录解析；支持精确文件、目录前缀、默认通配符，以及 `:(top)` / `:(exclude)` / `:(icase)` / `:(literal)` / `:(glob)` magic。

### `-s, --short`

以短格式输出。每个文件以带两个字符状态码的单行显示（例如 `M ` 表示已暂存修改，` M` 表示未暂存修改，`??` 表示未跟踪）。与 `--porcelain` 冲突。

```bash
libra status -s
libra status --short
```

### `--long`

以长格式输出。这是 Libra 的默认格式，故该标志仅为 Git 对齐而接受、显式选择默认渲染；与 `--short`/`--porcelain` 互斥。

```bash
libra status --long
```

### `--porcelain [VERSION]`

以机器可读格式输出。接受可选版本参数：`v1`（默认）或 `v2`（扩展格式）。与 `--short` 冲突。

```bash
libra status --porcelain
libra status --porcelain v1
libra status --porcelain v2
```

### `--branch`（`-b`）

在 short 或 porcelain 输出中包含分支信息。第一行显示当前分支及其 tracking 关系。`-b` 是短别名，故 `libra status -sb` 与 `git status -sb` 一致。

```bash
libra status --short --branch
libra status -sb
libra status --porcelain --branch
```

### `--show-stash`

显示 stash 条目数量。仅在标准（长）输出模式中生效。

```bash
libra status --show-stash
```

### `--ignored`

在输出中包含被忽略文件。

```bash
libra status --ignored
```

### `-u, --untracked-files [<MODE>]`

控制如何显示未跟踪文件。可接受值：`normal`（默认，显示未跟踪目录但不显示其内容）、`all`（递归列出未跟踪目录内的文件）、`no`（完全隐藏未跟踪文件）。与 Git 一致：不带值即 `all`，短形式接受附加值（`-uno`、`-uall`、`-unormal`）。

```bash
libra status -uno                  # 隐藏未跟踪文件
libra status -u                    # 等同 -uall（递归未跟踪目录）
libra status --untracked-files=all
```

### `--exit-code`

如果工作树有更改，以代码 1 退出；干净时以 0 退出。适合脚本和 CI 流水线无需解析输出即可检测脏状态。

```bash
libra status --exit-code
libra status --quiet --exit-code   # 静默脏状态检查
```

## 常用命令

```bash
libra status
libra status --short
libra status --json
libra status --exit-code
```

## 人类可读输出

默认人类模式将状态摘要写到 `stdout`。

干净工作树：

```text
On branch main
nothing to commit, working tree clean
```

有更改：

```text
On branch main
Your branch is ahead of 'origin/main' by 2 commits.
  (use "libra push" to publish your local commits)

Changes to be committed:
        new file:   src/feature.rs
        modified:   src/lib.rs

Changes not staged for commit:
        modified:   README.md

Untracked files:
        notes.txt
```

Detached HEAD：

```text
HEAD detached at abc1234
nothing to commit, working tree clean
```

短格式（`--short`）：

```text
A  src/feature.rs
M  src/lib.rs
 M README.md
?? notes.txt
```

未合并冲突：

```text
UU conflict.txt
```

`--quiet` 会抑制所有 `stdout` 输出。与 `--exit-code` 组合时，它作为静默脏状态检查（脏时 exit 1，干净时 exit 0）。

## 结构化输出

`libra status` 支持全局 `--json` 和 `--machine` 标志。

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- 成功时 `stderr` 保持干净

示例：

```json
{
  "ok": true,
  "command": "status",
  "data": {
    "head": {
      "type": "branch",
      "name": "main"
    },
    "has_commits": true,
    "upstream": {
      "remote_ref": "origin/main",
      "ahead": 2,
      "behind": 0,
      "gone": false
    },
    "staged": {
      "new": ["src/feature.rs"],
      "modified": ["src/lib.rs"],
      "deleted": []
    },
    "unstaged": {
      "modified": ["README.md"],
      "deleted": []
    },
    "untracked": ["notes.txt"],
    "ignored": [],
    "is_clean": false
  }
}
```

干净工作树：

```json
{
  "ok": true,
  "command": "status",
  "data": {
    "head": {
      "type": "branch",
      "name": "main"
    },
    "has_commits": true,
    "upstream": null,
    "staged": {
      "new": [],
      "modified": [],
      "deleted": []
    },
    "unstaged": {
      "modified": [],
      "deleted": []
    },
    "untracked": [],
    "ignored": [],
    "is_clean": true
  }
}
```

Detached HEAD：

```json
{
  "ok": true,
  "command": "status",
  "data": {
    "head": {
      "type": "detached",
      "oid": "abc1234def5678..."
    },
    "has_commits": true,
    "upstream": null,
    "staged": { "new": [], "modified": [], "deleted": [] },
    "unstaged": { "modified": [], "deleted": [] },
    "untracked": [],
    "ignored": [],
    "is_clean": true
  }
}
```

### Schema 说明

- `head.type` 是 `"branch"` 或 `"detached"`
- 在分支上时，`head.name` 是分支名；detached 时，`head.oid` 是提交哈希
- 未配置 tracking 分支或 HEAD detached 时，`upstream` 为 `null`
- 远程 tracking 分支不再存在时，`upstream.gone` 为 `true`
- `gone` 为 `true` 时，`upstream.ahead` / `upstream.behind` 为 `null`
- 只有 staged、unstaged、untracked、unmerged 列表都为空且没有全局 merge
  状态时，`is_clean` 才为 `true`
- 新初始化且无提交的仓库中，`has_commits` 为 `false`
- `stash_entries`（可选，整数）：仅在传递 `--show-stash` 时存在。统计 stash 栈上的条目（匹配 `libra stash list`），可为 `0`。没有 `--show-stash` 时完全省略，因此 JSON 消费者可以区分“未查询 stash 子系统”和“已查询 stash 子系统，返回零”；也就是说，该字段的*存在*表示显式 opt-in，而不是表示存在 stashed work。

## 设计理由

### Porcelain v1 和 v2

`libra status --porcelain`（无版本）输出 Git 的经典 v1 短格式布局（每个文件 `XY <path>`）。`libra status --porcelain v2` 输出扩展 v2 行布局；对每个已跟踪文件：

```text
1 XY <sub> <mode_HEAD> <mode_index> <mode_worktree> <hash_HEAD> <hash_index> <path>
```

未跟踪条目折叠为 `? <path>`，被忽略条目折叠为 `! <path>`，匹配 Git 自身 v2 编码。实现位于 `src/command/status.rs::output_porcelain_v2`，并由 `build_porcelain_v2_data` 提供数据；后者在渲染前从索引和 HEAD tree 中取出 mode + hash 元数据。

使用 `-z` 时，porcelain v1 和 v2 记录以 NUL 终止且不带尾随换行。启用 rename 检测的 porcelain 输出在 `-z` 下不使用人类可读的 `old -> new` 箭头形式；脚本应按 NUL 切分字段。

多数消费者仍应优先使用 `--json`（或紧凑单行 JSON 的 `--machine`）：JSON 信封携带相同 staged/unstaged/untracked 分区，以及 upstream tracking 和 `stash_entries`，并且比 v2 的位置文本列更容易解析。只有在明确需要与已理解 v2 语法的工具兼容时，才使用 `--porcelain v2`。

### 显式 `--exit-code` 而不是隐式行为

Git 的 `git status` 不管仓库状态如何都退出 0；检查脏状态需要 `git diff --exit-code` 或解析 `git status --porcelain` 输出。Libra 添加显式 `--exit-code` 标志，工作树为脏时返回 exit 1。这是有意 opt-in（而非默认），以避免破坏在 `libra status` 后检查 `$?` 的脚本。与 `--quiet` 组合时，它提供无输出、仅退出码的脏状态检查，比解析文本输出更干净。

### `--show-stash` 仅在标准模式中生效

`--show-stash` 标志只影响长（标准）人类可读输出，不影响 short 或 porcelain 格式。这匹配 Git 行为，Git 中 `--show-stash` 会向长格式追加 stash 摘要行。在 JSON 输出中，stash 信息可在未来迭代中加入信封，无需单独标志，因为 JSON 消费者可以简单忽略不需要的字段。

### JSON 中增强的 upstream tracking 信息

Git 的 porcelain v1 不包含 upstream tracking 信息；porcelain v2 会添加带 ahead/behind 计数的 header 行。Libra 的 JSON 输出在配置了 tracking 分支时始终包含完整 `upstream` 对象，带有 `remote_ref`、`ahead`、`behind` 和 `gone` 字段。丰富的 upstream 数据对 AI 代理和 CI 工具至关重要，它们需要判断分支是否需要 push 或 pull，而不必额外运行 `libra log` 或 `libra branch -vv`。

## 参数对比：Libra vs Git vs jj

| 参数 / 标志 | Git | jj | Libra |
|---|---|---|---|
| 显示 status | `git status` | `jj status` / `jj st` | `libra status` |
| 长格式 | `git status --long`（默认） | N/A | `libra status --long`（默认） |
| 短格式 | `git status -s` / `--short` | N/A（始终短） | `libra status -s` / `--short` |
| Porcelain v1 | `git status --porcelain` | N/A | `libra status --porcelain` |
| Porcelain v2 | `git status --porcelain=v2` | N/A | `libra status --porcelain v2`（v1 语义） |
| 短格式中的分支信息 | `git status -sb` | 始终显示 | `libra status -sb`（`--short --branch`） |
| 显示 stash 数量 | `git status --show-stash` | N/A | `libra status --show-stash`（标准模式） |
| 显示被忽略文件 | `git status --ignored` | N/A | `libra status --ignored` |
| 未跟踪文件控制 | `git status -u<mode>` | N/A（始终显示） | `libra status -u<mode>` / `--untracked-files=<mode>` |
| 脏状态退出码 | `git diff --exit-code` | N/A | `libra status --exit-code` |
| Quiet 模式 | `git status -q` | N/A | `libra status --quiet`（全局标志） |
| 列显示 | `git status --column` | N/A | `libra status --column`（`--no-column` 撤销） |
| Ahead/behind 显示 | `git status -sb`（仅文本） | N/A | 人类 + JSON 中结构化 `upstream` 对象 |
| 查找 renames | `git status -M` | 自动 | `--find-renames` / `--renames` |
| 忽略 submodules | `git status --ignore-submodules` | N/A | N/A（无 submodules） |
| 结构化 JSON 输出 | N/A | N/A | `--json` / `--machine` |
| 错误提示 | 最少 | 最少 | 每种错误类型都有可操作提示 |

## 退出码行为

| 标志 | 干净 | 脏 |
|------|-------|-------|
| （默认） | exit 0 | exit 0 |
| `--exit-code` | exit 0 | exit 1 |

`--exit-code` 启用适合脚本的静默脏状态检查。与 `--quiet` 组合时不会产生输出，只通过退出码表示仓库状态。

## 错误处理

每个 `StatusError` 变体都会映射到显式 `StableErrorCode`。

| 场景 | 错误码 | 退出码 | 提示 |
|----------|-----------|------|------|
| 索引文件损坏 | `LBR-REPO-002` | 128 | "the index file may be corrupted" |
| 无效路径编码 | `LBR-CLI-003` | 129 | "path contains invalid characters" |
| 无法哈希文件 | `LBR-IO-001` | 128 | -- |
| 无法列出工作目录 | `LBR-IO-001` | 128 | -- |
| 找不到工作目录 | `LBR-REPO-001` | 128 | -- |
| Bare 仓库 | `LBR-REPO-003` | 128 | "this operation must be run in a work tree" |

## 兼容性说明

- `--porcelain v2` 被接受，但当前产生 v1 格式输出；使用 `--json` 获取完整结构化数据
- jj 的 `jj status` 始终使用短格式，并且不区分已暂存与未暂存更改（jj 没有暂存区）
- 通过 `--find-renames[=<n>]` 及 `--renames`/`--no-renames` 开关支持重命名检测；不暴露 Git 的短别名 `-M`
- 支持 `--column` 列对齐显示；`--no-column`（等价于 `--column=never`）经 clap `overrides_with` 撤销先前的 `--column`（最后出现者生效），status 默认非列式故单独使用为 no-op
