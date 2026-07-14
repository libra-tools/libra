# `libra branch`

创建、删除、重命名、检查或列出分支。

**别名：** `br`

## 概要

```
libra branch [<new_branch>] [<commit_hash>]
libra branch -l [-r | -a] [--contains <commit>] [--no-contains <commit>] [--points-at <object>] [--merged [<commit>]] [--no-merged [<commit>]] [--sort <key>] [--ignore-case] [--format <format>] [--column[=<mode>]] [-v | --verbose]
libra branch -d <name>
libra branch -D <name>
libra branch -m [<old>] <new>
libra branch (-c | -C) [<old>] <new>
libra branch -u <upstream>
libra branch --edit-description [<branch>]
libra branch --show-current
```

## 说明

`libra branch` 管理存储在 SQLite 数据库中的本地和远程跟踪分支引用。不带参数时，它列出本地分支，并用星号高亮当前分支。给出位置参数 `<new_branch>` 时，它会创建一个指向 HEAD 的新分支；如果同时提供 `<commit_hash>`，则指向该提交。

删除有两种形式：`-d` 执行安全删除，移除前会检查该分支是否已完全合并到当前分支；`-D` 无论合并状态如何都会强制删除。两者都拒绝删除你当前所在的分支。

`--contains` 和 `--no-contains` 过滤器（别名为 `--with` 和 `--without`）可将分支列表缩小到历史中包含或不包含某个提交的分支；省略提交参数时默认为 HEAD。`--points-at <object>` 只列出 tip 等于解析后提交的分支；附注标签名和完整 `refs/tags/...` 名会递归剥离到目标提交。`--merged [<commit>]` / `--no-merged [<commit>]` 列出已合并（或尚未合并）入某提交的分支——即 tip 是否可从该提交到达，缺省 HEAD，是 `--contains` 的反方向。`--sort <key>` 按 `refname`、`version:refname`（数值感知）、`committerdate`/`creatordate`/`authordate`（tip 提交的 committer 日期，`authordate` 为 author 日期）、`objectsize`（tip 对象字节大小）或 `objectname`（tip 提交的对象 id）排序，前导 `-` 反转。未传该标志时，Git 兼容的 `branch.sort` 配置默认生效（严格 local → global → system 级联；无效值在任何列表输出前以 `LBR-CLI-002` fail-closed，local/global 读取失败为 `LBR-IO-001`）。与标志不同，配置默认既不隐含 `--list` 也不抑制 unborn-HEAD 行，与 Git 一致。已记录收窄：Git 会把重复的 `branch.sort` 值叠成多键排序；Libra 只应用胜出 scope 的最后一个值。

## 选项

| 标志 | 长选项 | 值 | 说明 |
|------|------|-------|-------------|
| | `<new_branch>` | 位置参数 | 创建指向 HEAD 或 `<commit_hash>` 的新分支 |
| | `<commit_hash>` | 位置参数（需要 `new_branch`） | 新分支的基础提交 |
| `-l` | `--list` | | 列出分支（未指定动作时默认） |
| `-D` | `--delete-force` | `<name>` | 强制删除分支，即使未完全合并 |
| `-d` | `--delete` | `<name>` | 安全删除分支（必须已完全合并） |
| `-u` | `--set-upstream-to` | `<upstream>` | 为当前分支设置 upstream tracking |
| | `--edit-description` | `[branch]` | 在配置的编辑器中编辑分支描述（`branch.<name>.description`）；空/仅注释的缓冲会清除该描述。默认当前分支。 |
| | `--show-current` | | 打印当前分支名或 detached HEAD 状态 |
| `-m` | `--move` | `<old> <new>` 或 `<new>` | 重命名分支；一个参数时重命名当前分支 |
| `-c` | `--copy` | `<old> <new>` 或 `<new>` | 复制分支（连同上游配置）到新名称，保留源分支；目标已存在则失败 |
| `-C` | `--copy-force` | `<old> <new>` 或 `<new>` | 同 `-c`，但目标已存在时覆盖 |
| `-r` | `--remotes` | | 只显示远程跟踪分支 |
| `-a` | `--all` | | 显示本地和远程跟踪分支 |
| | `--contains` | `[commit]`（默认 HEAD） | 只列出包含该提交的分支。别名：`--with` |
| | `--no-contains` | `[commit]`（默认 HEAD） | 只列出不包含该提交的分支。别名：`--without` |
| | `--points-at` | `<object>` | 只列出 tip 指向解析后提交的分支；附注标签会被剥离 |
| | `--merged` | `[commit]`（默认 HEAD） | 只列出已合并入该提交的分支（tip 可从其到达） |
| | `--no-merged` | `[commit]`（默认 HEAD） | 只列出尚未合并入该提交的分支 |
| | `--sort` | `<key>` | 按 `refname`、`version:refname`（`v:refname`）、`committerdate`/`creatordate`/`authordate`（tip 提交的 committer 日期，`authordate` 为 author 日期）、`objectsize`（tip 对象字节大小）或 `objectname`（tip 提交的对象 id）排序；前导 `-` 反转（dash 形式用 `--sort=-committerdate`）。优先于 `branch.sort` 配置默认 |
| | `--format` | `<format>` | 以 for-each-ref 格式串渲染每个分支（如 `%(refname:short)`、`%(objectname)`、`%(HEAD)`、`%(upstream)`、`%(if)`…`%(end)`）。取代默认 `* name` 列表（及 `-v`/`--column`），复用 for-each-ref atom 引擎 |
| | `--column[=<mode>]` | `always` / `auto` / `never` | 将分支列表按列布局而非每行一个（bare `--column` 即 `always`；`auto` 仅在 stdout 为终端时）。列模式显示纯文本无颜色的名称。 |
| | `--no-column` | | 不按列布局列出分支（等价于 `--column=never`），撤销先前的 `--column`（最后出现者生效）。分支默认每行一个，故单独使用时为 no-op。 |
| `-v` | `--verbose` | | 每个分支附带其 tip 的短 sha 与提交 subject。重复（`-vv`）时额外显示上游 tracking 段 `[<upstream>: ahead N, behind M]`（remote-tracking ref 未 fetch 时省略计数；无配置上游的分支不显示）。优先于 `--column`。 |

### 标志示例

```bash
# 从 HEAD 创建分支
libra branch feature-x

# 从另一个分支或提交创建分支
libra branch feature-x main
libra branch hotfix abc1234

# 列出本地分支
libra branch -l

# 列出所有分支（本地 + 远程）
libra branch -l -a

# 列出包含最新发布标签的分支
libra branch --contains v2.0

# 列出已合并入 main 的分支（或尚未合并的）
libra branch --merged main
libra branch --no-merged main

# 按名称（数值感知）排序，或反转
libra branch --sort version:refname
libra branch --sort=-refname

# 按 tip 提交日期排序（最新优先）
libra branch --sort=-committerdate

# 列出不包含 HEAD 的分支
libra branch --no-contains

# 安全删除已合并分支
libra branch -d topic

# 无论合并状态如何强制删除
libra branch -D experiment

# 重命名当前分支
libra branch -m new-name

# 重命名任意分支
libra branch -m old-name new-name

# 复制分支（保留原分支）
libra branch -c old-name new-name

# 设置 upstream tracking
libra branch -u origin/main

# 显示当前分支名
libra branch --show-current

# 面向代理的 JSON 输出
libra branch --json --show-current
```

## 常用命令

```bash
libra branch feature-x                  # 从 HEAD 创建分支
libra branch feature-x main             # 从另一个分支创建分支
libra branch -d topic                   # 删除已完全合并的分支
libra branch -D topic                   # 强制删除分支
libra branch --set-upstream-to origin/main
                                        # 为当前分支设置 upstream
libra branch --json --show-current      # 面向代理的结构化 JSON 输出
```

## 人类可读输出

- List：打印分支列表，用 `*` 标记当前分支
- 安全删除：`Deleted branch feature (was abc123...)`
- 重命名：`Renamed branch 'old' to 'new'`
- 复制：`Copied branch 'old' to 'new'`
- `--show-current`：打印当前分支名；detached 时打印 `HEAD detached at <hash>`

## 结构化输出（JSON 示例）

`--json` / `--machine` 使用 `action` 区分操作：

```json
{
  "ok": true,
  "command": "branch",
  "data": {
    "action": "create",
    "name": "feature",
    "commit": "abc123..."
  }
}
```

List 动作：

```json
{
  "ok": true,
  "command": "branch",
  "data": {
    "action": "list",
    "branches": [
      { "name": "main", "current": true, "commit": "abc1234..." },
      { "name": "feature", "current": false, "commit": "def5678..." }
    ]
  }
}
```

Show-current 动作：

```json
{
  "ok": true,
  "command": "branch",
  "data": {
    "action": "show-current",
    "name": "main",
    "detached": false,
    "commit": "abc1234..."
  }
}
```

支持的动作：

- `list`: `branches`
- `create`: `name`, `commit`
- `delete`: `name`, `commit`, `force`
- `rename`: `old_name`, `new_name`
- `set-upstream`: `branch`, `upstream`
- `show-current`: `name`, `detached`, `commit`

## 设计理由

### 为什么没有 --track/--no-track？

Git 的 `--track` 和 `--no-track` 标志控制新分支是否自动设置 upstream 关系。Libra 在 `branch` 中省略它们，因为 tracking 配置通过 `--set-upstream-to` 显式处理，或在 switch 时通过 `libra switch --track` 处理。这种分离让 `branch` 专注于 ref 创建，并避免 `git branch feature origin/feature` 静默配置 tracking 这种令人困惑的隐式行为。当代理创建分支时，它应当知道是否配置了 tracking；显式优于隐式。

### 为什么 `--contains`/`--no-contains` 有别名 --with/--without？

`--contains` 和 `--no-contains` 标志镜像 Git 以保持兼容，但 Libra 增加了更短的 `--with` 和 `--without` 别名。它们在脚本中读起来更自然（`libra branch --with v2.0`）并减少输入。标志接受可选提交参数，默认为 HEAD，覆盖了“哪些分支包含我当前工作？”这个最常见场景。

### 为什么使用 SQLite-backed refs？

Git 将分支引用存储为 `.git/refs/heads/` 下的单独文件。这在规模扩大时会产生问题：拥有数千分支的 monorepo 会遭遇文件系统开销、packed-refs 争用，以及并发更新时的竞态。Libra 将所有引用存储在 SQLite 数据库（`libra.db`）中，提供：

- **原子事务**：分支 create/delete/rename 是单事务操作，没有部分写入或损坏 ref 文件的风险。
- **高效查询**：列出分支、用 `--contains` 过滤和 upstream 查询都是 SQL 查询，而不是目录扫描。
- **并发安全**：SQLite 的 WAL 模式处理并发读取和串行化写入，无需外部锁。
- **一致快照**：读取多个 ref 的操作（例如 `--contains` 过滤）会看到 ref 存储的一致视图。

代价是 refs 不能作为普通文件直接检查。Libra 通过面向工具集成的结构化 JSON 输出进行弥补。

## 参数对比：Libra vs Git vs jj

| 功能 | Git | Libra | jj |
|---------|-----|-------|----|
| 创建分支 | `git branch <name>` | `libra branch <name>` | `jj branch create <name>` |
| 从提交创建 | `git branch <name> <commit>` | `libra branch <name> <commit>` | `jj branch create <name> -r <rev>` |
| 列出分支 | `git branch [-l]` | `libra branch [-l]` | `jj branch list` |
| 删除（安全） | `git branch -d <name>` | `libra branch -d <name>` | `jj branch delete <name>` |
| 删除（强制） | `git branch -D <name>` | `libra branch -D <name>` | `jj branch delete <name>`（总是强制） |
| 重命名 | `git branch -m <old> <new>` | `libra branch -m <old> <new>` | 不支持 |
| 复制 | `git branch -c <old> <new>` | `libra branch -c <old> <new>`（`-C` 强制） | 不支持 |
| 设置 upstream | `git branch -u <upstream>` | `libra branch -u <upstream>` | N/A（无 upstream 概念） |
| 显示当前 | `git branch --show-current` | `libra branch --show-current` | `jj log -r @` |
| 远程分支 | `git branch -r` | `libra branch -r` | `jj branch list --all` |
| 所有分支 | `git branch -a` | `libra branch -a` | `jj branch list --all` |
| Contains 过滤器 | `git branch --contains <commit>` | `libra branch --contains <commit>` | `jj log -r 'branches() & ancestors(<rev>)'` |
| Merged 过滤器 | `git branch --merged [<commit>]` / `--no-merged` | `libra branch --merged [<commit>]` / `--no-merged` | `jj log -r 'branches() & ::<rev>'` |
| 列表排序 | `git branch --sort <key>` | `libra branch --sort <key>`（refname / version:refname / committerdate / creatordate / authordate / objectsize / objectname） | `jj branch list`（revset 顺序） |
| 自定义格式 | `git branch --format <format>` | `libra branch --format <format>`（for-each-ref atom；取代 `* name`/`-v`/`--column`） | N/A |
| 列布局 | `git branch --column[=<mode>]` | `libra branch --column[=<mode>]`（`--no-column` 撤销） | N/A |
| 详细列表 | `git branch -v` / `-vv` | `libra branch -v`（sha + subject）/ `-vv`（+ 上游 tracking） | N/A |
| 自动 tracking | `git branch --track` | N/A（使用 `switch --track`） | N/A |
| 结构化输出 | 无 | `--json` / `--machine` | `--template` |
| 模糊建议 | 无 | 基于 Levenshtein 的 "did you mean" | 无 |

## 错误处理

| 场景 | 错误码 | 提示 |
|----------|-----------|------|
| 无效起点或缺少分支 | `LBR-CLI-003` | "use 'libra branch -l' to list branches" + 模糊建议 |
| 无效分支名 | `LBR-CLI-002` | "branch names cannot contain spaces, '..', '@{', or control characters." |
| 分支已存在 | `LBR-CONFLICT-002` | "delete it first or choose a different name." |
| 不能删除当前分支 | `LBR-REPO-003` | "switch to a different branch first." |
| 分支未完全合并（安全删除） | `LBR-REPO-003` | "use '-D' to force-delete." |
| 锁定/内部分支 | `LBR-CLI-003` | -- |
| HEAD detached（rename/upstream） | `LBR-REPO-003` | -- |
| 无法写入 refs | `LBR-IO-002` | -- |
| 存储查询失败 | `LBR-IO-001` | -- |
| 存储的引用损坏 | `LBR-REPO-002` | -- |
