# `libra reset`

移动 `HEAD`，并根据所选模式重置索引或工作树。

## 概要

```
libra reset [<target>] [--soft | --mixed | --hard]
libra reset <pathspec>...
libra reset [<target>] [--] <pathspec>...
```

## 说明

`libra reset` 将 HEAD 引用移动到目标提交，并可选地重置索引和工作树以匹配目标。三种模式控制影响多少状态：

- **`--soft`**：只移动 HEAD。索引和工作树保持不变，因此旧 HEAD 和目标之间的所有差异都会表现为已暂存更改。适合 squash commits。
- **`--mixed`**（默认）：移动 HEAD 并重置索引。工作树保持不变，因此更改表现为未暂存修改。适合取消暂存文件。
- **`--hard`**：移动 HEAD、重置索引并恢复工作树。所有未提交更改都会被丢弃。适合完全回到已知状态。

提供 pathspec 时，命令执行有针对性的 mixed reset：只将命名文件在索引中重置为匹配目标提交，不移动 HEAD。这是取消暂存特定文件的主要方式。与 Git 一样，如果第一个裸位置参数是已知路径且不是 revision，`libra reset src/lib.rs` 会按 `HEAD` 目标的 pathspec reset 处理，等价于 `libra reset HEAD -- src/lib.rs`。如果同一个 token 既是 revision 又是文件名，reset 会拒绝猜测并报歧义；要把它作为目标 revision，请使用 `libra reset <revision> -- <file>`，要把它作为路径，请使用 `libra reset -- <file>`。Pathspec 与 `--soft` 和 `--hard` 不兼容。当 pathspec reset 从目标提交恢复符号链接时，索引条目会保留 mode `120000`，blob 仍是链接目标字节。

默认目标是 `HEAD`，因此不带参数的 `libra reset` 等价于取消暂存所有内容。

`reset --hard` 恢复工作树时会保留 tree 中的文件类型：符号链接会恢复为真正的 symlink，链接 blob 字节作为目标路径写入；若工作树当前位置已有普通文件或已有 symlink，必要时会被替换为目标 symlink。不支持 symlink 的平台会返回明确诊断，而不是把链接目标写入普通文件。

## 选项

| 标志 | 长选项 | 值 | 说明 |
|------|------|-------|-------------|
| | `<target>` | 位置参数（默认：`HEAD`） | 要重置到的提交、分支或修订表达式 |
| | `--soft` | | 只移动 HEAD；保留索引和工作树 |
| | `--mixed` | | 移动 HEAD 并重置索引；保留工作树（默认） |
| | `--hard` | | 移动 HEAD、重置索引并恢复工作树 |
| | `<pathspec>...` | 位置参数，可放在 `--` 之后 | 要在索引中重置的特定文件 |

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

# 面向代理的 JSON 输出
libra reset --json --hard HEAD~1
```

## 常用命令

```bash
libra reset HEAD~1                    # 移动 HEAD 并将索引重置到上一个提交
libra reset --soft HEAD~2             # 只移动 HEAD，保留索引和工作树
libra reset --hard main               # 将 HEAD、索引和工作树重置到分支 'main'
libra reset --hard HEAD               # 同时把已跟踪符号链接恢复为 symlink
libra reset src/lib.rs                 # 将路径取消暂存回 HEAD
libra reset HEAD -- src/lib.rs        # 将路径取消暂存回 HEAD
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
- `files_restored` 统计由 `--hard` 重写或移除的已跟踪文件；在干净仓库中，`reset --hard HEAD` 可报告 `0`。
- `files_unstaged` 统计 mixed/pathspec reset 期间索引条目被重置的文件数。
- `subject` 是目标提交消息的第一行。

## 设计理由

### 为什么拒绝 pathspec 与 --soft/--hard 组合？

- **`--soft` + pathspecs**：`--soft` 按定义只移动 HEAD，不触碰其他内容。重置单个文件索引条目违背“仅 HEAD”的契约。如果要取消暂存特定文件，请使用默认 mixed 模式：`libra reset file` 或 `libra reset HEAD -- file`。
- **`--hard` + pathspecs**：`--hard` 将整个工作树恢复为匹配目标提交。只选择性恢复一些文件，同时让其他文件处于不同状态，会产生令人困惑的混合状态，既不是“完全 reset”，也不是“仅索引 reset”。对于选择性文件恢复，请使用 `libra restore --source <commit> -- file`。

该限制让三种模式无歧义：soft 触碰 HEAD，mixed 触碰 HEAD + index，hard 触碰 HEAD + index + worktree。Pathspec 正交地只在索引层面操作。

### 为什么默认 mixed？

Mixed 模式是最安全的通用 reset：它取消暂存更改但不丢弃工作。开发者不考虑模式直接运行 `libra reset HEAD~1` 时，会将更改保留在工作树中作为未暂存修改。这匹配 Git 默认值，并且对最常见用例（取消暂存文件或 amend 提交）来说最不意外。

### 为什么没有 --merge/--keep？

Git 的 `--merge` 和 `--keep` 模式试图通过在旧 HEAD、新 HEAD 和工作树之间执行三方合并，在 reset 期间保留未提交更改。这些模式：

- **很少使用**：大多数开发者只使用 `--soft`、`--mixed` 或 `--hard`。merge/keep 模式为小众用例增加复杂度。
- **难以推理**：reset 期间的三方合并可能产生冲突，让仓库处于既非“已 reset”也非“未更改”的状态。这会让人类和 AI 代理困惑。
- **可由显式工作流替代**：同样结果可通过 `libra stash && libra reset --hard <target> && libra stash pop` 实现，每一步都可见且可调试。

Libra 偏好显式、可组合命令，而不是隐藏在单个标志背后的隐式多步操作。

## 参数对比：Libra vs Git vs jj

| 功能 | Git | Libra | jj |
|---------|-----|-------|----|
| Mixed reset（默认） | `git reset <target>` | `libra reset <target>` | N/A（jj 没有暂存区） |
| Soft reset | `git reset --soft <target>` | `libra reset --soft <target>` | N/A |
| Hard reset | `git reset --hard <target>` | `libra reset --hard <target>` | `jj restore --from <rev>` |
| 取消暂存文件 | `git reset <file>` / `git reset HEAD -- <file>` | `libra reset <file>` / `libra reset HEAD -- <file>` | N/A（无暂存区） |
| Merge reset | `git reset --merge <target>` | 不支持 | N/A |
| Keep reset | `git reset --keep <target>` | 不支持 | N/A |
| 默认目标 | HEAD | HEAD | N/A |
| 结构化输出 | 无 | `--json` / `--machine` | `--template` |
| Pathspec + soft | 拒绝 | 拒绝（`LBR-CLI-002`） | N/A |
| Pathspec + hard | 拒绝 | 拒绝（`LBR-CLI-002`） | N/A |
| 失败回滚 | 无 | 尝试索引回滚 | N/A（operation log undo） |

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
| Pathspec 不匹配 | `LBR-CLI-003` | "check the path and try again." |
| 回滚失败 | （主错误码） | （主提示） |
