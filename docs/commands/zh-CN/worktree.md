# `libra worktree`

管理附加到此仓库的多个工作树。

**别名：** `wt`

## 概要

```
libra worktree add <path>
libra worktree list
libra worktree lock <path> [--reason <text>]
libra worktree unlock <path>
libra worktree move <src> <dest>
libra worktree prune
libra worktree remove <path>
libra worktree umount <path> [--cleanup]
libra worktree repair [<path>]
```

## 说明

`libra worktree` 管理共享同一个仓库数据库和对象存储的多个工作树。这允许你同时拥有同一仓库的多个 checkout，适用于同时处理多个分支、编辑代码时运行构建，或隔离测试更改。

每个 linked worktree 都是一个目录，其中包含它自己的真实 `.libra` gitdir——一个本地目录（不是符号链接），保存该 worktree 私有的 `HEAD`、index 和 `HEAD` reflog，以及指向共享存储的 `commondir` 指针和稳定的 `worktree_id`。主工作树是原始仓库目录。所有工作树共享同一个 SQLite 数据库、对象存储、branch/tag/remote refs 和配置，但各自拥有独立的 checked-out 分支和暂存状态。（由更早版本 Libra 创建的 worktree 可能仍是旧的共享 `.libra` 符号链接布局；运行 `libra worktree repair` 检查。）registry 文件 `worktrees.json` 自 v0.19.57 起带版本号（`schema_version: 2`）：每个 linked 条目持久化其 stable `worktree_id`；旧 v1 文件在首个**变更类** worktree 命令时就地升级（id 从各 gitdir 回填，`worktree list` 等无锁读取不会重写文件）；旧版二进制在数据库层被拒绝，无法误读或重写 v2 文件。

Worktree 元数据持久化在 `.libra` 存储目录内的 `worktrees.json` 文件中。每个条目记录文件系统路径、它是否是主工作树、锁定状态，以及可选锁定原因。状态文件通过临时文件重命名原子写入，以防损坏。

添加新 worktree 且 HEAD 指向提交时，该 worktree 会自动用 HEAD 中的已提交内容填充（不是已暂存的索引更改）。

## 选项

### 子命令：`add`

在给定文件系统路径创建新的 linked worktree。

| 参数 / 标志 | 说明 |
|-------------|------|
| `<path>` | 新 worktree 的文件系统路径,可相对可绝对;目录不存在时自动创建;不得位于 `.libra` 存储内、不得已注册、存在时必须为空。 |
| `<branch-or-commit>` | 可选目标。已存在的 branch 会被**附着**检出(任一 worktree——包括当前——已检出该 branch 时在任何副作用前拒绝);其余按 commit-ish 解析并以**分离 HEAD** 播种、内容取自该 commit。branch 不存在时 fail-closed:Git 的 remote-branch DWIM、`worktree.guessRemote`、`--track`/`--no-track` 首期 deferred。 |
| `--detach` | 即使目标是 branch 也分离 HEAD(branch 仍可被其它 worktree 检出)。 |
| `-b, --create-branch <NEW_BRANCH>` | 在 `<branch-or-commit>`(默认:源 worktree HEAD)处创建新 branch 并检出;branch 已存在时拒绝(`-B`/`--force` deferred);后续任何失败会完整回滚——不留 branch-only 残留。 |

不带目标时,新 worktree 以**源 commit 的分离 HEAD** 创建——与 Git 默认(以路径 basename 创建并检出新 branch)有意不同。`--lock`、`--orphan`、`--no-checkout` 首期 deferred;锁定请用独立的 `worktree lock` 子命令。

```bash
# 源 commit 处分离(Libra 默认)
libra worktree add ../my-feature

# 检出既有 branch
libra worktree add ../fix-1 hotfix

# 在 commit-ish 处分离
libra worktree add --detach ../probe v1.2.0

# 从起点创建新 branch 并检出
libra worktree add -b topic ../topic main
```

### 子命令：`list`

列出所有已注册 worktrees 及其状态。`--porcelain` 输出稳定的机器可读格式：每个 worktree 输出 `worktree <path>`、它自己的 `HEAD <sha>` 行，以及 `branch <ref>` 或 `detached` 行（每个 worktree 各自拥有 HEAD），被锁定时再加 `locked [<reason>]` 行，条目间空行分隔。若某 worktree 的 HEAD 无法解析（旧的共享 `.libra` 布局，或缺失/损坏的 scope），则省略 HEAD 相关行，而不是错误地标上其它 worktree 的提交。

```bash
libra worktree list
libra worktree list --porcelain
libra --json worktree list
libra --machine worktree list
```

结构化输出使用 `worktree.list` 命令信封。每个条目报告 `kind`、`path`、`is_main`、`locked`、`lock_reason`，以及该路径当前是否存在于磁盘上。

### 子命令：`lock`

将 worktree 标记为 locked，防止它被 prune 或 remove。

| 参数 / 标志 | 说明 |
|-------------|------|
| `<path>` | 要锁定的 worktree 文件系统路径。 |
| `--reason` | 可选的人类可读说明，解释为什么锁定该 worktree。 |

```bash
# 锁定 worktree
libra worktree lock ../my-feature

# 带原因锁定
libra worktree lock ../my-feature --reason "long-running experiment"
libra --json worktree lock ../my-feature --reason "long-running experiment"
```

### 子命令：`unlock`

移除先前锁定 worktree 的锁。幂等：解锁已经未锁定的 worktree 是无操作。

| 参数 | 说明 |
|------|------|
| `<path>` | 要解锁的 worktree 文件系统路径。 |

```bash
libra worktree unlock ../my-feature
libra --machine worktree unlock ../my-feature
```

### 子命令：`move`

移动或重命名现有 linked worktree。磁盘目录会被重命名，注册表也会更新。不能移动主 worktree 或已锁定 worktree。

| 参数 | 说明 |
|------|------|
| `<src>` | worktree 当前文件系统路径。 |
| `<dest>` | 新文件系统路径。磁盘上或注册表中不得已存在。不得位于 `.libra` 存储内部。 |

```bash
libra worktree move ../my-feature ../my-feature-v2
libra --json worktree move ../my-feature ../my-feature-v2
```

### 子命令：`prune`

从注册表移除磁盘目录已不存在的 worktree。只有 stat 返回 NotFound 的路径才算缺失——权限错误或未挂载卷绝不会把 worktree 判为缺失。主 worktree、已锁定 worktree、tombstone 条目（由 repair 处理）以及存在进行中 rebase/cherry-pick/bisect 的 scope 绝不会被 prune。被 prune 条目的 scoped 状态清理失败时,条目保留为 `tombstone`(输出 `tombstoned` 字段)由 `libra worktree repair` 重试。

```bash
libra worktree prune
libra --machine worktree prune
```

### 子命令：`remove`

移除 worktree。默认（保留目录）自 v0.19.58 起为**分离（detach）**：registry 条目转入 `detached_from_registry` 状态，其 scoped 数据库状态（HEAD、reflog、layer/sparse/dirty 行）全部保留，gitdir 中的标记使该目录内的一切命令 fail-closed 并给出 re-add/删除提示；用 `libra worktree add <path>` 重新挂接（按 registry 持久化 id 校验目录身份），或用 `--delete-dir` 完成删除。传入 `--delete-dir` 为 Git 风格行为：脏状态检查通过后删除目录并 fsync 父目录，然后才清理 scoped 数据库状态；清理失败会保留 `tombstone` 条目由 `libra worktree repair` 重试。不能移除主 worktree、已锁定 worktree,或存在进行中 rebase/cherry-pick/bisect 的 worktree。

| 参数 / 标志 | 说明 |
|-------------|------|
| `<path>` | 要注销的 worktree 文件系统路径。 |
| `--delete-dir` | 注销后，同时删除磁盘目录。当 worktree 包含未提交更改（已暂存或未暂存）时拒绝。 |

```bash
# 默认：保留磁盘目录
libra worktree remove ../my-feature
libra --json worktree remove ../my-feature

# Git 风格：同时删除目录（仅干净 worktree）
libra worktree remove --delete-dir ../my-feature
libra --machine worktree remove --delete-dir ../my-feature

# 脏时拒绝：
$ libra worktree remove --delete-dir ../dirty-feature
fatal: cannot delete dirty worktree '../dirty-feature' (uncommitted changes)
       Hint: commit or stash changes, or remove without --delete-dir to keep the directory
```

行为有意不同于 Git：Git 默认删除目录。Libra 默认保留目录以防意外数据丢失；`--delete-dir` 以显式 opt-in 恢复类 Git 语义。动机见 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 和 [`compatibility/worktree-surface.md`](../../development/commands/worktree.md)。

### 子命令：`umount`

卸载 FUSE worktree 挂载点。主要用于在操作系统报告路径 busy 时清理陈旧的 Agent task worktree。该命令也接受 Libra task worktree 根目录，并自动解析其 `workspace` 挂载点。

别名：`unmount`

| 参数 / 标志 | 说明 |
|-------------|------|
| `<path>` | FUSE 挂载点路径，或包含 `workspace` 挂载点的 Libra task worktree 根目录。 |
| `--cleanup` | 卸载后，移除 Libra task worktree 根目录。只接受 task FUSE worktree 路径。 |

```bash
libra worktree umount /repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id/workspace --cleanup
libra --json worktree umount /repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id --cleanup
```

JSON / machine 输出信封：

```json
{
  "ok": true,
  "command": "worktree.umount",
  "data": {
    "mountpoint": "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id/workspace",
    "unmounted": true,
    "cleanup_requested": true,
    "cleanup_root": "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id",
    "cleanup_root_removed": true
  }
}
```

### 子命令：`repair`

修复 worktree 元数据。不带参数时：移除重复条目（相同规范路径）、确保恰好存在一个主 worktree 条目，并运行 W3 lifecycle 恢复引擎——确定性回放中断的 add/move/remove/prune intent journal（恢复过程绝不删除目录）、重试 tombstone 条目的 scoped 清理、按 registry 重建 detached 标记与 SQL lifecycle 镜像；只有实际做出更改时才写入状态文件。

带 `--migrate-layout` 时(自 v0.19.60,W3-s3 §C.6):从**主 worktree** 把 legacy 共享 `.libra` 符号链接布局迁移到隔离布局(`--dry-run` 只报告;不带 path 迁移全部 legacy 条目)。迁移以原子 rename 安装带 journal 标识的新 gitdir(legacy 链接保留为 backup 直至校验通过),以共享 HEAD 快照播种分离 HEAD 并据其重建 private index:工作区文件绝不改动(迁移后按新 index 显示为 dirty/untracked),共享 index 的 staged 状态绝不复制——请先在主 worktree commit/stash。共享 index 存在冲突或主 scope 有进行中 sequencer 时在任何 rename 前拒绝;中断的迁移由下一次普通 `worktree repair` 按身份恢复。面向目标的生命周期命令(两种模式的 `worktree remove <path>` 与 `worktree repair <path>`)对 legacy-symlink 目标同样拒绝——共享符号链接会把写入导向**主**存储——须先完成迁移。

带路径参数时（registry v2，自 v0.19.57）：依据 registry 中持久化的 stable id 恢复该 **linked** worktree 的 gitdir 身份——重写缺失或损坏的 `.libra/worktree_id`，并恢复缺失或损坏（空/不可读）的 `commondir` 指针（指向本仓库共享存储）。身份永远来自 registry，绝不猜测；`commondir` 有效地指向**另一个**存储时会被拒绝（绝不悄悄改挂仓库），且拒绝时不产生任何写入。未注册路径与主 worktree 会被拒绝；registry 仍为 legacy v1 格式时（无持久化身份）同样拒绝——先运行一次不带参数的 `libra worktree repair` 完成升级后重试。

```bash
libra worktree repair
libra --json worktree repair
libra worktree repair ../experiment
libra --json worktree repair ../experiment
```

## 常用命令

```bash
# 创建新 worktree
libra worktree add ../experiment

# 列出所有 worktrees
libra wt list

# 锁定 worktree 以保护它
libra wt lock ../experiment --reason "production hotfix in progress"

# 完成后解锁
libra wt unlock ../experiment

# 将 worktree 移到新位置
libra wt move ../experiment ../experiment-v2

# 清理目录已删除的 worktrees
libra wt prune

# 注销 worktree（保留磁盘文件）
libra wt remove ../experiment-v2

# 修复不一致的 worktree 元数据
libra wt repair
```

## 人工输出

**`worktree add`**：

```text
/Users/alice/projects/my-feature
```

**`worktree list`**：

```text
main /Users/alice/projects/my-repo
worktree /Users/alice/projects/my-feature
worktree /Users/alice/projects/hotfix [locked: production hotfix in progress]
```

**`worktree remove`**：

```text
Removed worktree '/Users/alice/projects/my-feature' from registry. Directory kept on disk.
Removed worktree '/Users/alice/projects/my-feature' from registry and deleted directory.
```

**`worktree prune`**（有陈旧条目）：

```text
Will prune 2 worktrees:
  /Users/alice/projects/old-experiment
  /Users/alice/projects/deleted-branch
Pruned 2 worktrees
```

**`worktree prune`**（没有需要 prune 的条目）：

```text
No worktrees to prune
```

## JSON 输出

`worktree add`、`lock`、`unlock`、`move`、`prune`、`remove` 和 `repair` 使用命令专用信封。`--machine` 以紧凑单行 JSON 输出相同 schema。

**`worktree.add`**：

```json
{
  "ok": true,
  "command": "worktree.add",
  "data": {
    "path": "/Users/alice/projects/my-feature",
    "already_exists": false
  }
}
```

**`worktree.list`**：

```json
{
  "ok": true,
  "command": "worktree.list",
  "data": {
    "worktrees": [
      {
        "kind": "main",
        "path": "/Users/alice/projects/my-repo",
        "is_main": true,
        "locked": false,
        "lock_reason": null,
        "exists": true
      }
    ]
  }
}
```

**`worktree.lock`**：

```json
{
  "ok": true,
  "command": "worktree.lock",
  "data": {
    "path": "/Users/alice/projects/my-feature",
    "locked": true,
    "lock_reason": "long-running experiment",
    "changed": true
  }
}
```

**`worktree.unlock`**：

```json
{
  "ok": true,
  "command": "worktree.unlock",
  "data": {
    "path": "/Users/alice/projects/my-feature",
    "locked": false,
    "changed": true
  }
}
```

**`worktree.move`**：

```json
{
  "ok": true,
  "command": "worktree.move",
  "data": {
    "source": "/Users/alice/projects/my-feature",
    "destination": "/Users/alice/projects/my-feature-v2",
    "registry_updated": true,
    "disk_directory_moved": true
  }
}
```

**`worktree.prune`**：

```json
{
  "ok": true,
  "command": "worktree.prune",
  "data": {
    "pruned": ["/Users/alice/projects/old-experiment"],
    "pruned_count": 1
  }
}
```

**`worktree.remove`**：

```json
{
  "ok": true,
  "command": "worktree.remove",
  "data": {
    "path": "/Users/alice/projects/my-feature",
    "registry_removed": true,
    "disk_directory_deleted": false
  }
}
```

**`worktree.repair`**：

```json
{
  "ok": true,
  "command": "worktree.repair",
  "data": {
    "changed": true,
    "journal_recovered": 1,
    "tombstones_cleaned": 1,
    "tombstones_pending": 0,
    "notes": ["completed interrupted remove of '/abs/path/wt'"]
  }
}
```

**带路径的 `worktree.repair`**：

```json
{
  "ok": true,
  "command": "worktree.repair",
  "data": {
    "path": "/abs/path/to/experiment",
    "worktree_id": "1f0c…",
    "worktree_id_restored": true,
    "commondir_restored": true
  }
}
```

## 设计动机

### 为什么使用 JSON 文件持久化，而不是像 Git 那样使用文件系统链接？

Git 通过一组文件系统结构跟踪 worktree：主 `.git/worktrees/` 目录包含每个 worktree 的目录，里面有 `gitdir`、`HEAD` 和 `commondir` 文件，每个 linked worktree 又有一个指回去的 `.git` 文件（不是目录）。这种方式与 Git 基于文件的架构强耦合，并要求在多个位置之间仔细交叉引用。

Libra 在共享存储目录中使用单个 `worktrees.json` 文件。这有几个优势：所有 worktree 元数据位于一个可查询位置；状态通过临时文件重命名原子写入；格式也便于人类和 AI agent 检查。每个 linked worktree 拥有自己真实的 `.libra` gitdir（内含指回共享存储的 `commondir` 指针与稳定 `worktree_id`，不是符号链接），比 Git 的双向指针系统更简单。代价是 JSON 文件成为单一事实来源，必须保持一致，因此存在 `repair`。

### 为什么 lock 上有 `--reason`？

Git 的 `git worktree lock` 也支持 `--reason`，Libra 保留了这一点。锁定原因在团队环境和 AI agent 管理 worktree 时很有价值：它提供了为什么不应 prune 或 remove 该 worktree 的上下文。没有原因时，锁定 worktree 是不透明的，其他用户（或 agent）无法判断锁是否仍然相关。该原因会显示在 `list` 输出中，使锁状态自解释。

### 为什么 `remove` 不删除磁盘目录？

删除文件是不可撤销的破坏性操作。Libra 默认的 `remove` 改为**分离**：目录、其 scoped 数据库状态与身份全部保留,条目转入 `detached_from_registry` 状态,目录被冻结(其中一切命令 fail-closed 并提示 re-add/删除)。这是有意的安全选择:用户可用 `worktree add` 重新挂接,或确认后用 `--delete-dir` 完成删除。如果 worktree 包含未提交工作,这也能防止意外数据丢失。Git 的 `git worktree remove` 默认会删除目录,这曾导致工作丢失。

### 为什么 `move` 拒绝已锁定 worktree？

已锁定 worktree 表示它不应被修改。移动它会改变其文件系统路径，可能破坏其他工具、脚本或 agent 配置中对该路径的引用。用户必须先显式解锁 worktree 再移动，确保该操作是有意的。

### 为什么 `add` 从 HEAD 而不是索引填充？

创建 linked worktree 时，Libra 从 HEAD 提交恢复内容，而不是当前索引状态。这确保新 worktree 反映最后一次提交的状态，而不是只存在于原始 worktree 上下文中的已暂存未提交更改。这符合用户预期：新 worktree 从已知良好状态开始。

## 参数对比：Libra vs Git vs jj

| 操作 | Libra | Git | jj |
|------|-------|-----|----|
| 创建 worktree | `worktree add <path> [<branch-or-commit>]`（无目标：源 commit 分离） | `worktree add <path> [<branch>]` | `workspace add <path>` |
| 在分支上创建 | `worktree add <path> <branch>`（附着；任一处已检出即拒；无 DWIM） | `worktree add <path> <branch>` | `workspace add <path>`（然后 `jj edit`） |
| 创建 detached | `worktree add --detach <path> [<commit>]`（`add <path> <commit>` 亦可） | `worktree add --detach <path> <commit>` | N/A |
| 列出 worktrees | `worktree list` | `worktree list [--porcelain]` | `workspace list` |
| 锁定 | `worktree lock <path> [--reason]` | `worktree lock [--reason] <worktree>` | N/A |
| 解锁 | `worktree unlock <path>` | `worktree unlock <worktree>` | N/A |
| 移动 | `worktree move <src> <dest>` | `worktree move <worktree> <new-path>` | N/A |
| Prune | `worktree prune` | `worktree prune [--dry-run]` | N/A（自动） |
| Remove | `worktree remove <path>`（分离——目录与状态保留、冻结） | `worktree remove [--force] <worktree>`（删除目录） | `workspace forget <name>` |
| Repair | `worktree repair [<path>]` | `worktree repair [<path>...]` | N/A |
| 别名 | `wt` | N/A | N/A |
| 每个 worktree 一个分支 | `-b <new> [<start>]` 显式创建（全回滚；无 basename 默认） | 自动（新分支或已有分支） | 自动（新 working copy commit） |
| 存储 | JSON 文件（`worktrees.json`） | 文件系统结构（`.git/worktrees/`） | Operation log |
| Worktree 链接 | 真实本地 `.libra` gitdir，内含指向共享存储的 `commondir` 指针（legacy 布局为符号链接） | 指向 `gitdir` 的 `.git` 文件 | 指向共享 `.jj` 的符号链接 |

注意：jj 使用术语 "workspace" 而不是 "worktree"。每个 workspace 会自动获得自己的 working copy commit，并且 workspaces 记录在 operation log 中。jj workspaces 比 Git worktrees 更简单，因为 jj 基于变更的模型不需要为每个 workspace 单独管理分支。

## 错误处理

| 代码 | 条件 |
|------|------|
| `LBR-REPO-001` | 不是 libra 仓库 |
| `LBR-REPO-002` | `worktrees.json` 损坏 |
| `LBR-CLI-003` | Worktree 路径不能位于 `.libra` 存储内部 |
| `LBR-CLI-003` | 目标已存在且不是目录 |
| `LBR-CLI-003` | 没有该 worktree（lock、unlock、move、remove） |
| `LBR-CLI-003` | 不能移动或移除主 worktree |
| `LBR-CLI-003` | 不能移动或移除已锁定 worktree |
| `LBR-CLI-003` | 对非 task FUSE worktree 路径请求了 `worktree umount --cleanup` |
| `LBR-CONFLICT-002` | 目标目录已存在且非空 |
| `LBR-CONFLICT-002` | 目标已包含 `.libra` 条目 |
| `LBR-CONFLICT-002` | 目标已存在（move） |
| `LBR-CONFLICT-002` | 目标已注册为 worktree（move） |
| `LBR-CONFLICT-002` | 因 worktree 脏而拒绝 `--delete-dir` |
| `LBR-IO-001` | 读取或检查 worktree 路径/状态/status 失败 |
| `LBR-IO-002` | 写入 worktrees.json 失败 |
| `LBR-IO-002` | 从 HEAD 填充 worktree 失败 |
