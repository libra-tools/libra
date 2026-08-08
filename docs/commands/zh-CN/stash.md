# `libra stash`

把脏工作目录中的更改临时贮藏起来。

## 概要

```
libra stash push [-m <message>] [-u | -a] [-k | --keep-index] [-- <pathspec>...]
libra stash pop [<stash>]
libra stash list
libra stash apply [<stash>]
libra stash drop [<stash>]
libra stash show [<stash>] [-p | --patch] [--name-only | --name-status]
libra stash branch <branch> [<stash>]
libra stash clear [--force]
```

## 说明

`libra stash` 将本地修改保存为新的 stash 条目，并把工作目录还原到与 HEAD 一致。默认情况下，`stash push` 只记录已跟踪文件的索引/工作区修改，并保留未跟踪文件。使用 `-u` / `--include-untracked` 可以包含可见未跟踪文件；使用 `-a` / `--all` 还会包含被忽略文件。传入 `-- <pathspec>...`（文件或目录路径，`.` 表示整棵树）可只 stash 这些路径的修改，工作树中其余改动原样保留（`-u`/`-a`/`-k` 不能与 pathspec 同用，否则 `LBR-CLI-002`）。之后可以用 `libra stash pop` 或 `libra stash apply` 恢复这些修改——恢复时是三方合并到当前工作树（而非 HEAD），因此期间对无关文件所做的未提交改动（包括 pathspec push 留下的那些路径）都会被保留。如果在干净工作树上运行 `stash push`，且没有请求纳入的未跟踪文件，命令会作为无操作成功退出，并报告没有可保存的本地更改。

Stash 条目以特殊结构的提交对象存储在 `.libra/refs/stash` 下，并通过一个扁平文件列表跟踪 stash 栈。每个 stash 都捕获创建时的索引状态和工作树状态。

**Worktree（自 W2 起）**：stash *栈* 有意在所有 worktree 间共享——一个 worktree 推入的条目可在任何其他 worktree 中 list/apply/pop；而 `push`/`apply`/`pop` 只快照与修改你运行命令的那个 worktree。栈突变经锁串行化，`pop`/`stash branch` 只删除自己刚 apply 的那个条目（若栈被并发修改则保留条目并报告，绝不误删他人条目，也不回滚已成功的 apply）。

## 选项

### 子命令

#### `push`

将本地修改保存到新的 stash，并清理工作目录。

| 选项 | 短参数 | 长参数 | 说明 |
|------|--------|--------|------|
| Message | `-m` | `--message` | stash 条目的可选描述消息。省略时会生成默认的 "WIP on `<branch>`: `<short-hash>` ..." 消息。 |
| Include untracked | `-u` | `--include-untracked` | 将可见未跟踪文件纳入 stash，并从工作区删除它们。被忽略文件会保留。 |
| No include untracked | | `--no-include-untracked` | 不纳入未跟踪文件（默认），撤销先前的 `-u`/`--include-untracked`（最后出现者生效）。未跟踪文件默认排除，故单独使用时为 no-op。 |
| Include all | `-a` | `--all` | 将可见未跟踪文件和被忽略文件都纳入 stash，并从工作区删除它们。 |
| Keep index | `-k` | `--keep-index` | 保留已暂存内容，并把工作区恢复到索引记录的内容，只移除未暂存 delta。 |

```bash
# 使用默认消息保存
libra stash push

# 使用描述性消息保存
libra stash push -m "work in progress on feature X"

# 纳入可见未跟踪文件
libra stash push -u

# 同时纳入被忽略文件
libra stash push -a

# 只贮藏未暂存 delta，保留已暂存内容
libra stash push --keep-index
```

#### `pop`

应用栈顶 stash 条目，并将其从 stash 列表中移除。等价于先 `apply` 再 `drop`。

| 参数 | 说明 |
|------|------|
| `<stash>` | Stash 引用，例如 `stash@{1}`。默认是 `stash@{0}`（最近的 stash）。 |

```bash
# 弹出最新 stash
libra stash pop

# 弹出指定 stash
libra stash pop stash@{2}
```

#### `list`

列出所有 stash 条目及其索引、消息和 stash ID。

```bash
libra stash list
```

#### `apply`

应用一个 stash 条目，但不从 stash 列表中移除它。适合需要把同一个 stash 应用到多个分支的场景。

| 参数 | 说明 |
|------|------|
| `<stash>` | Stash 引用，例如 `stash@{1}`。默认是 `stash@{0}`。 |

```bash
libra stash apply
libra stash apply stash@{1}
```

#### `drop`

从 stash 列表中移除单个 stash 条目，但不应用它。

| 参数 | 说明 |
|------|------|
| `<stash>` | Stash 引用，例如 `stash@{1}`。默认是 `stash@{0}`。 |

```bash
libra stash drop
libra stash drop stash@{1}
```

#### `show`

显示 stash 条目中记录的文件级更改。

| 参数 / 标志 | 说明 |
|-------------|------|
| `<stash>` | Stash 引用，例如 `stash@{1}`。默认是 `stash@{0}`。 |
| `-p` / `--patch` | 以统一 diff（patch）形式显示 stash 的改动，取代文件级摘要。 |
| `--name-only` | 只显示已更改文件名，每行一个。 |
| `--name-status` | 显示带状态码前缀的文件名（`A` / `M` / `D`）。 |

`--name-only` 和 `--name-status` 在人工渲染模式下互斥；无论设置哪个提示，JSON 信封始终携带包含状态的完整 `files` 列表。使用 `-p` / `--patch` 时，人类输出为统一 diff（无摘要脚注），JSON 信封增加 `patch` 字段（否则不含）。

```bash
# stash@{0} 的文件级摘要
libra stash show

# 查看指定 stash 条目
libra stash show stash@{1}

# 以统一 diff 显示 stash 改动
libra stash show -p

# 只显示文件名
libra stash show --name-only
```

#### `branch`

从 stash 条目创建新分支，在该分支上应用 stash，然后删除该条目。当 stash 只在某个已不存在的分支上能干净应用，或你想把贮藏的工作恢复为普通分支时很有用。

| 参数 | 说明 |
|------|------|
| `<branch>` | 要创建的新分支名称。必需。 |
| `<stash>` | Stash 引用，例如 `stash@{1}`。默认是 `stash@{0}`。 |

```bash
# 基于最新 stash 创建分支并删除该 stash
libra stash branch hotfix

# 基于指定 stash 创建分支
libra stash branch hotfix stash@{2}
```

#### `clear`

移除所有 stash 条目。在 `--json` / `--machine` 模式之外，需要 `--force` 以避免意外数据丢失。

| 标志 | 说明 |
|------|------|
| `--force` | 跳过确认要求。人工模式下必需；JSON / machine 模式会自动绕过。 |

```bash
# 人工模式（没有 --force 会拒绝）
libra stash clear --force

# JSON 模式（不需要 --force）
libra stash clear --json
```

### 全局标志

| 标志 | 说明 |
|------|------|
| `--json` | 输出结构化 JSON |
| `--quiet` | 抑制人类可读输出 |

## 常用命令

```bash
# 保存当前更改
libra stash push

# 带消息保存
libra stash push -m "work in progress on feature X"

# 保存已跟踪修改和可见未跟踪文件
libra stash push -u

# 保存未暂存 delta，同时保留暂存区内容
libra stash push --keep-index

# 列出 stashes
libra stash list

# 应用并移除最新 stash
libra stash pop

# 只应用，不移除
libra stash apply

# 删除指定 stash
libra stash drop stash@{1}

# 面向脚本的 JSON 输出
libra stash list --json
```

## 人工输出

**`stash push`**（有更改）：

```text
Saved working directory and index state WIP on main: abc1234 ...
```

**`stash push`**（干净工作树）：

```text
No local changes to save
```

**`stash list`**：

```text
stash@{0}: WIP on main: abc1234 initial commit
stash@{1}: On main: work in progress on feature X
```

**`stash pop` / `stash apply`**：

```text
On branch main
Changes restored from stash@{0}
```

**`stash drop`**：

```text
Dropped stash@{0} (abc1234...)
```

## 结构化输出（JSON）

传入 `--json` 时，所有子命令都会生成 JSON 信封：

```json
{
  "command": "stash",
  "data": {
    "action": "push",
    "message": "WIP on main: abc1234 ...",
    "stash_id": "..."
  }
}
```

使用 `-u`、`-a` 或 `--keep-index` 时，push 信封只会额外带上相关字段：

```json
{
  "command": "stash",
  "data": {
    "action": "push",
    "message": "WIP on main: abc1234 ...",
    "stash_id": "...",
    "included_untracked": 2,
    "kept_index": true
  }
}
```

在干净工作树上，`stash push --json` 返回：

```json
{
  "command": "stash",
  "data": { "action": "noop", "message": "No local changes to save" }
}
```

`data.action` 字段是以下值之一：`noop`、`push`、`pop`、`apply`、`drop`、`list`、`show`、`branch`、`clear`。

### `list` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "list",
    "entries": [
      { "index": 0, "message": "WIP on main: ...", "stash_id": "abc1234..." }
    ]
  }
}
```

### `pop` / `apply` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "pop",
    "index": 0,
    "stash_id": "abc1234...",
    "branch": "main"
  }
}
```

### `drop` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "drop",
    "index": 0,
    "stash_id": "abc1234..."
  }
}
```

### `show` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "show",
    "stash": "stash@{0}",
    "stash_id": "abc1234...",
    "files": [
      { "path": "src/foo.rs", "status": "M" }
    ],
    "files_changed": {
      "total": 1,
      "added": 0,
      "modified": 1,
      "deleted": 0
    }
  }
}
```

结构化信封始终输出完整的 `files` 列表。`--name-only` / `--name-status` 标志只影响人工渲染输出。

### `branch` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "branch",
    "branch": "hotfix",
    "stash": "stash@{0}",
    "stash_id": "abc1234...",
    "applied": true,
    "dropped": true
  }
}
```

### `clear` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "clear",
    "cleared_count": 3
  }
}
```

## 设计动机

### 未跟踪和忽略文件如何存储

`stash push -u` 和 `stash push -a` 使用第三个 stash parent 保存未跟踪/全部文件快照，与 Git 的对象拓扑保持一致。`stash apply` 和 `stash pop` 会把这些文件恢复为未跟踪的工作区文件。如果恢复时会覆盖本地已有文件，apply/pop 会失败并保留 stash 条目。

### `--keep-index` 如何工作

`stash push --keep-index` 写入与普通 push 相同的 stash 元数据，然后恢复保存前的索引，并把工作区还原到索引状态。对于同一文件同时存在已暂存和未暂存编辑的情况，已暂存内容会留在索引和工作区中，未暂存 delta 会进入 stash。

### 为什么采用精选子命令模型？

Git 的 stash 经过长期演化，支持把 `git stash` 作为 `git stash push` 的简写，还支持 `git stash save`（已弃用）以及 plumbing 组合 `git stash create` / `git stash store`。Libra 暴露用户实践中实际会用到的八个子命令：`push`、`pop`、`list`、`apply`、`drop`、`show`、`branch` 和 `clear`。Plumbing 组合（`create` / `store`）以及 `save` 简写被推迟；参见 [`docs/development/commands/_compatibility.md`](../../development/commands/_compatibility.md) 的 D8 和 D9 小节。这让日常工作流保持与标准 Git 对齐，同时把很少使用的 plumbing 留在维护面之外。

### 为什么使用 `stash@{N}` 语法而不是纯索引？

Libra 保留 Git 的 `stash@{N}` 引用语法以保持熟悉度。从 Git 迁移的用户可以沿用同样的肌肉记忆。解析器在某些上下文中也接受裸整数，但规范形式仍然是 `stash@{N}`。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|------|-------|-----|----|
| Push（保存更改） | `stash push` | `stash push` / `stash save`（已弃用） | N/A（无 stash；使用 `jj new` 搁置） |
| 消息 | `-m <message>` | `-m <message>` | N/A |
| 保留索引 | `--keep-index` | `--keep-index` / `--no-keep-index` | N/A |
| 包含未跟踪 | `-u` / `--include-untracked` | `-u` / `--include-untracked` | N/A |
| 不包含未跟踪 | `--no-include-untracked`（撤销 `-u`） | `--no-include-untracked` | N/A |
| 包含全部（也含忽略文件） | `-a` / `--all` | `-a` / `--all` | N/A |
| Pathspec（部分 stash） | `stash push -- <pathspec>...`（文件/目录路径，`.` 选整树，其余保留；不能与 `-u`/`-a`/`-k` 同用→`LBR-CLI-002`；无匹配→`LBR-CLI-003`） | `stash push [--] <pathspec>...` | N/A |
| Pop | `stash pop [ref]` | `stash pop [--index] [<stash>]` | N/A |
| Apply | `stash apply [ref]` | `stash apply [--index] [<stash>]` | N/A |
| Drop | `stash drop [ref]` | `stash drop [<stash>]` | N/A |
| List | `stash list` | `stash list [<log-options>]` | N/A |
| 显示文件级摘要 | `stash show [<stash>] [--name-only \| --name-status]` | `stash show [<stash>]` | N/A |
| 以 patch 显示 stash | `stash show -p \| --patch [<stash>]` | `stash show -p [<stash>]` | N/A |
| 从 stash 创建分支 | `stash branch <branch> [<stash>]` | `stash branch <branch> [<stash>]` | N/A |
| 清空所有 stash | `stash clear [--force]` | `stash clear` | N/A |
| Plumbing create/store | 不支持（已推迟，见 compatibility/declined.md D8/D9） | `stash create` / `stash store` | N/A |
| JSON 输出 | `--json` | 不支持 | N/A |
| Quiet 模式 | `--quiet` | `-q` / `--quiet` | N/A |

注意：jj 没有 stash 命令。它基于变更的模型允许用 `jj new` 创建匿名变更，起到类似 stash 的作用。

## 错误处理

| 代码 | 条件 |
|------|------|
| `LBR-REPO-001` | 不是 libra 仓库 |
| `LBR-REPO-003` | 没有初始提交 |
| `LBR-CLI-002` | stash 引用语法无效 |
| `LBR-CLI-003` | stash 不存在 |
| `LBR-CONFLICT-001` | 应用 stash 时发生合并冲突 |
