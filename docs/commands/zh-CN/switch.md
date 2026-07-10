# `libra switch`

切换分支、创建并切换到新分支，或在特定提交上 detach HEAD。

**别名：** `sw`

## 概要

```
libra switch <branch>
libra switch -c <name> [<start-point>]
libra switch -C <name> [<start-point>]
libra switch --orphan <name>
libra switch -d <commit|tag|branch>
libra switch --track <remote/branch>
libra switch [--guess | --no-guess] <branch>
```

## 说明

`libra switch` 是更改分支的主要命令。它会在切换前验证工作树干净，更新 HEAD 和索引，并恢复工作树以匹配目标提交。与作为 Git 兼容表面存在的 `libra checkout` 不同，`switch` 是分支操作的推荐命令。

该命令支持多种模式：切换到已有本地分支（默认）、用 `-c` 创建新分支、用 `-C` 强制创建或重置分支、用 `--orphan` 创建 unborn 无父提交分支、用 `-d` detach HEAD，以及用 `--track` 跟踪远程分支。当目标分支已经是当前分支时，该命令是 no-op，并完全跳过干净性检查。

当找不到分支时，会通过 Levenshtein 距离提供模糊分支名建议，帮助捕获拼写错误，而无需精确匹配。

## 选项

| 标志 | 长选项 | 值 | 说明 |
|------|------|-------|-------------|
| | `<branch>` | 位置参数（可选） | 要切换到的本地分支；与 `--detach` 搭配时也可为提交、标签或分支 |
| `-c` | `--create` | `<name>` | 创建新分支并切换到它 |
| `-C` | `--force-create` | `<name>` | 创建新分支或重置已有分支并切换到它 |
| | `--orphan` | `<name>` | 创建 unborn 无父提交分支并切换到它 |
| `-d` | `--detach` | | 在给定提交、标签或分支上 detach HEAD |
| | `--track` | | 创建跟踪给定远程分支的本地分支，并切换到它 |
| | `--guess` | | 当 `<branch>` 唯一匹配某个远程跟踪分支时自动创建 tracking 分支（默认；DWIM） |
| | `--no-guess` | | 禁用远程跟踪猜测；要求本地分支或显式 `--track` |
| | `--no-progress` | | 不显示进度条。为对齐 Git 而接受的 no-op：Libra 的 switch 从不渲染进度条。 |

### 标志细节

**`-c / --create <name> [start-point]`**：从 `<start-point>`（省略时为 HEAD）创建名为 `<name>` 的新分支，然后切换到它。会验证名称，检查不存在同名分支，并拒绝保留的内部分支名。

```bash
libra switch -c feature-x              # 从 HEAD 创建新分支
libra switch -c fix-123 abc1234        # 从特定提交创建新分支
libra switch -c release-2.0 main       # 从另一个分支创建新分支
```

**`-C / --force-create <name> [start-point]`**：类似 `--create`，但如果 `<name>` 已存在，会从 `<start-point>`（省略时为 HEAD）删除并重建该分支。当前分支不能被删除重建。

```bash
libra switch -C feature-x              # 将 feature-x 重置到 HEAD 并切换
libra switch -C fix-123 abc1234        # 从特定提交重置分支
```

`-c` 或 `-C` 成功后，`HEAD` 会保持为指向创建/重置分支的 symbolic ref（`refs/heads/<name>`），即使提供了 start-point 也是如此。

**`--orphan <name>`**：创建没有父提交历史的 unborn 分支，并把 `HEAD` 切到 `refs/heads/<name>`，但此时尚不创建分支 ref。索引和工作树会保留上一分支的状态，所以 orphan 分支上的首个用户提交会把保留的索引写成无 parent 的 root commit。切换前工作树必须干净；如果同名分支已存在，命令会 fail-closed 拒绝，而不是删除重建。

```bash
libra switch --orphan fresh-start      # 创建 unborn 分支；首个提交无 parent
```

**`-d / --detach`**：让 HEAD 直接指向某个提交，而不是分支。适合检查历史状态或从标签构建。

```bash
libra switch --detach v1.0             # 在标签处 detach
libra switch --detach abc1234          # 在提交处 detach
```

**`--track`**：查找远程跟踪引用，创建同名本地分支，设置 upstream tracking，并切换到它。与 `--create` 和 `--detach` 冲突。

```bash
libra switch --track origin/main       # 跟踪并切换到远程分支
libra switch --track feature            # 假设 origin/feature
```

**`--guess` / `--no-guess`**：当 `<branch>` 不是已有本地分支，但恰好只有一个远程有同名 tracking 分支时，`--guess`（默认）会创建同名本地分支、设置 upstream，并切换到它；这与 `--track <remote>/<branch>` 的单步行为一致。猜测默认开启，生效优先级为 `--no-guess` > `--guess` > `checkout.guess`（默认 `true`）。多个远程同名时会以歧义错误失败（退出码 128），除非 `checkout.defaultRemote` 指定一个远程。显式 `remote/branch` 形式（例如 `libra switch origin/main`）不受 guess 影响，仍会提示使用 `--track`。

```bash
libra switch feature                   # 如果只有 origin 有 feature，则自动 tracking
libra switch --no-guess feature        # 禁止远程分支猜测
```

## 常用命令

```bash
libra switch main                      # 切换到已有分支
libra switch -c feature-x              # 创建并切换到新分支
libra switch -c fix-123 abc1234        # 从特定提交创建分支
libra switch -C feature-x              # 重置分支到 HEAD 并切换
libra switch --orphan fresh-start      # 创建 unborn 分支；首个提交无 parent
libra switch --detach v1.0             # 在标签上 detach HEAD
libra switch --track origin/main       # 跟踪并切换到远程分支
libra switch feature                   # 从唯一远程自动创建 tracking 分支（guess）
libra switch --no-guess feature        # 禁用远程分支猜测
libra switch --json main               # 面向代理的结构化 JSON 输出
```

## 人类可读输出

默认人类模式将结果写到 `stdout`。

切换到已有分支：

```text
Switched to branch 'main'
```

创建并切换到新分支：

```text
Switched to a new branch 'feature'
```

在提交上 detach HEAD：

```text
HEAD is now at abc1234
```

已经在目标分支上（no-op）：

```text
Already on 'main'
```

`--quiet` 会抑制所有 `stdout` 输出。

## 结构化输出（JSON 示例）

`libra switch` 支持全局 `--json` 和 `--machine` 标志。

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- 成功时 `stderr` 保持干净

切换到已有分支：

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": "feature",
    "commit": "def5678abc1234901234567890abcdef12345678",
    "created": false,
    "detached": false,
    "unborn": false,
    "already_on": false,
    "tracking": null
  }
}
```

创建并切换到新分支：

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": "feature-x",
    "commit": "abc1234def5678901234567890abcdef12345678",
    "created": true,
    "detached": false,
    "unborn": false,
    "already_on": false,
    "tracking": null
  }
}
```

创建 unborn orphan 分支：

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": "fresh-start",
    "commit": "0000000000000000000000000000000000000000",
    "created": true,
    "detached": false,
    "unborn": true,
    "already_on": false,
    "tracking": null
  }
}
```

在标签或提交上 detach HEAD：

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": null,
    "commit": "def5678abc1234901234567890abcdef12345678",
    "created": false,
    "detached": true,
    "unborn": false,
    "already_on": false,
    "tracking": null
  }
}
```

跟踪并切换到远程分支：

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": "feature",
    "commit": "def5678abc1234901234567890abcdef12345678",
    "created": true,
    "detached": false,
    "unborn": false,
    "already_on": false,
    "tracking": {
      "remote": "origin",
      "remote_branch": "feature"
    }
  }
}
```

### Schema 说明

- `previous_branch` 在切换前 HEAD detached 时为 `null`
- `branch` 在 HEAD 当前 detached（`--detach`）时为 `null`
- `unborn` 仅在 `--orphan` 后为 `true`；首个用户提交创建分支 ref 前，`commit` 为全零 OID
- `already_on` 在目标分支等于当前分支（no-op）时为 `true`
- `tracking` 在 `--track` 或成功 guess 时存在，包含 `remote` 和 `remote_branch`
- `created` 在 `--create`、`--force-create`、`--track` 或 guess 创建/重置本地分支时为 `true`

## 设计理由

### 为什么与 checkout 分离？

Git 的 `checkout` 被过度重载：它切换分支、恢复文件、detach HEAD、创建分支，这些都通过同一命令的不同标志组合完成。这让人类和 AI 代理都难以预测行为。Libra 遵循 Git 自身的现代化路径（Git 2.23 引入），将 `checkout` 拆分为 `switch`（分支操作）和 `restore`（文件操作）。`libra switch` 只处理分支相关状态变更，使行为可预测，错误消息精确。

保持 `switch` 聚焦也简化了结构化输出：每个 `SwitchOutput` 无论操作模式如何都包含相同字段，因此代理无需猜测适用哪个 schema 变体就能解析结果。

### 为什么自动跟踪远程分支？

使用 `--track origin/feature` 时，Libra 会在单个原子操作中自动创建本地分支、设置 upstream tracking 并切换到它。这消除了 `git fetch && git branch feature origin/feature && git branch -u origin/feature feature && git switch feature` 这种多步仪式。对于在 trunk-based 工作流中运行的 AI 代理，将四个命令减少为一个命令意味着更少失败点和更简单的工具编排。

当只提供分支名时（例如 `libra switch --track feature`），`--track` 标志默认使用 `origin` 远程，这匹配最常见的远程设置。

### 为什么有模糊建议？

当找不到分支名时，Libra 会对所有已知分支计算 Levenshtein 距离，并建议编辑距离 2 以内的匹配。这可以捕获常见拼写错误（`faeture` 而不是 `feature`），无需 glob 模式或正则。建议会作为错误输出中的可操作提示出现，减少人类用户和可解析提示文本的 AI 代理的往返。

## 参数对比：Libra vs Git vs jj

| 功能 | Git | Libra | jj |
|---------|-----|-------|----|
| 切换分支 | `git switch main` | `libra switch main` | `jj edit <rev>` |
| 创建并切换 | `git switch -c feature` | `libra switch -c feature` | `jj new -m "feature"` + `jj branch create feature` |
| 从提交创建 | `git switch -c fix abc1234` | `libra switch -c fix abc1234` | `jj new abc1234` + `jj branch create fix` |
| Detach HEAD | `git switch --detach v1.0` | `libra switch --detach v1.0` | `jj edit <rev>`（始终类似 detached） |
| 跟踪远程 | `git switch --track origin/main` | `libra switch --track origin/main` | N/A（jj 跟踪所有远程） |
| 强制创建 | `git switch -C feature` | `libra switch -C feature` | N/A |
| Orphan 分支 | `git switch --orphan <name>` | `libra switch --orphan <name>` | `jj new root()` |
| 结构化输出 | 无 | `--json` / `--machine` | `--template` |
| 模糊建议 | 无 | 基于 Levenshtein 的 "did you mean" 提示 | 无 |
| 干净状态验证 | 警告但有时继续 | 以可操作错误阻止切换 | 无 dirty state 概念 |

## 错误处理

每个 `SwitchError` 变体都会映射到显式 `StableErrorCode`。

| 场景 | 错误码 | 退出码 | 提示 |
|----------|-----------|------|------|
| 缺少 track 目标 | `LBR-CLI-002` | 129 | "provide a remote branch name, for example 'origin/main'." |
| 缺少 detach 目标 | `LBR-CLI-002` | 129 | "provide a commit, tag, or branch to detach at." |
| 缺少分支名 | `LBR-CLI-002` | 129 | "provide a branch name." |
| 找不到分支 | `LBR-CLI-003` | 129 | "create it with 'libra switch -c {name}'." + 模糊建议 |
| 得到远程分支 | `LBR-CLI-003` | 129 | "use 'libra switch --track ...' to create a local tracking branch." |
| 找不到远程分支 | `LBR-CLI-003` | 129 | "Run 'libra fetch {remote}' to update remote-tracking branches." |
| 无效远程分支 | `LBR-CLI-003` | 129 | "expected format: 'remote/branch'." |
| 分支已存在 | `LBR-CONFLICT-002` | 128 | "use 'libra switch {name}' if you meant the existing local branch." |
| 内部分支被阻止 | `LBR-CLI-003` | 129 | -- |
| 未暂存更改 | `LBR-REPO-003` | 128 | "commit or stash your changes before switching." |
| 未提交更改 | `LBR-REPO-003` | 128 | "commit or stash your changes before switching." |
| 未跟踪文件会被覆盖 | `LBR-CONFLICT-002` | 128 | "move or remove it before switching." |
| 状态检查失败 | `LBR-IO-001` | 128 | -- |
| 提交解析失败 | `LBR-CLI-003` | 129 | "check the revision name and try again." |
| 分支创建失败 | `LBR-IO-002` | 128 | -- |
| HEAD 更新失败 | `LBR-IO-002` | 128 | -- |
| 委托（branch/restore） | 原始代码 | 原始 | 原始提示 |

`switch -c <existing-branch>` 当前通过 `DelegatedCli` 保留原始 `branch` 命令冲突契约，因此该路径保持 branch 命令现有错误形状，而不是添加 `SwitchError::BranchAlreadyExists` 提示。
