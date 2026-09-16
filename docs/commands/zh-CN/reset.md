# `libra reset`

移动 `HEAD`，并根据所选模式重置索引或工作树。

## 概要

```
libra reset [<target>] [--soft | --mixed | --hard | --merge | --keep]
libra reset <pathspec>...
libra reset [<target>] [--] <pathspec>...
libra reset [<target>] --pathspec-from-file=<file> [--pathspec-file-nul]
```

## 说明

`libra reset` 将 HEAD 引用移动到目标提交，并可选地重置索引和工作树以匹配目标。五种模式控制影响多少状态：

- **`--soft`**：只移动 HEAD。索引和工作树保持不变，因此旧 HEAD 和目标之间的所有差异都会表现为已暂存更改。适合 squash commits。
- **`--mixed`**（默认）：移动 HEAD 并重置索引。工作树保持不变，因此更改表现为未暂存修改。适合取消暂存文件。
- **`--hard`**：移动 HEAD、重置索引并恢复工作树。所有未提交更改都会被丢弃。适合完全回到已知状态。
- **`--merge`**：重置 HEAD/index，只更新可安全替换的路径并保留 unstaged 变更；若 target/index 变化会覆盖 unstaged 路径，则在写入前拒绝。
- **`--keep`**：重置 HEAD/index，但 target 与旧 HEAD 不同的路径只要存在 staged 或 unstaged 本地变更就拒绝；不受影响路径的本地变更会保留。

两个保留模式都会先完整预检，并快照原始 index 字节及受影响工作树条目；工作树写入或最终 ref 更新失败时精确恢复，避免部分 reset。工作树遍历绝不跟随 symlink ancestor（包括被 ignore 的 symlink）；不安全写入会失败并 rollback，不会触碰链接目标。逃出工作树或指向 `.libra` 元数据的损坏 index/tree 路径也会被拒绝。`--merge` 还会保留既有 unmerged index stages。

提供 pathspec 时，命令执行有针对性的 mixed reset：只将命名文件在索引中重置为匹配目标提交，不移动 HEAD。这是取消暂存特定文件的主要方式。与 Git 一样，如果第一个裸位置参数是已知路径且不是 revision，`libra reset src/lib.rs` 会按 `HEAD` 目标的 pathspec reset 处理，等价于 `libra reset HEAD -- src/lib.rs`。如果同一个 token 既是 revision 又是文件名，reset 会拒绝猜测并报歧义；要把它作为目标 revision，请使用 `libra reset <revision> -- <file>`，要把它作为路径，请使用 `libra reset -- <file>`。Pathspec 与 `--soft`、`--hard`、`--merge`、`--keep` 不兼容。当 pathspec reset 从目标提交恢复符号链接时，索引条目会保留 mode `120000`，blob 仍是链接目标字节。

无 pathspec 的整树 reset（任意模式）还会结束已停止的序列项：清除已停止的单提交 cherry-pick 或 revert 状态，使下一次 pick 或 revert 可以正常开始。多提交序列保留剩余提交，并记录被停提交已在此结束；用 `libra cherry-pick --skip`（或 `--quit`）、`libra revert --abort` 收尾。带 pathspec 的 reset 不改动任何序列状态，进行中的 rebase 也不受影响。若该收尾写入失败，reset 仍然成功，并在 warning 中指明残留状态。

默认目标是 `HEAD`，因此不带参数的 `libra reset` 等价于取消暂存所有内容。

`reset --hard` 恢复工作树时会保留 tree 中的文件类型：符号链接会恢复为真正的 symlink，链接 blob 字节作为目标路径写入；若工作树当前位置已有普通文件或已有 symlink，必要时会被替换为目标 symlink。不支持 symlink 的平台会返回明确诊断，而不是把链接目标写入普通文件。

## 选项

| 标志 | 长选项 | 值 | 说明 |
|------|------|-------|-------------|
| | `<target>` | 位置参数（默认：`HEAD`） | 要重置到的提交、分支或修订表达式 |
| | `--soft` | | 只移动 HEAD；保留索引和工作树 |
| | `--mixed` | | 移动 HEAD 并重置索引；保留工作树（默认） |
| | `--hard` | | 移动 HEAD、重置索引并恢复工作树 |
| | `--merge` | | 重置 HEAD/index，更新安全路径并保留 unstaged 变更 |
| | `--keep` | | 受影响路径有本地变更时拒绝 reset |
| | `<pathspec>...` | 位置参数，可放在 `--` 之后 | 要在索引中重置的特定文件 |
| | `--pathspec-from-file` | `<file>` | 从文件（`-` 为 stdin）读取 pathspec；与 CLI pathspec 互斥 |
| | `--pathspec-file-nul` | | 使用 NUL 分隔 pathspec |
| | `--no-refresh` | | Git 兼容 no-op；Libra 不执行 index refresh |

### 标志示例

```bash
# 取消暂存所有内容（mixed reset 到 HEAD）
libra reset

# 将 HEAD 后退一个提交，保留更改为已暂存
libra reset --soft HEAD~1

# 将 HEAD 后退两个提交，取消暂存更改
libra reset HEAD~2

# 完全回到某个分支 tip，丢弃所有更改
libra reset --hard main

# 保留可安全保留的 unstaged 变更
libra reset --merge HEAD~1

# 受影响路径有本地变更时拒绝
libra reset --keep HEAD~1

# 取消暂存特定文件
libra reset src/lib.rs

# 取消暂存一个看起来像 revision 的文件名
libra reset -- HEAD

# 从显式目标取消暂存特定文件
libra reset HEAD -- src/lib.rs

# 取消暂存多个文件
libra reset src/main.rs src/cli.rs

# 将特定文件重置到先前提交
libra reset abc1234 -- path/to/file.rs

# 从文件批量读取 pathspec
libra reset --pathspec-from-file=paths.txt

# 面向代理的 JSON 输出
libra reset --json --hard HEAD~1
```

## 常用命令

```bash
libra reset HEAD~1                    # 移动 HEAD 并将索引重置到上一个提交
libra reset --soft HEAD~2             # 只移动 HEAD，保留索引和工作树
libra reset --hard main               # 将 HEAD、索引和工作树重置到分支 'main'
libra reset --merge HEAD~1            # 保留安全的 unstaged 变更
libra reset --keep HEAD~1             # 受影响路径有本地变更时拒绝
libra reset --hard HEAD               # 同时把已跟踪符号链接恢复为 symlink
libra reset src/lib.rs                 # 将路径取消暂存回 HEAD
libra reset HEAD -- src/lib.rs        # 将路径取消暂存回 HEAD
libra reset --pathspec-from-file=paths.txt   # 从文件读取待取消暂存路径
libra reset --json --hard HEAD~1      # 面向代理的结构化 JSON 输出
```

## 人类可读输出

完整 reset（无 pathspec）：

```text
HEAD is now at abc1234 Initial commit
```

Pathspec reset（取消暂存特定文件）：

```text
Unstaged changes after reset:
M	path/to/file
```

## 结构化输出（JSON 示例）

完整 reset：

```json
{
  "ok": true,
  "command": "reset",
  "data": {
    "mode": "hard",
    "commit": "abc123def456789012345678901234567890abcd",
    "short_commit": "abc123d",
    "subject": "Initial commit",
    "previous_commit": "def456abc789012345678901234567890abcd1234",
    "files_unstaged": 0,
    "files_restored": 1,
    "pathspecs": []
  }
}
```

Pathspec reset：

```json
{
  "ok": true,
  "command": "reset",
  "data": {
    "mode": "mixed",
    "commit": "abc123def456789012345678901234567890abcd",
    "short_commit": "abc123d",
    "subject": "Initial commit",
    "previous_commit": null,
    "files_unstaged": 2,
    "files_restored": 0,
    "pathspecs": ["src/lib.rs", "src/cli.rs"]
  }
}
```

### Schema 说明

- 当 `pathspecs` 非空时，命令只对指定路径执行 mixed reset，不移动 HEAD。
- `previous_commit` 对 pathspec-only reset 为 `null`（HEAD 不移动）。
- `files_restored` 统计由 `--hard`、`--merge` 或 `--keep` 重写/移除的已跟踪文件；same-target clean reset 可报告 `0`。
- `files_unstaged` 统计 mixed/pathspec reset 期间索引条目被重置的文件数。
- `subject` 是目标提交消息的第一行。

## 设计理由

### 为什么拒绝 pathspec 与 whole-tree 模式组合？

- **`--soft` + pathspecs**：`--soft` 按定义只移动 HEAD，不触碰其他内容。重置单个文件索引条目违背“仅 HEAD”的契约。如果要取消暂存特定文件，请使用默认 mixed 模式：`libra reset file` 或 `libra reset HEAD -- file`。
- **`--hard` + pathspecs**：`--hard` 将整个工作树恢复为匹配目标提交。只选择性恢复一些文件，同时让其他文件处于不同状态，会产生令人困惑的混合状态，既不是“完全 reset”，也不是“仅索引 reset”。对于选择性文件恢复，请使用 `libra restore --source <commit> -- file`。
- **`--merge`/`--keep` + pathspecs**：安全判断把 target、HEAD、index、worktree 视为一个完整 transition；部分路径会改变保护契约，请改用 mixed pathspec reset 或 `libra restore`。

该限制让 pathspec reset 始终是 index-only mixed 操作，而所有工作树模式都是 whole-tree transition。

### 为什么默认 mixed？

Mixed 模式是最安全的通用 reset：它取消暂存更改但不丢弃工作。开发者不考虑模式直接运行 `libra reset HEAD~1` 时，会将更改保留在工作树中作为未暂存修改。这匹配 Git 默认值，并且对最常见用例（取消暂存文件或 amend 提交）来说最不意外。

### `--merge` 与 `--keep`

`--merge` 保护 index→worktree 的 unstaged 变更，在安全时可以丢弃 staged 变更；`--keep` 对旧 HEAD 与 target 不同的路径更严格，staged 或 unstaged 本地变更都会使其拒绝。两者都不会写冲突标记；不安全 transition 在任何 mutation 前以 `LBR-CONFLICT-002` 失败。

## 参数对比：Libra vs Git vs jj

| 功能 | Git | Libra | jj |
|---------|-----|-------|----|
| Mixed reset（默认） | `git reset <target>` | `libra reset <target>` | N/A（jj 没有暂存区） |
| Soft reset | `git reset --soft <target>` | `libra reset --soft <target>` | N/A |
| Hard reset | `git reset --hard <target>` | `libra reset --hard <target>` | `jj restore --from <rev>` |
| 取消暂存文件 | `git reset <file>` / `git reset HEAD -- <file>` | `libra reset <file>` / `libra reset HEAD -- <file>` | N/A（无暂存区） |
| Merge reset | `git reset --merge <target>` | `libra reset --merge <target>` | N/A |
| Keep reset | `git reset --keep <target>` | `libra reset --keep <target>` | N/A |
| 默认目标 | HEAD | HEAD | N/A |
| 结构化输出 | 无 | `--json` / `--machine` | `--template` |
| Pathspec + soft | 拒绝 | 拒绝（`LBR-CLI-002`） | N/A |
| Pathspec + hard | 拒绝 | 拒绝（`LBR-CLI-002`） | N/A |
| Pathspec + merge/keep | 拒绝 | 拒绝（`LBR-CLI-002`） | N/A |
| 失败回滚 | 无 | classic mode 尝试 tree rollback；merge/keep 精确恢复 index/worktree snapshot | N/A（operation log undo） |

## 错误处理

| 场景 | 错误码 | 提示 |
|----------|-----------|------|
| 不是 libra 仓库 | `LBR-REPO-001` | "run 'libra init' to create a repository in the current directory." |
| 无效修订 | `LBR-CLI-003` | "check the revision name and try again." |
| revision/path token 歧义 | `LBR-CLI-002` | "use '--' to separate paths from revisions, like 'libra reset <revision> -- <file>' or 'libra reset -- <file>'." |
| HEAD unborn | `LBR-REPO-003` | "create a commit first before resetting HEAD." |
| 无法解析 HEAD | `LBR-IO-001` | "check whether the repository database is readable." |
| HEAD 引用损坏 | `LBR-REPO-002` | "the HEAD reference or branch metadata may be corrupted." |
| 对象加载失败 | `LBR-REPO-002` | "the object store may be corrupted." |
| 索引加载失败 | `LBR-REPO-002` | "the index file may be corrupted." |
| 索引保存失败 | `LBR-IO-002` | -- |
| HEAD 更新失败 | `LBR-IO-002` | -- |
| 工作树读取失败 | `LBR-IO-001` | -- |
| 工作树恢复失败 | `LBR-IO-002` | -- |
| 无效路径编码 | `LBR-CLI-002` | "rename the path or invoke libra from a path representable as UTF-8." |
| `--soft` 与 pathspec 组合 | `LBR-CLI-002` | "--soft only moves HEAD; use --mixed to reset index for specific paths." |
| `--hard` 与 pathspec 组合 | `LBR-CLI-002` | "--hard updates the working tree; omit pathspecs or use --mixed for specific paths." |
| `--merge`/`--keep` 与 pathspec 组合 | `LBR-CLI-002` | "--merge/--keep operate on the whole tree; omit pathspecs or use --mixed for specific paths." |
| `--merge`/`--keep` 会覆盖本地变更 | `LBR-CONFLICT-002` | "commit or stash the local changes, then retry the reset." |
| Pathspec 不匹配 | `LBR-CLI-003` | "check the path and try again." |
| 回滚失败 | （主错误码） | （主提示） |
