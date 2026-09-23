# `libra pull`

从远程获取对象，并将获取到的分支集成到当前分支。

## 概要

```text
libra pull [--ff-only] [--ff] [--no-ff] [--squash] [--no-commit] [--commit] [--autostash] [--no-progress] [--rebase] [--no-rebase] [--depth <n>] [<repository> [<refspec>]]
```

## 说明

`libra pull` 组合了 `fetch` 和 `libra merge` 使用的同一合并引擎。它下载新对象，更新远程跟踪引用，然后将选中的 upstream 集成到当前分支。远程 URL 指向 Git v2 bundle 时，每次 pull 都会重新读取该文件，替换 bundle 后可以快进当前分支。

使用 `--rebase`（`-r`）时，集成步骤会改为在获取到的 upstream tip 之上重放仅本地提交。这等价于 `libra fetch` 后跟 `libra rebase <upstream>`。

使用 `--ff-only` 时，pull 会获取 upstream，但在本地和远程历史已经分叉时拒绝创建合并提交。快进和已经最新的 pull 在集成预检允许时仍可成功；未解决的索引仍会阻塞 merge 阶段。`--ff-only` 与 `--rebase`、`--ff` 和 `--no-ff` 冲突；与 Git 一样，它可以和 `--squash`、`--no-commit` 或 `--commit` 组合。

使用 `--no-ff` 时，即使 upstream 可以快进，pull 也会记录一个真实的 merge commit，对齐 `git pull --no-ff`。`--ff` 显式允许快进，并覆盖本次调用中的 `pull.ff`。`--ff`、`--no-ff` 和 `--ff-only` 互斥且都与 `--rebase` 冲突，但都可以和 `--commit` 组合。

仅合并标志（`--ff-only`、`--ff`、`--no-ff`、`--squash`、`--no-commit`、`--commit`）即使配置了 `pull.rebase` 或 `branch.<name>.rebase`，也会选择 merge 路径。互相矛盾的显式合并标志会在 fetch 前被拒绝。

未传命令行集成标志时，Libra 会按本地、全局、系统配置的顺序读取 Git 风格的 pull 默认值（变量名不区分大小写）：`branch.<name>.rebase` 覆盖 `pull.rebase`，`pull.ff` 接受 `true`、`false` 或 `only`。本地和全局的加密值会先解密再校验。Git 的 `pull.rebase=merges`/`interactive`（以及 `m`/`i`）会被识别为不支持的模式，并以可操作的 `LBR-CLI-002` 诊断拒绝。命令行标志仍优先于配置。空值或其他无效的本地/全局配置会在 fetch 或集成前以 `LBR-CLI-002` 失败；本地/全局配置读取失败以 `LBR-IO-001` 失败。不可读或不支持的 system 配置 scope 会跳过，继续尝试低优先级默认值或内置 merge 行为。

不带参数调用时，命令读取当前分支 tracking 配置（`branch.<name>.remote` 和 `branch.<name>.merge`）。已配置的本地 upstream（`branch.<name>.remote=.`，由 `libra branch -u <本地分支>` 写入）会在任何网络或 `FETCH_HEAD` 写入前被拒绝（`LBR-CLI-003`，退出 129）；Git 2.54 的 `fetch`/`pull` 可以对本地 upstream 操作——该支持延后到 [issues/480 HP-16](https://github.com/libra-tools/libra/issues/480)。显式仓库参数 `.` 仍走现有的 `remote '.' not found`。只给出 `<repository>` 时，当前分支名会被用作远程分支。同时给出 `<repository>` 和 `<refspec>` 时，会获取并合并指定远程分支。

pull 的 merge 阶段开始前继承 `libra merge` 的索引检查。没有 merge 状态但索引仍有未解决条目时（例如冲突 squash 后），即使获取到的目标已最新，也会以 `LBR-CONFLICT-002` 拒绝（退出 128，`phase: "merge"`）。该阶段保持 HEAD、索引和工作树原样，提示先解决冲突、用 `libra add` 暂存，再运行普通 `libra commit`。已有 merge 状态仍使用 `merge --continue` / `--abort` 提示。拒绝发生前 fetch 可能已经下载对象并更新远程跟踪引用，因此这不是整个 pull 的零写入保证。

Pull 支持 already-up-to-date、fast-forward 和 single-head three-way merge 结果——包括拥有多个 merge base 的交叉合并历史：共享的 merge 引擎会像 `libra merge` 一样通过递归虚拟祖先解决（见其「交叉合并历史」一节；嵌套超过 20 层、或同一层超过 32 个 merge base 的历史以 `LBR-UNSUPPORTED-001` 拒绝）。如果本地和远程分支冲突，pull 会返回由 merge 拥有的 `LBR-CONFLICT-002` 错误，带有 `phase: "merge"`，除 `--squash` 外留下与 `libra merge` 相同的 merge 状态。非 squash 冲突在解决文件、用 `libra add <path>` 暂存后运行 `libra merge --continue`，或运行 `libra merge --abort`。squash 冲突则只留下未解决的索引条目而不记录 merge 状态，解决、暂存后用普通 `libra commit` 创建单亲提交；`merge --continue`、`--abort`、`--restart` 均报 `no merge in progress`。 重命名同样按 `libra merge` 的方式逐侧检测（见其「重命名」一节）：远端改名而本地修改过的文件会在新路径上合并，`merge.renames` 与 `merge.renameLimit` 在此同样生效。路径级改名冲突也原样继承——`CONFLICT (rename/rename)`、`CONFLICT (rename/delete)` 与 `CONFLICT (rename involved in collision)` 会在冲突错误之前打印于人读输出（`--json`/`--machine` 下 stdout 保持机器可读），而两侧把同一文件改名到同一路径则是干净合并而非冲突。目录/文件冲突按 `libra merge` 的方式处理（见其「目录/文件冲突」一节）：目录保留路径，文件移到 `<path>~HEAD` / `<path>~<branch>`，人读输出会在冲突错误之前打印 Git 的 `CONFLICT (file/directory)` 提示行（`--json`/`--machine` 下 stdout 保持机器可读，冲突只由错误信封承载）。

pull 的 merge 阶段也继承 `libra merge` 的目录改名推断和 `merge.directoryRenames=true|false|conflict`。默认 `conflict` 会建议并暂存迁移后的未解决路径；`true` 自动迁移；`false` 把新增路径留在旧目录名下。目标分裂会让合并停下，但受影响新增路径仍是 stage 0。未知值在 merge 阶段以 `LBR-REPO-003` fail-closed；在这次阶段性拒绝前，fetch 可能已经更新远程跟踪引用。

merge 路径也继承 `merge.renormalize=true|false`。启用时，共享三路引擎先根据
`text` / `eol` 属性 canonicalize base、本地和远端输入，再执行内容合并，
结果保留本地（ours 侧）行尾。pull 不公开 `-X renormalize|no-renormalize`
覆盖；请使用仓库配置。无效值只在 pull 真正进入三路合并时以
`LBR-REPO-003` 失败；此前 fetch 可能已更新对象与远程跟踪引用。
fast-forward 与 already-up-to-date 不读取该配置。任意 clean/smudge filter
仍不支持。

`pull` 已支持 `--ff-only`、`--ff`、`--no-ff`、`--squash`、`--no-commit`、`--commit`、`--autostash`、`--no-progress`、`--rebase`、`--no-rebase` 与 fetch `--depth`；尚不支持 octopus merge 与自定义合并策略（`--strategy`/`-X`）。`--commit` 只与 `--squash`、`--rebase` 冲突，并与 `--no-commit` 按命令行最后出现者生效；它不会自行强制 merge commit 或覆盖快进策略。`--depth` 要求 upstream 能协商 shallow boundary；本地 Libra upstream 以 `LBR-REPO-002` fail-closed（已决终态，见开发兼容登记 D20）。`--no-progress` 把进度抑制转发给 fetch，抑制其 “Receiving objects” 进度条。`--autostash` 在集成前 stash 已跟踪改动、集成结束时再应用回来，让 `pull` 能在脏工作树上运行——「结束」因路径而异：**rebase** 路径即使失败也会恢复；**merge** 路径沿用 `libra merge` 自己的 autostash，干净结果或前置失败时立即应用，非 squash 冲突合并进行中则**持有**（JSON `autostash: "kept"`），由 `libra merge --continue` / `--abort` 应用回来，绝不丢失；冲突 squash 则直接把 autostash 保存进 stash list，不执行回贴，保留未解决的索引和工作树，解决、提交后再用 `libra stash pop` 恢复；保存失败时警告并保留 held sidecar 引用。未跟踪/忽略文件保持原样，应用冲突时提升入 stash list 并报错（用 `libra stash pop` 恢复）。

## 全局配置 Schema 保护

配置 schema 兼容性按角色判定。`libra pull` 在信任配置前，以只读方式检查 GlobalConfig 与 SystemConfig 元数据。真正的配置 future schema，或未注册／名称不匹配的迁移 receipt，在命令需要该作用域时以 `LBR-CONFIG-001` fail-closed。当前 manifest 已知的 Repository-only receipt（包括 `2026090801`）不会使配置库被误判为 future，受支持的配置值仍可读取。本 build 能识别 configuration-owned legacy-reader barrier；详见[配置兼容性](config.md#配置-schema-兼容性)。

全局路径为 `LIBRA_CONFIG_GLOBAL_DB` 或 XDG 配置目录（`$XDG_CONFIG_HOME/libra/config.db`，默认 `<home>/.config/libra/config.db`），在自动迁移前回退到 legacy `<home>/.libra/config.db`；系统路径为 `LIBRA_CONFIG_SYSTEM_DB` 或 `/etc/libra/config.db`。完整的进程环境／repo-local 存储设置可以证明无需 GlobalConfig（`cloud` 还须满足 D1 设置），但不能证明无需 SystemConfig 默认值。诊断只说明受影响的 scope、ledger 与版本，不输出配置值或未信任 receipt 名称。

本阶段对未知／不支持的状态只有升级路径，不执行自动修复。安装兼容的较新 Libra：
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`。
禁止手工删除或修改 SQLite receipt。仅在明确需要本地对象访问时使用 `--offline` 或 `LIBRA_READ_POLICY=offline|local`；这些模式会告警，并不授权远端同步。

## 选项

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `<repository>` | 要从中 pull 的远程名称。省略时使用当前分支已配置的 upstream。 | `libra pull origin` |
| `<refspec>` | 远程上的分支名。需要 `<repository>`。省略时使用当前分支名。 | `libra pull origin main` |
| `--ff-only` | 拒绝创建合并提交；仅允许 fast-forward 或 already-up-to-date 集成；未解决的索引仍会拒绝 merge 阶段。与 `--rebase`、`--ff` 和 `--no-ff` 冲突。 | `libra pull --ff-only` |
| `--ff` | 显式允许快进合并，覆盖 `pull.ff=false|only`。与 `--no-ff`、`--ff-only` 和 `--rebase` 冲突。 | `libra pull --ff` |
| `--no-ff` | 即使可以快进也总是创建 merge commit。与 `--ff`、`--ff-only` 和 `--rebase` 冲突。 | `libra pull --no-ff` |
| `--squash` | 暂存合并后的树，但不提交、不移动 `HEAD`、不记录 merge 状态，即使冲突也如此。解决并暂存冲突后用普通 `libra commit` 创建单亲提交，merge 控制动作不可用。与 `--no-commit`、`--rebase` 冲突。 | `libra pull --squash` |
| `--no-commit` | 合并并暂存，但提交前停止，记录 merge state 以便用 `libra merge --continue` 完成。与 `--squash`、`--rebase` 冲突。 | `libra pull --no-commit` |
| `--commit` | 提交 merge 结果；与 `--no-commit` 最后出现者生效，且不覆盖快进策略。与 `--squash`、`--rebase` 冲突。 | `libra pull --commit` |
| `--autostash` | 集成前 stash 已跟踪工作树改动，集成结束时再应用回来（rebase：即使失败也恢复；merge：非 squash 冲突时持有，直到 `merge --continue`/`--abort`；冲突 squash：直接保存进 stash list，解决并提交后 `stash pop`），让脏工作树也能 pull。未跟踪/忽略文件保持原样。 | `libra pull --autostash` |
| `--no-progress` | 抑制 fetch 进度条（“Receiving objects” spinner），对齐 `git pull --no-progress`。 | `libra pull --no-progress` |
| `--notes` | 转发给 fetch：从本地 Libra upstream 额外导入文件依赖图（`refs/notes/deps`，lore.md 3.2）。默认关闭；网络或普通 Git upstream 会告警且不导入。见 `libra fetch --notes`。 | `libra pull --notes` |
| `--depth <n>` | 将 fetch 阶段限制为每个 tip 的 `n` 个提交。与 `--rebase` 冲突；本地 Libra upstream 因不能声明 shallow boundary 以 `LBR-REPO-002` fail-closed（已决终态，D20）。 | `libra pull --depth 1` |
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
| 已配置本地 upstream（`branch.<name>.remote=.`） | `LBR-CLI-003` | 129 | 用 `libra branch --unset-upstream` 清除；网络支持见 issues/480 HP-16 |
| `pull.rebase`、`branch.<name>.rebase` 或 `pull.ff` 配置值无效 | `LBR-CLI-002` | 129 | "libra config <key> <value>" |
| 不支持的 `pull.rebase=merges|interactive` 模式 | `LBR-CLI-002` | 129 | 使用布尔 rebase 或显式的受支持 pull 标志 |
| `merge.renames` / `merge.renameLimit` / `merge.directoryRenames` / `merge.renormalize` 配置值无效（继承自 `libra merge`） | `LBR-REPO-003` | 128 | 把对应 merge 配置设为支持值或删除 |
| Fetch：网络不可达 / 超时 | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Fetch：封包读取连接重置 / 非协议 IO 故障 | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Fetch：认证失败 | `LBR-AUTH-001` | 128 | "check SSH key or HTTP credentials" |
| Fetch：pkt-line discovery / 传输建立错误 | `LBR-NET-002` | 128 | "check that the remote serves Git data and that a proxy has not altered the response" |
| Fetch：pkt-line 截断 / sideband / checksum / pack 协议错误 | `LBR-NET-002` | 128 | 无额外提示；不完整 pack 除外："the connection dropped mid-transfer — retry the pull" |
| Merge：非 squash 冲突、脏工作树或未跟踪覆盖 | `LBR-CONFLICT-002` | 128 | "resolve conflicts, then run 'libra merge --continue'" |
| Merge：无 merge 状态的未解决索引，或新发生的 squash 冲突 | `LBR-CONFLICT-002` | 128 | "resolve conflicts, stage the resolved paths with 'libra add', then run 'libra commit'" |
| Merge：`--ff-only` 拒绝非快进 | `LBR-CONFLICT-002` | 128 | "run 'libra pull' without --ff-only to allow a merge commit" |
| Merge 或 rebase 需要裁决 `160000` gitlink（submodule） | `LBR-UNSUPPORTED-001` | 128 | 在 Libra 之外解决 submodule 指针，或移除该 gitlink 条目——拒绝发生在 autostash 与任何索引/工作树写入之前（见 `docs/commands/zh-CN/merge.md`） |
| Merge：递归虚拟祖先嵌套超过 20 层或同层超过 32 个 base | `LBR-UNSUPPORTED-001` | 128 | 先把两条分支的共同祖先合并到一起，或改用 `--rebase` pull（见 `docs/commands/zh-CN/merge.md`） |
| Rebase：重放期间冲突 | `LBR-CONFLICT-001` | 128 | "resolve conflicts, stage them, then run 'libra rebase --continue'" |
| Rebase：脏工作树 | `LBR-REPO-003` | 128 | "commit or stash your changes before rebasing" |
| Merge：无效目标 | `LBR-CLI-003` | 129 | "verify the upstream ref and try again" |
| Merge：无关历史或无效 merge 状态 | `LBR-REPO-003` | 128 | "inspect branch history and merge state" |
| Merge：仓库损坏 | `LBR-REPO-002` | 128 | "inspect repository state and object integrity" |
| Merge：读取失败 | `LBR-IO-001` | 128 | "check repository metadata and permissions" |
| Merge：写入失败 | `LBR-IO-002` | 128 | "check filesystem permissions and retry" |

### Phase 详情

当子操作失败时，错误 JSON 会在 details 对象中包含 `phase` 键（`"fetch"`、`"merge"` 或 `"rebase"`），以便代理区分失败阶段。

## 畸形 HTTP(S) discovery 响应

在 HTTP(S) 引用发现（discovery）期间，Libra 会拒绝零字节广告和畸形
pkt-line 帧，包括不完整或非十六进制标头、小于四的帧长度以及截断的 payload。
合法的 `0000` flush 与未收到响应有明确区别；合法的空仓库广告仍受支持。
不支持的 object-format capability 使用固定错误消息
`Unsupported object format capability`，不回显远端提供的值。
请确认 URL 指向 Git smart HTTP 服务，并检查代理是否截断或替换了响应，然后重试。

## pkt-line 错误归类

检测到的 pkt-line 帧格式错误返回 `LBR-NET-002`（退出码128），包括空的 HTTP(S)
discovery 广告。普通连接失败、连接重置和超时返回 `LBR-NET-001`（退出码128）。
协议错误发生时请核对 Git 服务及代理响应。discovery 帧错误的提示为
`check that the remote serves Git data and that a proxy has not altered the response`。

fetch 阶段的 discovery 与对象传输建立使用此归类。读取流时的标头或 payload 截断返回
`LBR-NET-002`，无额外 CLI 提示；在完整帧边界结束但 pack 尚未完整时，保留字节数及
`the connection dropped mid-transfer — retry the pull` 提示。封包读取中的普通连接重置
返回 `LBR-NET-001`。这些错误的 JSON 仍保留 `details.phase = "fetch"`。

在 pack 数据开始前，upload-pack 流于帧边界遇到 EOF（包括零字节 POST 响应）
返回 `LBR-NET-001`，提示为 `check network connectivity and retry`。
空的 discovery 广告仍返回 `LBR-NET-002`。

## Git 与 SSH 广告帧边界

Git/SSH pkt-line 广告读取器拒绝声明长度 `0001` 至 `0003`、不完整的四字节标头，
以及声明 payload 内的截断 EOF。flush `0000`、空 payload `0004` 和最大 `ffff`
帧保持既有行为。读取边界的 typed pkt-line 错误经分类得到 `LBR-NET-002`；
普通传输 IO 和 idle 超时经分类仍为 `LBR-NET-001`。

在 `git://` 取对象前的 advertisement 阶段，fetch、clone 和 pull 已将这些错误
报告为 `LBR-NET-002`，包括零字节广告；提示为
`check that the remote serves Git data and that a proxy has not altered the response`。
长度 1–3 之前可能触发 panic；截断广告之前返回 `LBR-NET-001` 与网络/传输提示。
Git discovery 现在保留协议错误分类；SSH 广告透传与有界清理见下节。
所有读取点均要求四位 ASCII 十六进制标头。

该广告阶段不同于协商后的 upload-pack 响应：HTTP(S) discovery 广告帧分类和空 upload-pack 响应
分类不变。测试已覆盖真实本机 TCP 取对象广告读取及公开 fetch/clone/pull 错误转换，
不代表完整命令执行或有界 SSH 清理。畸形帧应检查远端 Git 服务或代理。

## SSH 广告错误处理

SSH advertisement 长度 `0001` 至 `0003`、不完整标头（包括零字节 EOF）及截断
payload 返回 `LBR-NET-002`。固定协议原因与 marker 保留，不插入捕获的 SSH
stdout/stderr。

必需标头不完整时有一项主机信任例外：本地 SSH 退出码为255，且 stderr 前64 KiB
包含受识别的 host-key 诊断时，返回固定主机核验指引与 `LBR-NET-001`。这项分类
本身不验证远端指纹。其它缺失广告（含认证失败）仍用 `LBR-NET-002`；能够观察到
非零本地退出状态时，追加 `SSH exited with status N` 与固定连接、可信主机、
ssh-agent 及仓库访问指引，不显示原始 SSH 诊断。

必需标头不完整时最多用100毫秒观察 SSH 退出状态，再按需请求终止；其它读取
错误立即请求终止。状态观察、直接子程序回收及输出收集共用两秒清理截止时间。
协议错误与带类型的主机信任错误优先于次要清理警告。普通 IO/超时保留传输错误
分类，可追加固定本地清理警告。终止程序可能改变观察到的退出状态；这不承诺
回收任意后代程序。

Clone 将主机核验指引放在结构化 hints 中；其它命令边界在 message 中保留固定
主机指引，并沿用 `LBR-NET-001` 的网络 hint。human、JSON 与 machine 诊断均
不包含捕获的远端 stderr 原文。

`git://` discovery 与取对象阶段均将上述帧错误归为 `LBR-NET-002`；所有异步
读取点拒绝非 ASCII/非十六进制标头并给出固定协议原因，HTTP(S) discovery 广告帧校验不变。

## SSH 认证与捕获诊断

无论是否由终端调用，Libra 都以 `BatchMode=yes` 启动 SSH，不在 Libra 命令中
询问私钥口令或进行交互式主机信任决定。重试前请先在 `ssh-agent` 中加载或解锁
加密私钥。主机信任应先通过可信服务商控制台或其它可信渠道核对指纹，再手动
更新 `~/.ssh/known_hosts`；也可以单独建立交互 SSH 连接，核对显示的指纹后才
接受。`ssh -T git@github.com` 是 GitHub 示例，请使用实际仓库 SSH 用户、主机
和端口，不要接受未经核验的指纹。

`ssh.strictHostKeyChecking` 保留既有 `ask`、`yes`、`accept-new`、`no` 设置。
`ask` 不向 SSH 传递该选项，由用户 SSH 配置决定；`BatchMode=yes` 仍禁止
交互决定。显式设置会转交 SSH，请按仓库需求选择主机信任策略。

SSH stderr 在终端会话中也始终捕获，从子程序启动时便持续读取，最多保留64 KiB，
其余字节继续计数并计算摘要。用户错误只含固定文字与可用的本地退出状态，不
打印或记录远端 stderr 原文。debug 诊断只含退出状态、总字节数、保留字节数及
已收集字节流的 SHA-256；收集失败或取消时可能没有这些元数据，不声称已有完整
摘要。摘要计算的工作量与实际读取字节数成正比。

SSH 引用广告与 receive-pack 响应各有16 MiB累计上限。广告超限以 `LBR-NET-001`
失败并提示在服务可用时使用仓库的 HTTPS URL，否则请维护者减少引用；push 响应超限以 `LBR-NET-001` 失败并提示减少推送
引用，绝不把截断响应视为成功。极大的引用集或更新可能受到影响；流式 fetch
pack 不受该上限约束。push 响应失败不代表服务端回滚了引用，重试前应检查远端
实际状态。既有 IO 超时仍然生效。

完整 discovery 广告之后，Libra 最多给 SSH 100毫秒退出，再请求终止；共用两秒
清理截止时间。捕获任务在所属操作退出或截止时间到期时取消，也覆盖后代程序
继续持有管道的情况。

### SSH 主机身份变更与诊断收集

SSH 报告主机身份已经变更时，Libra 保留独立的固定警告：可能发生拦截，也可能是合法密钥轮换。必须先通过可信渠道核对新指纹，才可替换 `~/.ssh/known_hosts` 中的旧条目；不要绕过主机密钥检查。此情况与首次未信任主机均使用 `LBR-NET-001`，但消息与操作提示不同。

如果 stderr 管道在有界收集期限后仍未关闭，已取得的完整协议输出及本地退出状态仍可使用；不会仅因诊断收集失败而拒绝完整传输。已观察到的非零退出状态及主要读取错误仍会导致失败。缺失的诊断仅记录固定的 debug 提示，不伪造空流字节数或摘要；stdout 收集或进程等待失败仍按原错误处理。

### SSH 上限与主机分类边界

上述16 MiB广告与receive-pack响应累计上限仅适用于Libra的SSH传输。
HTTPS和Git传输没有这一特定上限。若服务器提供HTTPS端点，SSH广告超限时可改用
该仓库的HTTPS远端URL；只读用户无需修改服务器引用。否则请仓库维护者减少广告中的
引用集合。流式fetch pack仍不受此累计上限约束。

主机信任分类必须同时满足：首个必需标头未完成、未观察到任何stdout字节、本地退出码255，
以及保留stderr前缀中的已知模式。一旦读到任何stdout字节（包括部分标头），
类似主机密钥错误的stderr不能触发特定信任指引；完整广告之后的失败保留固定通用诊断。
广告之前的模式仍只是诊断启发式，不等于指纹核验。

成功discovery的子程序若等待请求，通常会耗尽100 ms原生退出观察窗口，每次discovery
分别承担该成本。这与两秒直接子程序清理预算分开，不构成性能基准或任意后代清理保证。

## 严格 pkt-line 标头

pkt-line 标头必须恰好包含四位 ASCII 十六进制数字（`0`–`9`、`a`–`f` 或 `A`–`F`）。
fetch 流、`git://` 广告与 SSH 广告均拒绝 `+004` 等带符号标头、空白、非十六进制文字
及无效 UTF-8，并返回 `LBR-NET-002`（退出128）。原因固定，不回显标头或 payload。
此前发送带符号或其它不合规标头的对端，需要改为四位十六进制数字后重试。

Git discovery 对长度 `0001`–`0003`、缺失/不完整的必需标头及截断 payload 也保留
协议分类，并传递至 clone、fetch、pull、ls-remote 与 push。请检查远端 Git 服务或
代理响应。结构化错误字段保持原有格式；push 保留自己的协议 hint，其它命令亦然。

flush `0000`、空数据 `0004` 与最大长度 `ffff` 的语义不变；普通网络错误及超时保留
原有分类。尚无完整 pack 时的空 fetch 数据流仍属于网络失败，完整 pack 之后的 EOF
保留成功语义。上文 SSH 主机信任例外、捕获上限及清理截止时间保持不变。

## 空仓库 discovery 的帧校验

HTTP(S) 广告声明仓库为空后，仍会校验剩余的全部 pkt-line 帧。零 object ID 之后
出现畸形标头、小于四的帧长度或截断 payload 时，返回 `LBR-NET-002`（退出128），
原因固定且不回显远端字节，不再误报为空仓库成功。重试前请核对远端 Git 服务或代理
响应。合法空仓库、支持的 SHA-1/SHA-256 广告、既有命令 hint 和结构化错误字段保持
原有行为。

## Issue #477 notes

拒绝对本地 upstream（`remote=.`）执行
