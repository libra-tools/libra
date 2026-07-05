# `libra pull`

从远程获取对象，并将获取到的分支集成到当前分支。

## 概要

```text
libra pull [--ff-only] [--ff] [--no-ff] [--squash] [--no-commit] [--commit] [--autostash] [--no-progress] [--rebase] [--no-rebase] [--depth <n>] [<repository> [<refspec>]]
```

## 说明

`libra pull` 组合了 `fetch` 和 `libra merge` 使用的同一合并引擎。它下载新对象，更新远程跟踪引用，然后将选中的 upstream 集成到当前分支。

使用 `--rebase`（`-r`）时，集成步骤会改为在获取到的 upstream tip 之上重放仅本地提交。这等价于 `libra fetch` 后跟 `libra rebase <upstream>`。

使用 `--ff-only` 时，pull 会获取 upstream，但在本地和远程历史已经分叉时拒绝创建合并提交。快进和已经最新的 pull 仍会成功。`--ff-only` 与 `--rebase` 冲突。

不带参数调用时，命令读取当前分支 tracking 配置（`branch.<name>.remote` 和 `branch.<name>.merge`）。只给出 `<repository>` 时，当前分支名会被用作远程分支。同时给出 `<repository>` 和 `<refspec>` 时，会获取并合并指定远程分支。

Pull 支持 already-up-to-date、fast-forward 和 single-head three-way merge 结果。如果本地和远程分支冲突，pull 会返回由 merge 拥有的 `LBR-CONFLICT-002` 错误，带有 `phase: "merge"`，并留下与 `libra merge` 相同的 merge 状态。使用 `libra add <path>` 解决冲突并运行 `libra merge --continue`，或运行 `libra merge --abort`。

`pull` 已支持 `--ff-only`、`--ff`、`--no-ff`、`--squash`、`--no-commit`、`--commit`、`--autostash`、`--no-progress`、`--rebase`、`--no-rebase` 与 fetch `--depth`；尚不支持 octopus merge 与自定义合并策略（`--strategy`/`-X`）。`--no-progress` 把进度抑制转发给 fetch，抑制其 “Receiving objects” 进度条。`--autostash` 在集成前 stash 已跟踪改动、之后再 pop 回（即使整合失败也会 pop），让 `pull` 能在脏工作树上运行；未跟踪/忽略文件保持原样，pop 冲突时保留 stash 并报错（用 `libra stash pop` 恢复）。

## 选项

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `<repository>` | 要从中 pull 的远程名称。省略时使用当前分支已配置的 upstream。 | `libra pull origin` |
| `<refspec>` | 远程上的分支名。需要 `<repository>`。省略时使用当前分支名。 | `libra pull origin main` |
| `--ff-only` | 拒绝创建合并提交；仅 fast-forward 或 already-up-to-date pull 会成功。与 `--rebase` 冲突。 | `libra pull --ff-only` |
| `-r`, `--rebase` | 获取后，将当前分支 rebase 到 upstream tip，而不是合并。 | `libra pull --rebase` |
| `--no-rebase` | 合并而非 rebase（默认），撤销先前的 `--rebase`/`-r`（最后出现者生效）。pull 默认 merge，故单独使用时为 no-op。 | `libra pull --no-rebase` |
| `--json` | 向 stdout 输出结构化 JSON 信封（全局标志）。 | `libra pull --json` |
| `--machine` | 紧凑单行 JSON；抑制进度（全局标志）。 | `libra pull --machine` |
| `--quiet` | 抑制所有进度和合并摘要输出。 | `libra pull --quiet` |

## 示例

```bash
libra pull
libra pull origin main
libra pull --ff-only
libra pull --rebase origin main
```

## 人类可读输出

默认人类模式将 fetch 进度写到 `stderr`，将 pull 摘要写到 `stdout`。

快进：

```text
From git@github.com:user/repo.git
   abc1234..def5678  origin/main
Updating abc1234..def5678
Fast-forward
 3 files changed
```

干净三方合并：

```text
From git@github.com:user/repo.git
   abc1234..def5678  origin/main
Updating abc1234..def5678
Merge made by the 'three-way' strategy.
 2 files changed
```

已经最新：

```text
From git@github.com:user/repo.git
Already up to date.
```

没有 tracking 信息：

```text
There is no tracking information for the current branch.
Please specify which branch you want to merge with.
See git-pull(1) for details.

    libra pull <remote> <branch>

If you wish to set tracking information for this branch you can do so with:

    libra branch --set-upstream-to=origin/<branch> main
```

Rebase：

```text
From git@github.com:user/repo.git
   abc1234..def5678  origin/main
Successfully rebased 2 commits onto 'origin/main' (1111111..2222222).
```

`--quiet` 会抑制所有进度和合并摘要输出。

## 结构化输出

`--json` 向 stdout 写入一个成功信封。`--machine` 以一行紧凑 JSON 写入相同 schema。成功时 stderr 保持干净。

```json
{
  "ok": true,
  "command": "pull",
  "data": {
    "branch": "main",
    "upstream": "origin/main",
    "fetch": {
      "remote": "origin",
      "url": "git@github.com:user/repo.git",
      "refs_updated": [
        {
          "remote_ref": "refs/remotes/origin/main",
          "old_oid": "abc1234...",
          "new_oid": "def5678..."
        }
      ],
      "objects_fetched": 12,
      "bytes_received": 2048
    },
    "merge": {
      "strategy": "three-way",
      "old_commit": "abc1234...",
      "commit": "def5678...",
      "files_changed": 2,
      "up_to_date": false,
      "parents": ["abc1234...", "fedcba9..."]
    }
  }
}
```

Rebase 输出省略 `merge` 并包含 `rebase`：

```json
{
  "ok": true,
  "command": "pull",
  "data": {
    "branch": "main",
    "upstream": "origin/main",
    "fetch": {
      "remote": "origin",
      "url": "git@github.com:user/repo.git",
      "refs_updated": [],
      "objects_fetched": 0,
      "bytes_received": 0
    },
    "rebase": {
      "status": "completed",
      "old_commit": "1111111...",
      "commit": "2222222...",
      "replay_count": 2,
      "up_to_date": false
    }
  }
}
```

### Schema 说明

- `branch` 是正在更新的当前本地分支。
- `upstream` 是远程 tracking 分支名，例如 `"origin/main"`。
- `fetch.refs_updated` 列出 fetch 期间发生变化的远程引用。
- 根据是否传递 `--rebase`，`merge` 或 `rebase` 中恰好出现一个。
- `merge.old_commit` 是合并前的 `HEAD`；首次 pull 到空本地分支时为 `null`。
- `merge.strategy` 是 `"fast-forward"`、`"three-way"` 或 `"already-up-to-date"`。
- `merge.commit` 是合并后的新 HEAD 提交；已经最新时为 `null`。
- `merge.parents` 出现在成功的三方合并提交中。
- `merge.files_changed` 是合并结果更改的路径数量。
- `rebase.status` 是 `"completed"`、`"fast-forwarded"`、`"already-up-to-date"` 或 `"no-commits"`。
- `rebase.replay_count` 是重放到 upstream tip 之上的本地提交数量。
- `rebase.up_to_date` 在 rebase 没有移动 `HEAD` 时为 `true`。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 基本 pull | `libra pull` | `git pull` | N/A（jj 使用 `jj git fetch` + working copy） |
| 从指定远程 pull | `libra pull origin main` | `git pull origin main` | N/A |
| 快进集成 | 支持 | 支持 | N/A |
| 仅快进 pull | `libra pull --ff-only` | `git pull --ff-only` | N/A |
| 三方集成 | 通过 merge 引擎支持 | 支持 | N/A |
| Pull 时 rebase | `libra pull --rebase` | `git pull --rebase` | N/A |
| 强制合并提交 | `libra pull --no-ff` | `git pull --no-ff` | N/A |
| Squash | `libra pull --squash` | `git pull --squash` | N/A |
| 不提交 | `libra pull --no-commit` | `git pull --no-commit` | N/A |
| 强制提交 | `libra pull --commit` | `git pull --commit` | N/A |
| Autostash | `libra pull --autostash` | `git pull --autostash` | N/A |
| 抑制进度条 | `libra pull --no-progress` | `git pull --no-progress` | N/A |
| 结构化输出 | `--json` / `--machine` | 无 | 无 |
| 阶段诊断 | 错误 JSON 中的 `phase` 详情 | 无 | 无 |

## 错误处理

每个 `PullError` 变体都会映射到显式 `StableErrorCode`。Fetch、merge 和 rebase 子错误会带着 `phase` 详情转发，便于诊断。

| 场景 | 错误码 | 退出码 | 提示 |
|----------|-----------|------|------|
| HEAD detached | `LBR-REPO-003` | 128 | "checkout a branch before pulling" |
| 分支没有 tracking 信息 | `LBR-REPO-003` | 128 | Git 风格 advisory block，包含 `libra pull <remote> <branch>` 和 `libra branch --set-upstream-to=...` |
| 找不到远程 | `LBR-CLI-003` | 129 | "use 'libra remote -v' to see configured remotes" |
| Fetch：网络不可达 / 超时 | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Fetch：认证失败 | `LBR-AUTH-001` | 128 | "check SSH key or HTTP credentials" |
| Fetch：协议错误 | `LBR-NET-002` | 128 | "the remote did not respond correctly" |
| Merge：冲突、脏工作树或未跟踪覆盖 | `LBR-CONFLICT-002` | 128 | "resolve conflicts, then run 'libra merge --continue'" |
| Merge：`--ff-only` 拒绝非快进 | `LBR-CONFLICT-002` | 128 | "run 'libra pull' without --ff-only to allow a merge commit" |
| Rebase：重放期间冲突 | `LBR-CONFLICT-001` | 128 | "resolve conflicts, stage them, then run 'libra rebase --continue'" |
| Rebase：脏工作树 | `LBR-REPO-003` | 128 | "commit or stash your changes before rebasing" |
| Merge：无效目标 | `LBR-CLI-003` | 129 | "verify the upstream ref and try again" |
| Merge：无关历史或无效 merge 状态 | `LBR-REPO-003` | 128 | "inspect branch history and merge state" |
| Merge：仓库损坏 | `LBR-REPO-002` | 128 | "inspect repository state and object integrity" |
| Merge：读取失败 | `LBR-IO-001` | 128 | "check repository metadata and permissions" |
| Merge：写入失败 | `LBR-IO-002` | 128 | "check filesystem permissions and retry" |

### Phase 详情

当子操作失败时，错误 JSON 会在 details 对象中包含 `phase` 键（`"fetch"`、`"merge"` 或 `"rebase"`），以便代理区分失败阶段。
