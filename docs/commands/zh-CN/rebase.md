# `libra rebase`

在另一个 base tip 之上重新应用提交。

**别名：** `rb`

## 概要

```
libra rebase [--autosquash] [--reapply-cherry-picks] [--autostash] [--exec <cmd>] [--update-refs] [--fork-point] [--no-rerere-autoupdate] [--keep-empty | --no-keep-empty] [--empty=<mode>] <upstream>
libra rebase --onto <newbase> <upstream> [<branch>]
libra rebase --continue
libra rebase --abort
libra rebase --skip
```

## 说明

`libra rebase` 将当前分支上的一系列提交移动到新的 base 提交之上。它会找到当前分支与指定 upstream 之间的共同祖先，收集从该祖先到当前 HEAD 的所有提交，并在 upstream 分支之上重放每个提交。所有提交重放后，当前分支引用会更新为指向最终 rebased 提交。

如果重放期间发生冲突，rebase 会停止并报告冲突文件。用户手动解决冲突、暂存已解决文件，然后运行 `libra rebase --continue` 继续。或者，`--abort` 会恢复原始分支状态，`--skip` 会丢弃当前提交并继续下一个。

`--autostash` 会在重放前把 tracked index/worktree 变更保存为 held stash，并在成功或中止后分别恢复 staged index 层与 unstaged worktree 层。可重复的 `--exec <cmd>` 会在每个重放提交后通过 Libra 强制 workspace-write、禁网 sandbox 依次执行；失败会停止序列，`--continue` 重试失败命令。exec 失败后的 `--skip` 保留已重放提交并跳过该提交剩余命令。`--update-refs` 原子重定向重写区间中的其他本地分支，但排除任何 worktree 已检出的分支。`--fork-point` 从 upstream reflog 选择仍是 `HEAD` 祖先的最具体旧 tip，找不到时回退普通 merge base。

Rebase 状态（剩余和已完成提交列表、原始 HEAD 和目标 base）持久化在 SQLite 数据库中。恢复关键的 autostash、exec 与 update-refs 元数据会原子且强制 fsync 到 `.libra/rebase-aux.json`，直到序列进入终态。旧 Libra 版本的 legacy file-based 状态会在首次访问时自动迁移到数据库。

## 选项

| 选项 | 长选项 | 说明 |
|--------|------|-------------|
| `<upstream>` | | 要 rebase 到的 upstream 分支或提交。除非指定 `--continue`、`--abort` 或 `--skip`，否则必需。可以是分支名、提交哈希或任何 Git 引用。 |
| | `--onto <newbase>` | 把 `<upstream>..HEAD` 区间重放到 `<newbase>`，而不是 `<upstream>`。 |
| | `--continue` | 在解决冲突后继续 rebase。与 `--abort`、`--skip` 和 `<upstream>` 互斥。 |
| | `--abort` | 中止当前 rebase，并将原始分支恢复到 rebase 前状态。与 `--continue`、`--skip` 和 `<upstream>` 互斥。 |
| | `--skip` | 跳过当前冲突提交；exec 失败时则保留已重放提交并跳过剩余命令。与 `--continue`、`--abort` 和 `<upstream>` 互斥。 |
| | `--autosquash` | 在重放时把 `fixup!`、`squash!`、`amend!` 提交移动并折叠到目标提交。 |
| | `--reapply-cherry-picks` | 显式重放 clean cherry-pick；这与 Libra 默认线性重放行为一致。 |
| | `--autostash` / `--no-autostash` | 重放前保存 tracked index/worktree 变更，分别保持 staged 与 unstaged 层，并在成功或中止后恢复。恢复冲突时先保存为 `stash@{0}` 并警告。最后一个 toggle 生效。 |
| | `--exec <cmd>` | 在每个重放提交后，通过强制 workspace-write、禁网 sandbox 执行可重复 shell 命令。非零退出或超时停止 rebase，`--continue` 重试。 |
| | `--update-refs` / `--no-update-refs` | 原子移动指向重写区间的其他本地分支；排除任何 worktree 已检出的分支。最后一个 toggle 生效。 |
| | `--fork-point` / `--no-fork-point` | 尽可能从 upstream reflog 选重放边界，否则使用普通 merge base。最后一个 toggle 生效。 |
| | `--no-rerere-autoupdate` | Git 兼容的接受式 no-op：启用时 rerere 记录已集成，但 rebase 不公开正向 `--rerere-autoupdate`；暂存行为遵循 `rerere.autoUpdate`。 |
| | `--keep-empty` | 保留 start-empty（重放前就为空）的提交而非丢弃。为 Git 兼容性接受的 no-op：Libra 的 rebase 默认就保留空提交。与 `--no-keep-empty` 组成 toggle，last-wins。 |
| | `--no-keep-empty` | 丢弃 start-empty 提交（其 tree 等于父 tree，未引入变更）而非重放。与 `--keep-empty` 组成 toggle。（此项控制*开始*就为空的提交；`--empty=<mode>` 控制 replay 后*变空*的提交。） |
| | `--empty=<mode>` | 如何处理 replay 后*变空*的提交（其变更已在新 base 上）：`drop` 跳过它（HEAD 不前进，并打印 `dropping <sha> <subject> -- patch contents already upstream`），`keep` 保留这个空提交。省略时 Libra **保留**——有意与 Git 不同（Git 默认 drop）；需要 Git 行为请用 `--empty=drop`。该模式会跨冲突 round-trip 到 `--continue`/`--skip`。Git 的 `stop`/`ask`（停下交由你决定）不支持（Libra 非交互 rebase 无 halt-on-empty 续作流）；它们与任何未知值均为用法错误（`LBR-CLI-002`，退出 129）。 |

### 选项细节

**`<upstream>`**

开始新的 rebase，将当前分支提交重放到指定 upstream 之上：

```bash
$ libra rebase main
Found common ancestor: abc1234
Rebasing 3 commits from `feature` onto `main`...
Applied: def5678 feat: add parser
Applied: 987abcd feat: add lexer
Applied: 13579bd test: add parser tests
Successfully rebased branch 'feature' onto '1234567'.
```

**`--continue`**

解决冲突并暂存已解决文件后，继续 rebase：

```bash
$ libra rebase --continue
Applied: 987abcd feat: add lexer
Rebasing 1 commits from `feature` onto `1234567`...
Applied: 13579bd test: add parser tests
Successfully rebased branch 'feature' onto '1234567'.
```

**`--abort`**

中止 rebase 并恢复原始分支状态：

```bash
$ libra rebase --abort
Rebase aborted. Restored branch 'feature'.
```

**`--skip`**

跳过当前冲突提交并移动到下一个：

```bash
$ libra rebase --skip
Skipped: 987abcd feat: add lexer
Rebasing 1 commits from `feature` onto `1234567`...
Applied: 13579bd test: add parser tests
Successfully rebased branch 'feature' onto '1234567'.
```

**`--autostash`、`--exec`、`--update-refs` 与 `--fork-point`**

```bash
# 在历史重写前后保存并恢复 tracked 本地变更
libra rebase --autostash main

# 每个重放提交后按顺序运行两个 sandbox 命令
libra rebase --exec 'cargo test' --exec 'cargo clippy' main

# 重定向重写区间内未检出的其他本地分支
libra rebase --update-refs main

# upstream force-move 后通过 reflog 恢复 fork point
libra rebase --fork-point origin/main
```

Exec 命令是用户可控 shell 输入。只有内部 sandbox 能强制 workspace-only 写入且禁止网络时 Libra 才会执行；若所需 backend 不可用，则以 `LBR-CONFLICT-002` fail-closed，并保留可续作的 rebase 状态。

## 常用命令

```bash
# 将当前分支 rebase 到 main
libra rebase main

# Rebase 到特定提交
libra rebase abc1234

# 保存 tracked 本地变更后 rebase
libra rebase --autostash main

# 每个重放提交后运行 sandbox 检查
libra rebase --exec 'cargo test' main

# 重定向重写区间内其他本地分支
libra rebase --update-refs main

# upstream force-move 后恢复 fork point
libra rebase --fork-point origin/main

# 解决冲突后继续
libra rebase --continue

# 中止 rebase
libra rebase --abort

# 跳过有问题的提交
libra rebase --skip

# 使用别名
libra rb main
```

## 人类可读输出

正常 rebase 进度：

```text
Found common ancestor: abc1234
Rebasing 3 commits from `feature` onto `main`...
Applied: def5678 feat: add parser
Applied: 987abcd feat: add lexer
Applied: 13579bd test: add parser tests
Successfully rebased branch 'feature' onto '1234567'.
```

Rebase 期间冲突：

```text
fatal: rebase stopped while applying 987abcd: feat: add lexer

Hint: conflicted files:
Hint:   src/lexer.rs
Hint: resolve conflicts, stage them, then run 'libra rebase --continue'.
Hint: or run 'libra rebase --skip' / 'libra rebase --abort'.
```

已经最新：

```text
Current branch is ahead of upstream. No rebase needed.
```

仅快进场景：

```text
Fast-forwarded branch 'feature' to 'main'.
```

Abort：

```text
Rebase aborted. Restored branch 'feature'.
```

## JSON / Machine 输出

当前，成功的 `rebase <upstream>`、`--abort`、`--continue` 和 `--skip` 输出支持 `--json` 和 `--machine`。CLI/preflight 失败、未解决冲突的 `--continue` 失败，以及结构化 `rebase <upstream>` 冲突停止，都会通过 Libra 标准结构化错误信封渲染。更深层的 replay/conflict-stop 错误分类仍在命令改进计划中作为后续工作跟踪。

开始并完成重放：

```json
{
  "ok": true,
  "command": "rebase",
  "data": {
    "action": "start",
    "status": "completed",
    "branch": "feature",
    "commit": "abc1234...",
    "upstream": "main",
    "onto": "fedcba9...",
    "common_ancestor": "0123456...",
    "replay_count": 1,
    "previous_commit": "def5678...",
    "applied_commits": [
      {
        "original_commit": "0123456...",
        "commit": "abc1234...",
        "subject": "Feature adds file"
      }
    ],
    "remaining": 0
  }
}
```

Fast-forward start 结果使用相同信封，`status: "fast-forwarded"`，`commit` 等于 `onto`，并且没有 `applied_commits`。已经领先 upstream 的分支返回 `status: "already-up-to-date"`。

```json
{
  "ok": true,
  "command": "rebase",
  "data": {
    "action": "abort",
    "status": "aborted",
    "branch": "feature",
    "commit": "abc1234...",
    "previous_commit": "def5678...",
    "restored": true
  }
}
```

解决冲突后 continue：

```json
{
  "ok": true,
  "command": "rebase",
  "data": {
    "action": "continue",
    "status": "completed",
    "branch": "feature",
    "commit": "abc1234...",
    "onto": "fedcba9...",
    "previous_commit": "def5678...",
    "applied_commits": [
      {
        "original_commit": "0123456...",
        "commit": "abc1234...",
        "subject": "Feature modifies conflict.txt"
      }
    ],
    "remaining": 0
  }
}
```

跳过已停止提交（exec 失败后 `skipped_commit` 缺失，因为已重放提交会保留）：

```json
{
  "ok": true,
  "command": "rebase",
  "data": {
    "action": "skip",
    "status": "completed",
    "branch": "feature",
    "commit": "abc1234...",
    "onto": "fedcba9...",
    "previous_commit": "def5678...",
    "skipped_commit": "0123456...",
    "skipped_subject": "Feature modifies conflict.txt",
    "remaining": 0
  }
}
```

## Rebase 状态持久化

Rebase 状态存储在 `rebase_state` SQLite 表中，包含以下字段：

| 字段 | 类型 | 说明 |
|-------|------|-------------|
| `head_name` | TEXT | 正在 rebase 的原始分支名 |
| `onto` | TEXT | 正在 rebase 到其上的提交哈希 |
| `orig_head` | TEXT | Rebase 开始前的原始 HEAD 提交 |
| `current_head` | TEXT | 当前新 base（目前已 rebased 提交的 HEAD） |
| `todo` | TEXT | 剩余待重放提交（换行分隔哈希） |
| `todo_actions` | TEXT | 剩余重放动作（换行分隔的 `pick` / `fixup` / `squash` / `amend`） |
| `done` | TEXT | 已重放提交（换行分隔哈希） |
| `stopped_sha` | TEXT（nullable） | 导致冲突的当前提交 |
| `autosquash` | INTEGER | 当前 rebase 是否折叠 autosquash 提交（`0` 或 `1`） |

`.libra/rebase-aux.json` 是原子且总是 fsync 的恢复 sidecar，保存可重复 exec 命令与 pending 索引、捕获的 update-refs 分支与 rewrite 映射，以及 held autostash 对象 ID。它会跨冲突、exec 失败、进程重启和 `maintenance gc` 存活（GC 把 held OID 当作 fail-closed reachability root）。最终分支移动在单一 SQLite 事务中完成，并在移动前比较捕获的旧 tip；并发分支移动会 fail-closed。任何 worktree 已检出的分支都不会被捕获。只有 refs、worktree/index、sequencer 状态与 autostash 恢复全部进入终态后才删除 sidecar。若 autostash 重放冲突，会先把对象提升到普通 stash 列表，保证本地变更可恢复。

## 设计理由

### 为什么没有 `--interactive` / `-i`？

Git 的交互式 rebase 会打开编辑器，包含一份可以重排、squash、edit 或 drop 的提交列表。这是 Git 最强大的功能之一，但本质上是交互式的：它需要编辑器会话，并在启动时由人类决策。

Libra 面向 AI 代理和自动化工作流，在这些场景中交互式编辑器会话不可行。Libra 不提供交互式 rebase，而是鼓励将复杂历史重写拆成离散操作：使用 `rebase` 进行线性重放，并在未来使用专用命令进行 squash 或重排。

### `--onto`

Git 的 `--onto` 标志允许将提交子集 rebase 到任意 base 上，独立于 upstream 引用。Libra **已支持** `--onto <newbase> [<upstream>] [<branch>]`：把 `<upstream>..HEAD` 区间重放到 `<newbase>` 上，第三个位置参数 `<branch>` 会在 rebase 前先被检出。不带 `--onto` 时，Libra 将从共同祖先到 HEAD 的所有提交 rebase 到指定 upstream 上，覆盖绝大多数 rebase 用例。

### 为什么在 SQLite 中持久化状态？

Git 将 rebase 状态持久化在 `.git/rebase-merge/` 目录中，每个字段一个文件（head-name、onto、orig-head 等）。这种方式脆弱：部分写入可能损坏状态，并发访问没有保护。

Libra 使用 SQLite 持久化 rebase 状态，提供：
- **原子写入**：状态更新是事务性的，防止部分损坏。
- **一致读取**：不会从部分写入文件中产生 torn reads。
- **Schema 演进**：可以通过迁移添加新字段，而不是添加新文件。
- **单一事实来源**：所有元数据位于一个数据库中，简化备份和恢复。

### 这与 Git 和 jj 如何比较？

Git 的 rebase 功能丰富，包含交互模式、autosquash、`--onto`、`--exec`、`--rebase-merges` 等。它是 Git 中最复杂的命令之一，在冲突解决和状态管理方面有大量边缘场景。

jj 采取根本不同的方法：历史默认不可变，没有传统 rebase 命令。虽然存在 `jj rebase`，但它直接作用于修订 DAG，将修订及其后代移动到新父级。冲突记录在提交自身中，而不是停止流程，因此没有 `--continue`/`--abort` 流程。

Libra 提供折中方案：带 conflict-stop 语义的线性 rebase（Git 用户熟悉），同时使用 SQLite-backed 状态持久化以提高可靠性。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| Upstream | `<upstream>`（位置参数） | `<upstream>`（位置参数） | `-d` / `--destination` |
| Continue | `--continue` | `--continue` | N/A（冲突存储在提交中） |
| Abort | `--abort` | `--abort` | `jj op undo` |
| Skip | `--skip` | `--skip` | N/A |
| Interactive | 不支持 | `-i` / `--interactive` | N/A |
| Onto | `--onto <newbase>` | `--onto <newbase>` | 带 `-s` / `--source` 的 `-d` |
| Exec | 支持；可重复、强制 workspace-write/禁网 sandbox、失败可续作 | `--exec <cmd>` | N/A |
| Autosquash | 支持（`--autosquash`） | `--autosquash` | N/A |
| Autostash | 支持 `--autostash` / `--no-autostash`；tracked 变更跨 sequencer 停止保持 held | `--autostash` / `--no-autostash` | N/A |
| Update refs | 支持；排除已检出分支，并原子比较捕获 tip 后移动 | `--update-refs` / `--no-update-refs` | N/A |
| Fork point | 支持 upstream reflog 选点与 merge-base 回退 | `--fork-point` / `--no-fork-point` | N/A |
| Rerere autoupdate | `--no-rerere-autoupdate` 为接受式 no-op；不公开正向 flag，暂存遵循 `rerere.autoUpdate` | `--rerere-autoupdate` / `--no-rerere-autoupdate` | N/A |
| Rebase merges | 不支持 | `--rebase-merges` | 默认行为 |
| Keep empty | `--keep-empty`（no-op；默认已保留）/ `--no-keep-empty`（丢弃 start-empty 提交） | `--keep-empty` / `--no-keep-empty` | 默认保留空提交 |
| Empty mode | `--empty=<drop\|keep>`（become-empty；默认 **keep**） | `--empty=<drop\|keep\|stop>`（默认 drop） | N/A |
| Force rebase | 不支持 | `--force-rebase` | N/A |
| Branch | `<branch>`（第三个位置参数） | `<branch>`（第三个位置参数） | `-s` / `--source` |
| Revision set | 不支持 | N/A | `-r` / `--revisions` |
| 状态持久化 | SQLite 数据库 | `.git/rebase-merge/` 目录 | 不适用 |

注意：jj 在 rebase 期间不会因冲突停止。相反，冲突会 materialize 到提交内容中，并可稍后解决，因此不需要 `--continue`/`--abort`/`--skip`。

## 错误处理

`execute_safe` 与 replay 控制对 CLI、状态、冲突、sandbox 和 durable-sidecar 失败返回标准结构化 `CliError` 信封。

| 场景 | StableErrorCode | 退出码 | 行为 |
|----------|-----------------|------|----------|
| 不是 libra 仓库 | `LBR-REPO-001`（RepoNotFound） | 128 | 以 repo-not-found 消息报错 |
| 缺少 upstream | `LBR-CLI-002`（CliInvalidArgument） | 129 | 来自 clap 的用法错误 |
| Upstream ref 无法解析 | `LBR-CLI-003`（CliInvalidTarget） | 129 | 报告 ref 无效的错误 |
| 没有进行中 rebase 却 `--continue` | `LBR-REPO-003`（RepoStateInvalid） | 128 | 报告没有进行中 rebase |
| `--continue` 仍有未解决冲突 | `LBR-CONFLICT-001`（ConflictUnresolved） | 128 | 报告冲突必须用 `libra add <file>` 暂存 |
| 没有进行中 rebase 却 `--abort` | `LBR-REPO-003`（RepoStateInvalid） | 128 | 报告没有进行中 rebase |
| 没有进行中 rebase 却 `--skip` | `LBR-REPO-003`（RepoStateInvalid） | 128 | 报告没有进行中 rebase |
| `--skip` 但没有已停止或待处理提交 | `LBR-REPO-003`（RepoStateInvalid） | 128 | 报告没有可跳过提交 |
| 空或包含 NUL 的 `--exec` 命令 | `LBR-CLI-002`（CliInvalidArguments） | 129 | 在创建 rebase 状态或修改 worktree 前拒绝 |
| Exec 失败、超时或强制 sandbox 不可用 | `LBR-CONFLICT-002`（ConflictOperationBlocked） | 128 | Rebase 保持可续作；修复后 `--continue`，或 `--skip` 剩余 exec 命令 |
| Autostash 重放冲突 | warning；held 对象提升为 `stash@{0}` | 0 | Rebase 完成且本地变更不丢失；检查 stash |
| Update-refs 分支被并发移动 | `LBR-IO-002`（IoWriteFailed） | 128 | Ref 事务回滚，rebase 保持可续作 |
| 找不到共同祖先 | 待定类型映射 | 128 | Legacy text 错误，拒绝 rebase 无关历史 |
| 提交重放期间冲突 | 待定类型映射 | 128 | Rebase 停止，状态已保存，提示用户解决 |
| 无法创建 rebased 提交 | 待定类型映射 | 128 | 带提交详情的 legacy text 错误 |
| 无法更新分支引用 | 待定类型映射 | 128 | 带 ref 更新详情的 legacy text 错误 |
