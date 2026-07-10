# `libra checkout`

显示当前分支、切换到已有分支、创建并切换到新分支，或通过显式 `--` 兼容形式恢复路径。
与 `git checkout` 的常见分支操作和显式路径恢复兼容。

## 概要

```
libra checkout [<branch>]
libra checkout -b <name> [<start-point>]
libra checkout -B <name> [<start-point>]
libra checkout --orphan <name>
libra checkout [<tree-ish>] -- <pathspec>...
```

## 说明

`libra checkout` 是一个 Git 兼容表面，内部委托给 `switch` 和 `restore`。它支持最常见的 `git checkout` 模式：显示当前分支、切换到已有分支、用 `-b` 从 HEAD 或显式 start-point 创建新分支、用 `-B` 从 HEAD 或显式 start-point 强制创建/重置分支、用 `--orphan` 创建 unborn orphan 分支、自动跟踪远程分支，以及在存在显式 `--` 分隔符时恢复路径。

该命令存在的目的是让从 Git 迁移的开发者可以使用熟悉的肌肉记忆。对于新工作流，优先使用 `libra switch`（分支操作）和 `libra restore`（文件操作），它们提供更丰富的错误消息、结构化 JSON 输出和更清晰的语义。

当 checkout 一个本地不存在、但匹配远程跟踪分支（例如 `origin/feature`）的分支名时，Libra 会自动创建本地 tracking 分支，设置 upstream，并执行 pull；这比 Git 的 auto-track 更进一步，会立即同步内容。

路径恢复只有在显式 `--` 分隔符存在时才启用。没有 `--` 时，`libra checkout <name>` 始终是分支模式，即使存在同名文件。`--` 后的 pathspec 使用与 `libra restore` 相同的共享 Git 风格匹配器：普通前缀、通配符 pathspec，以及 `:(top)`/`:/`/`:(glob)`/`:(literal)`/`:(icase)`/`:(exclude)`/`:!`/`:^` magic 都会生效；看起来像通配符的 pathspec 也会匹配同名的字面路径或目录前缀。

如果恢复的路径在来源 tree 中是符号链接，Libra 会在支持 symlink 的平台上恢复为真正的 symlink，而不是写入一个普通文件。链接内容按 blob 字节作为目标路径保存；不会跟随或打开该目标。

## 选项

| 标志 | 长选项 | 值 | 说明 |
|------|------|-------|-------------|
| | `<branch>` | 位置参数（可选） | 要切换到的目标分支。省略时显示当前分支。 |
| `-b` | | `<name>` | 从 `[<start-point>]` 或当前 HEAD 创建新分支并切换到它 |
| `-B` | | `<name>` | 从 `[<start-point>]` 或当前 HEAD 强制创建/重置分支并切换到它；已有分支会被重置到该提交 |
| | `[<start-point>]` | 位置参数 | 与 `-b` / `-B` 搭配使用的可选提交、标签或分支，作为新分支 tip |
| | `--orphan` | `<name>` | 创建 unborn orphan 分支，保留索引/工作树，并把 HEAD 切到该分支。不支持额外 start-point。 |
| `-d` | `--detach` | | 即使目标是分支也在其提交处 detach HEAD（而非切换到分支） |
| `-t` | `--track` | | checkout 远程跟踪分支时配置 upstream。接受式 no-op：Libra 在 checkout 远程跟踪分支时本就通过 DWIM 配置跟踪，故该标志请求的正是已有行为；对非远程目标无效果。独立显式跟踪请用 `libra switch --track`。 |
| | `--ignore-other-worktrees` | | 即使另一个 linked worktree 已 checkout 这个共享分支，也允许 checkout；该标志会绕过 Libra 的 other-worktree 安全保护。 |
| | `--no-progress` | | 不显示进度条。接受式 no-op：Libra 的 checkout 从不渲染进度条。 |
| | `--no-overlay` | | 不以 overlay 模式检出路径（source 中缺失的路径仍会被移除）。接受式 no-op：Libra 的 checkout 从不处于 overlay 模式，已是 Git 默认。（Git 的 `--overlay` 未实现。） |
| | `[<tree-ish>] -- <pathspec>...` | 位置参数 | 用共享 pathspec magic 恢复路径。没有 `<tree-ish>` 时，从索引恢复工作树。带 `<tree-ish>` 时，从该来源同时恢复索引和工作树。 |

### 标志示例

```bash
# 显示当前分支
libra checkout

# 切换到已有本地分支
libra checkout main

# 创建并切换到新分支
libra checkout -b feature-x
libra checkout -b fix-123 abc1234

# 强制创建（或重置）并切换到当前 HEAD 或 start-point
libra checkout -B feature-x
libra checkout -B feature-x main

# 创建 unborn orphan 分支；首个提交无 parent
libra checkout --orphan fresh-start

# 自动跟踪远程分支（创建本地分支、设置 upstream、pull）
libra checkout feature

# 从索引恢复路径到工作树
libra checkout -- src/main.rs

# 从 HEAD 恢复路径到索引和工作树
libra checkout HEAD -- src/main.rs

# 恢复 Rust 文件，但排除生成文件
libra checkout -- ':(glob)src/*.rs' ':(exclude)src/generated.rs'

# 从 HEAD 恢复已跟踪符号链接
libra checkout HEAD -- link-to-target
```

## 常用命令

```bash
libra checkout                         # 显示当前分支
libra checkout main                    # 切换到已有本地分支
libra checkout feature-x               # 切换到其他分支
libra checkout -b feature-x            # 创建并切换到新分支
libra checkout -b fix-123 abc1234      # 从 start-point 创建并切换
libra checkout -B feature-x            # 强制创建或重置分支并切换
libra checkout -B feature-x main       # 将分支重置到 start-point 并切换
libra checkout --orphan fresh-start    # 创建 unborn 分支；首个提交无 parent
libra checkout -- file.txt             # 从索引恢复文件到工作树
libra checkout HEAD -- file.txt        # 从 HEAD 恢复文件到索引 + 工作树
libra checkout HEAD -- link-to-target  # 恢复已跟踪符号链接本身
libra --json checkout main             # 结构化兼容输出
libra checkout --quiet main            # 切换时不输出信息性 stdout
```

## 人类可读输出

默认人类模式将结果写到 `stdout`。

显示当前分支：

```text
Current branch is main.
```

显示 detached HEAD：

```text
HEAD detached at abc1234d
```

切换到已有分支：

```text
Switched to branch 'main'
```

创建并切换到新分支：

```text
Switched to a new branch 'feature-x'
```

当 `-b` 或 `-B` 搭配 start-point 使用时，创建/重置后的分支会成为当前 symbolic `HEAD`（`refs/heads/<branch>`）；Libra 不会在操作成功后把仓库留在 detached HEAD。

创建并切换到 unborn orphan 分支：

```text
Switched to a new branch 'fresh-start'
```

执行 `checkout --orphan` 后，`HEAD` 是指向 `refs/heads/<branch>` 的 symbolic ref，但首个用户提交前该分支 ref 还不能解析。索引和工作树会保留上一分支的状态；首个提交没有 parent。如果同名分支已存在，Libra 会拒绝命令，不会删除或移动它。

自动跟踪远程分支：

```text
branch 'feature' set up to track 'origin/feature'.
Switched to a new branch 'feature'
Branch 'feature' set up to track remote branch 'origin/feature'
```

根据远程状态，后续 `pull` 步骤可能发出额外同步输出。

已经在目标分支上（no-op）：

```text
Already on main
```

路径恢复：

```text
Updated 1 path(s) from HEAD
```

`--quiet` 会抑制所有 `stdout` 输出。

## 结构化输出（JSON）

`checkout` 为兼容表面支持 `--json` 和 `--machine`。`--json` 输出普通命令信封；`--machine` 以一行 NDJSON 输出同一信封。嵌套的 `restore`、branch-upstream 和 pull 输出会被抑制，因此 stdout 只包含 checkout 结果。

切换到已有本地分支的示例：

```json
{
  "ok": true,
  "command": "checkout",
  "data": {
    "action": "switch",
    "previous_branch": "main",
    "previous_commit": "abc1234...",
    "branch": "feature-x",
    "commit": "def5678...",
    "short_commit": "def5678a",
    "switched": true,
    "created": false,
    "pulled": false,
    "already_on": false,
    "detached": false,
    "tracking": null
  }
}
```

| Action | 何时输出 |
|--------|--------------|
| `show-current` | 不带分支的 `libra checkout` |
| `already-on` | 目标分支已经 checkout |
| `switch` | Checkout 已有本地分支 |
| `create` | `checkout -b <branch> [<start-point>]`、`checkout -B <branch> [<start-point>]` 或 `checkout --orphan <branch>` |
| `track` | 从 `origin/<branch>` 创建本地分支并尝试 pull |
| `restore-paths` | 显式 `checkout [<tree-ish>] -- <pathspec>...` 路径恢复 |

远程 auto-track 输出会设置 `created: true`、`pulled: true`，并包含 `tracking.remote` 和 `tracking.remote_branch`。

对于 `checkout --orphan`，`action` 为 `create`，`created` 为 `true`，`branch` 是 unborn 分支名，首个用户提交创建分支 ref 前 `commit` / `short_commit` 为 `null`。

对于更丰富的分支工作流，`libra switch --json ...` 仍是首选结构化命令。对于文件工作流，`libra restore --json ...` 仍是首选；checkout path 模式只是 Git 兼容别名。

路径恢复示例：

```json
{
  "ok": true,
  "command": "checkout",
  "data": {
    "action": "restore-paths",
    "previous_branch": "main",
    "branch": "main",
    "switched": false,
    "restore": {
      "source": "HEAD",
      "worktree": true,
      "staged": true,
      "restored_files": ["src/main.rs"],
      "deleted_files": []
    }
  }
}
```

## 设计理由

### 为什么将 checkout 保留为兼容命令？

Git 肌肉记忆根深蒂固。使用 `git checkout` 多年的开发者会本能地输入 `libra checkout main`。Libra 不强制立即改变心智模型，而是提供 `checkout` 作为薄包装，处理最常见模式。这降低了采用门槛，同时文档会推荐并鼓励 `switch`/`restore` 拆分。

该命令有意将文件恢复放在 Git 的显式 `--` 分隔符之后。普通 `libra checkout <name>` 仍是分支模式；`libra checkout -- <path>` 和 `libra checkout <tree-ish> -- <path>` 是对应 `restore` 操作的兼容别名。

### 可见兼容表面（C5 之后）

`checkout` 作为兼容表面暴露在顶层帮助（`libra --help`）中；它**不再隐藏**。从 Git 迁移的新用户可以毫不意外地找到它，但帮助横幅和命令索引都会将日常用法引导到 `switch`（分支导航）和 `restore`（文件恢复）。`switch` 和 `restore` 提供：

- 类型化的命令特定错误枚举和稳定错误码
- 结构化 JSON 输出（`--json` / `--machine`）
- 拼写错误时的模糊分支建议
- 显式语义（不会在“切换分支”和“恢复文件”之间产生歧义）

### 为什么远程分支自动 pull？

当 `libra checkout feature` 找到 `origin/feature` 但没有本地 `feature` 分支时，它会创建本地分支，设置 upstream tracking，并立即 pull。这超出了 Git 行为（Git 只创建 tracking 分支而不 pull）。理由：

- **Trunk-based 开发**：在 Libra 的目标工作流中，checkout 远程分支意味着打算在其上工作，因此几乎总是希望拥有最新内容。
- **代理命令更少**：checkout 远程分支的 AI 代理希望立即获得工作内容，而不是得到一个需要额外 `pull` 的空 tracking 分支。
- **快速失败**：如果 pull 失败（网络错误、合并冲突），用户会立即知道，而不是之后才发现内容过时。

## 参数对比：Libra vs Git vs jj

| 功能 | Git | Libra | jj |
|---------|-----|-------|----|
| 显示当前分支 | `git branch --show-current` | `libra checkout`（无参数） | `jj log -r @` |
| 切换分支 | `git checkout main` | `libra checkout main` | `jj edit <rev>` |
| 创建并切换 | `git checkout -b feature` | `libra checkout -b feature` | `jj new` + `jj branch create` |
| 从提交创建 | `git checkout -b fix abc1234` | `libra checkout -b fix abc1234` | `jj new abc1234` + `jj branch create fix` |
| 强制创建 / 重置分支 | `git checkout -B feature main` | `libra checkout -B feature main` | N/A |
| 自动跟踪远程 | `git checkout feature`（创建 tracking） | `libra checkout feature`（创建 tracking + pull） | N/A |
| 恢复文件 | `git checkout -- file` | `libra checkout -- file`（优先 `libra restore file`） | `jj restore` |
| 从修订恢复文件 | `git checkout HEAD -- file` | `libra checkout HEAD -- file`（优先 `libra restore --source HEAD -S -W file`） | `jj restore --from <revision>` |
| Detach HEAD | `git checkout <commit>` / `git checkout --detach <branch>` | `libra checkout <commit>` / `libra checkout -d`/`--detach <branch>` | `jj edit <rev>` |
| 跟踪远程分支 | `git checkout -t`/`--track <remote>/<branch>` | `libra checkout -t`/`--track`（接受式 no-op；DWIM 本就跟踪） | N/A |
| 结构化输出 | 无 | 分支兼容动作支持 `--json` / `--machine` | `--template` |

## 错误处理

`checkout` 对 checkout 自身失败使用类型化 `CheckoutError`，并委托 path restore 失败给 `restore`，同时保留稳定错误码。

| 场景 | 稳定代码 | 消息 | 退出码 |
|----------|-------------|---------|------|
| 脏工作树（未暂存或已暂存更改） | `LBR-REPO-003` | "local changes would be overwritten by checkout" | 128 |
| 未跟踪文件会被覆盖 | `LBR-CONFLICT-002` | "local changes would be overwritten by checkout" | 128 |
| 内部分支被阻止 | `LBR-CLI-003` | "checking out '{name}' branch is not allowed" | 128 |
| 创建内部分支被阻止 | `LBR-CLI-003` | "creating/switching to '{name}' branch is not allowed" | 128 |
| 找不到分支或 start-point（无远程匹配） | `LBR-CLI-003` | "path specification '{name}' did not match any files known to libra" | 129 |
| Path 模式中 pathspec 不匹配 | `LBR-CLI-003` | "pathspec '{path}' did not match any files" | 128 |
| `-b` 与 path 模式组合 | `LBR-CLI-002` | "checkout path mode cannot be combined with -b" | 128 |
| 当前分支（no-op） | N/A | 打印 "Already on {branch}" 并成功 | 0 |
| 分支存储查询失败 | `LBR-IO-001` | "failed to resolve checkout target: {detail}" | 128 |
| 分支引用损坏 | `LBR-REPO-002` | "failed to resolve checkout target: {detail}" | 128 |
