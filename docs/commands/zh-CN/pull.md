# `libra pull`

从远程获取对象，并将获取到的分支集成到当前分支。

## 概要

```text
libra pull [--ff-only] [--ff] [--no-ff] [--squash] [--no-commit] [--commit] [--autostash] [--no-progress] [--rebase] [--no-rebase] [--depth <n>] [<repository> [<refspec>]]
```

## 说明

`libra pull` 组合了 `fetch` 和 `libra merge` 使用的同一合并引擎。它下载新对象，更新远程跟踪引用，然后将选中的 upstream 集成到当前分支。

使用 `--rebase`（`-r`）时，集成步骤会改为在获取到的 upstream tip 之上重放仅本地提交。这等价于 `libra fetch` 后跟 `libra rebase <upstream>`。

使用 `--ff-only` 时，pull 会获取 upstream，但在本地和远程历史已经分叉时拒绝创建合并提交。快进和已经最新的 pull 仍会成功。`--ff-only` 与 `--rebase`、`--ff` 和 `--no-ff` 冲突；与 Git 一样，它可以和 `--squash`、`--no-commit` 或 `--commit` 组合。

使用 `--no-ff` 时，即使 upstream 可以快进，pull 也会记录一个真实的 merge commit，对齐 `git pull --no-ff`。`--ff` 显式允许快进，并覆盖本次调用中的 `pull.ff`。`--ff`、`--no-ff` 和 `--ff-only` 互斥且都与 `--rebase` 冲突，但都可以和 `--commit` 组合。

仅合并标志（`--ff-only`、`--ff`、`--no-ff`、`--squash`、`--no-commit`、`--commit`）即使配置了 `pull.rebase` 或 `branch.<name>.rebase`，也会选择 merge 路径。互相矛盾的显式合并标志会在 fetch 前被拒绝。

未传命令行集成标志时，Libra 会按本地、全局、系统配置的顺序读取 Git 风格的 pull 默认值（变量名不区分大小写）：`branch.<name>.rebase` 覆盖 `pull.rebase`，`pull.ff` 接受 `true`、`false` 或 `only`。本地和全局的加密值会先解密再校验。Git 的 `pull.rebase=merges`/`interactive`（以及 `m`/`i`）会被识别为不支持的模式，并以可操作的 `LBR-CLI-002` 诊断拒绝。命令行标志仍优先于配置。空值或其他无效的本地/全局配置会在 fetch 或集成前以 `LBR-CLI-002` 失败；本地/全局配置读取失败以 `LBR-IO-001` 失败。不可读或不支持的 system 配置 scope 会跳过，继续尝试低优先级默认值或内置 merge 行为。

不带参数调用时，命令读取当前分支 tracking 配置（`branch.<name>.remote` 和 `branch.<name>.merge`）。只给出 `<repository>` 时，当前分支名会被用作远程分支。同时给出 `<repository>` 和 `<refspec>` 时，会获取并合并指定远程分支。

Pull 支持 already-up-to-date、fast-forward 和 single-head three-way merge 结果。如果本地和远程分支冲突，pull 会返回由 merge 拥有的 `LBR-CONFLICT-002` 错误，带有 `phase: "merge"`，并留下与 `libra merge` 相同的 merge 状态。使用 `libra add <path>` 解决冲突并运行 `libra merge --continue`，或运行 `libra merge --abort`。

`pull` 已支持 `--ff-only`、`--ff`、`--no-ff`、`--squash`、`--no-commit`、`--commit`、`--autostash`、`--no-progress`、`--rebase`、`--no-rebase` 与 fetch `--depth`；尚不支持 octopus merge 与自定义合并策略（`--strategy`/`-X`）。`--commit` 只与 `--squash`、`--rebase` 冲突，并与 `--no-commit` 按命令行最后出现者生效；它不会自行强制 merge commit 或覆盖快进策略。`--depth` 要求 upstream 能协商 shallow boundary；本地 Libra upstream 会在集成前以 `LBR-REPO-002` fail-closed。`--no-progress` 把进度抑制转发给 fetch，抑制其 “Receiving objects” 进度条。`--autostash` 在集成前 stash 已跟踪改动、之后再 pop 回（即使整合失败也会 pop），让 `pull` 能在脏工作树上运行；未跟踪/忽略文件保持原样，pop 冲突时保留 stash 并报错（用 `libra stash pop` 恢复）。

## 全局配置 Schema 保护

`libra pull` 在信任远端 / tiered 对象存储设置前，会读取全局存储配置（`~/.libra/config.db`，或 `LIBRA_CONFIG_GLOBAL_DB` 指定的路径）。如果该数据库的 schema 版本比当前二进制支持的版本更新，pull 会以 `LBR-CONFIG-001` fail-closed，而不是静默忽略全局存储配置并回退到本地对象。诊断会包含二进制路径和版本、配置 DB 路径、schema 版本，以及升级命令：
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`。

只有在明确希望本地对象访问时，才使用 `libra --offline pull ...` 或 `LIBRA_READ_POLICY=offline|local libra pull ...`。Libra 会告警一次，并在本次运行中忽略全局存储配置。

## 选项

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `<repository>` | 要从中 pull 的远程名称。省略时使用当前分支已配置的 upstream。 | `libra pull origin` |
| `<refspec>` | 远程上的分支名。需要 `<repository>`。省略时使用当前分支名。 | `libra pull origin main` |
| `--ff-only` | 拒绝创建合并提交；仅 fast-forward 或 already-up-to-date pull 会成功。与 `--rebase`、`--ff` 和 `--no-ff` 冲突。 | `libra pull --ff-only` |
| `--ff` | 显式允许快进合并，覆盖 `pull.ff=false|only`。与 `--no-ff`、`--ff-only` 和 `--rebase` 冲突。 | `libra pull --ff` |
| `--no-ff` | 即使可以快进也总是创建 merge commit。与 `--ff`、`--ff-only` 和 `--rebase` 冲突。 | `libra pull --no-ff` |
| `--squash` | 暂存合并后的树，但不提交也不移动 `HEAD`，结果留给普通 `libra commit`。与 `--no-commit`、`--rebase` 冲突。 | `libra pull --squash` |
| `--no-commit` | 合并并暂存，但提交前停止，记录 merge state 以便用 `libra merge --continue` 完成。与 `--squash`、`--rebase` 冲突。 | `libra pull --no-commit` |
| `--commit` | 提交 merge 结果；与 `--no-commit` 最后出现者生效，且不覆盖快进策略。与 `--squash`、`--rebase` 冲突。 | `libra pull --commit` |
| `--autostash` | 集成前 stash 已跟踪工作树改动，之后再应用回来（即使失败也会尝试），让脏工作树也能 pull。未跟踪/忽略文件保持原样。 | `libra pull --autostash` |
| `--no-progress` | 抑制 fetch 进度条（“Receiving objects” spinner），对齐 `git pull --no-progress`。 | `libra pull --no-progress` |
| `--notes` | 转发给 fetch：从本地 Libra upstream 额外导入文件依赖图（`refs/notes/deps`，lore.md 3.2）。默认关闭；网络或普通 Git upstream 会告警且不导入。见 `libra fetch --notes`。 | `libra pull --notes` |
| `--depth <n>` | 将 fetch 阶段限制为每个 tip 的 `n` 个提交。与 `--rebase` 冲突；本地 Libra upstream 因不能声明 shallow boundary 会以 `LBR-REPO-002` fail-closed。 | `libra pull --depth 1` |
| `-r`, `--rebase` | 获取后，将当前分支 rebase 到 upstream tip，而不是合并。 | `libra pull --rebase` |
| `--no-rebase` | 合并而非 rebase，撤销先前的 `--rebase`/`-r`，并覆盖本次调用中的 `pull.rebase`（最后出现者生效）。 | `libra pull --no-rebase` |
| `--json` | 向 stdout 输出结构化 JSON 信封（全局标志）。 | `libra pull --json` |
| `--machine` | 紧凑单行 JSON；抑制进度（全局标志）。 | `libra pull --machine` |
| `--quiet` | 抑制所有进度和合并摘要输出。 | `libra pull --quiet` |

## 仓库 hooks

集成阶段使用所选操作的同一套 `.libra/hooks` 生命周期。merge 模式运行 merge
hooks；自动 merge commit 也运行消息 hooks。rebase 模式在 fetch 后、本地历史移动
前运行 blocking `pre-rebase <upstream>`，成功重写后运行 advisory
`post-rewrite rebase`。pull 没有专用 `--no-verify`；只有评估策略影响后才设置
`LIBRA_NO_HOOKS=1`。quiet、JSON 与 machine 输出会抑制嵌套 hook 的 stdout/stderr。
详见[仓库 hooks](repository-hooks.md)。

## 示例

```bash
libra pull
libra pull origin main
libra pull --ff-only
libra pull --depth 1
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
- 根据 CLI 标志和 `pull.rebase`/`branch.<name>.rebase` 默认值合成出的有效集成模式，`merge` 或 `rebase` 中恰好出现一个。因此即使命令行没有 `--rebase`，配置 `pull.rebase=true` 也可能让 JSON 出现 `rebase` 对象。
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
| Rebase 配置默认值 | 未传 CLI rebase 标志时，`branch.<name>.rebase` 覆盖 `pull.rebase` | 相同 | N/A |
| 强制合并提交 | `libra pull --no-ff` | `git pull --no-ff` | N/A |
| 快进配置默认值 | 未传 CLI 快进标志时，`pull.ff=true|false|only` 生效 | 相同 | N/A |
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
| `pull.rebase`、`branch.<name>.rebase` 或 `pull.ff` 配置值无效 | `LBR-CLI-002` | 129 | "libra config <key> <value>" |
| 不支持的 `pull.rebase=merges|interactive` 模式 | `LBR-CLI-002` | 129 | 使用布尔 rebase 或显式的受支持 pull 标志 |
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
