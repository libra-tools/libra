# `libra restore`

从来源恢复工作树文件或索引条目。

**别名：** `unstage`

## 概要

```
libra restore [--source <tree-ish>] [--staged] [--worktree] <pathspec>...
libra restore (--ours | --theirs | --merge | --conflict <style>) <pathspec>...
libra restore --ignore-unmerged [--source <tree-ish>] <pathspec>...
```

## 说明

`libra restore` 从给定来源恢复工作树或索引中的文件。默认情况下（未指定 `--staged` 或 `--worktree` 时），它会从索引恢复工作树中的文件，实际效果是丢弃未暂存更改。使用 `--staged` 时，它从 HEAD（或指定的 `--source`）恢复索引，也就是取消暂存文件。同时使用 `-S` 和 `-W` 时，它会同时恢复索引和工作树。

对于新工作流，请直接使用 `libra restore`。`libra checkout -- <path>` 和 `libra checkout <tree-ish> -- <path>` 仅作为此路径恢复行为的 Git 兼容别名被接受。

`<pathspec>` 参数是必需的，并接受一个或多个文件路径或目录路径。特殊路径 `.` 会恢复所有文件。Pathspec 使用 Libra 共享的 Git 风格匹配器：普通 pathspec 匹配文件或目录前缀，支持通配符，并支持高价值 magic 形式 `:(top)`、`:/`、`:(glob)`、`:(literal)`、`:(icase)`、`:(exclude)`、`:!`、`:^`。排除 pathspec 会从正向选择中扣除；启用 `core.ignorecase` 时，匹配会按忽略大小写处理；看起来像通配符的 pathspec 也会匹配同名的字面路径或目录前缀。

当来源提交包含当前工作树中不存在的文件时，这些文件会被创建。在默认（`--no-overlay`）模式下，当当前工作树包含来源中不存在的已跟踪文件时，这些文件会被删除以使目标与来源完全一致；使用 `--overlay` 时则保留这些来源中缺失的已跟踪路径。输出会分别报告 `restored_files` 和 `deleted_files`。

从引用 LFS 指针的提交恢复时，LFS 管理的文件会自动从 LFS 服务器下载。

从来源 tree、索引或冲突 stage 恢复符号链接时，Libra 会在支持 symlink 的平台上创建真正的 symlink，并把链接 blob 字节作为目标路径。恢复过程不会跟随或打开目标路径，因此指向仓库外部的 symlink 也只会被恢复为链接本身。`--merge` 重建冲突标记时也会先替换工作树中的既有 symlink，再写入普通冲突标记文件。不支持 symlink 的平台会返回明确诊断，而不是把链接目标写成普通文件内容。

## 选项

| 选项 | 短选项 | 长选项 | 说明 |
|--------|-------|------|-------------|
| Pathspec | | 位置参数（必需） | 要恢复的一个或多个文件或目录。支持共享 pathspec magic；使用 `.` 表示所有文件。 |
| Source | `-s` | `--source <tree-ish>` | 从指定提交或 tree-ish 恢复，而不是从默认来源恢复。省略时，默认来源取决于模式：工作树恢复使用索引，暂存恢复使用 HEAD。 |
| Staged | `-S` | `--staged` | 恢复索引（取消暂存文件）。如果未给出 `--source`，默认来源为 HEAD。 |
| Worktree | `-W` | `--worktree` | 恢复工作树。当未给出 `--staged` 时这是默认值。 |
| Ours | `-2` | `--ours` | 对未合并路径，将冲突 stage 2（我方）写入工作树。与 `--theirs`、`--source`、`--staged`、`--ignore-unmerged` 互斥。 |
| Theirs | `-3` | `--theirs` | 对未合并路径，将冲突 stage 3（对方）写入工作树。互斥关系同 `--ours`。 |
| Merge | | `--merge` | 对未合并路径，从索引 stage 重建冲突标记写回工作树（ours=stage 2、theirs=stage 3），索引保持未合并。Libra 写整文件 `ours`/`theirs` 标记（标签为通用 `ours`/`theirs`），非 Git 行级 3-way。（注：`libra merge`/`cherry-pick` 现经三方合并引擎写行级标记；restore 的索引-stage 重建仍为整文件。）互斥关系同 `--ours`。 |
| 冲突风格 | | `--conflict <style>` | 隐含 `--merge`。`merge`（默认）写 `ours`/`theirs` 块；`diff3` 额外含 base 块（stage 1）。`zdiff3` 不支持。 |
| 忽略未合并 | | `--ignore-unmerged` | 跳过未合并路径而非报错；其余路径仍正常恢复。 |
| 从文件读取 pathspec | | `--pathspec-from-file <FILE>` | 从 `<FILE>` 读取共享匹配器 pathspec（每行一个，`-` 读 stdin）。给出此选项时，文件内容会替换位置 pathspec（此时无需再提供位置参数）。 |
| pathspec 文件 NUL 分隔 | | `--pathspec-file-nul` | 经 `--pathspec-from-file` 读取的 pathspec 以 NUL 而非换行分隔（要求同时给出 `--pathspec-from-file`）。 |
| 不显示进度条 | | `--no-progress` | 不显示进度条。为对齐 Git 而接受的 no-op：Libra 的 restore 从不渲染进度条。 |
| Overlay | | `--overlay` | 以 overlay 模式恢复：仅创建/更新 source 中存在的路径；source 中缺失的已跟踪路径保持不动而非被移除。与 `--no-overlay` 构成切换对（后者优先=最后给出的生效）。 |
| 不用 overlay | | `--no-overlay` | 不以 overlay 模式恢复（默认）：source 中缺失的路径会从目标移除，使目标与 source 完全一致。与 `--overlay` 构成切换对（最后给出的生效）。 |
| JSON | | `--json` | 输出结构化 JSON。 |
| Quiet | | `--quiet` | 抑制人类可读输出。 |

### 选项细节

**`--source` / `-s`**

指定一个提交、标签或任意 tree-ish 作为恢复来源：

```bash
# 从上一个提交恢复
libra restore --source HEAD~1 src/main.rs

# 从特定提交哈希恢复
libra restore -s abc1234 lib/
```

**`--staged` / `-S`**

从 HEAD（或 `--source`）恢复索引，实际效果是取消暂存文件：

```bash
# 取消暂存一个文件
libra restore --staged file.txt

# 取消暂存所有文件
libra restore --staged .
```

**`--worktree` / `-W`**

显式目标为工作树。当未指定 `--staged` 时这是默认行为，因此只有与 `--staged` 组合时才需要：

```bash
# 同时恢复索引和工作树
libra restore -S -W file.txt
```

**冲突阶段恢复：`--ours` / `-2`、`--theirs` / `-3`、`--ignore-unmerged`**

当一次合并留下未合并路径时，索引最多保留三个冲突阶段：stage 1（合并基）、stage 2（“我方”——当前分支）、stage 3（“对方”——被合并分支）。编辑完带冲突标记的文件后，你可以整体取回某一侧：

```bash
# 取冲突的我方一侧
libra restore --ours file.txt

# 取冲突的对方一侧
libra restore --theirs file.txt
```

这些标志读取冲突阶段后**只重写工作树**——索引刻意保持未合并，因此 `libra status` 仍会报告冲突，直到你用 `libra add` 暂存一个解决方案。它们按设计仅作用于工作树，故在 CLI 层拒绝 `--source` 与 `--staged`（以及彼此）（`LBR-CLI-002`，退出码 129）。若请求的阶段不存在（例如 modify/delete 冲突没有“对方”版本），命令以 `LBR-CONFLICT-001`、退出码 128 失败。

普通 `libra restore` 命中未合并路径时会拒绝操作并报告 `path '<file>' is unmerged`（`LBR-CONFLICT-001`，退出码 128），以免静默覆盖或跳过冲突。传 `--ignore-unmerged` 可跳过未合并路径、恢复其余：

```bash
# 从 HEAD 恢复所有内容，跳过仍冲突的路径
libra restore --ignore-unmerged --source HEAD .
```

> **尚未支持：** Git 的行级 3-way 冲突标记与 `zdiff3` 风格（Libra 从索引 stage 重建整文件 `ours`/`theirs` 标记——不同于现已行级的 `libra merge`/`cherry-pick`），以及 `-p` / `--patch` 仍被推迟。参见 [COMPATIBILITY.md](../../../COMPATIBILITY.md)。

## 常用命令

```bash
# 丢弃文件的未暂存更改（从索引恢复）
libra restore file.txt

# 取消暂存文件（从 HEAD 恢复索引）
libra restore --staged file.txt

# 从特定提交恢复
libra restore --source HEAD~1 src/main.rs

# 恢复 Rust 文件，但排除生成文件
libra restore ':(glob)src/*.rs' ':(exclude)src/generated.rs'

# 从 HEAD 恢复已跟踪符号链接
libra restore --source HEAD link-to-target

# 同时恢复工作树和索引
libra restore -S -W file.txt

# 从 HEAD 恢复所有内容
libra restore --source HEAD .

# 取合并冲突的我方/对方一侧
libra restore --ours file.txt
libra restore --theirs file.txt

# 从 HEAD 恢复，跳过仍冲突的路径
libra restore --ignore-unmerged --source HEAD .

# overlay 恢复：从较早的提交更新文件，但不删除较新的文件
libra restore --overlay --source HEAD~3 .

# 面向脚本的 JSON 输出
libra restore --json --source HEAD .
```

## 人类可读输出

```text
Updated 3 path(s) from HEAD
```

确认信息报告的是已恢复文件和已删除文件并集的数量（也就是说，当来源中移除了一个已跟踪文件时，它会从工作树/索引中删除）。当省略 `--source` 时，对于 `--staged` 恢复，来源标签为 `HEAD`；对于仅工作树恢复，来源标签为 `the index`：

```text
Updated 1 path(s) from the index
```

`--quiet` 会抑制所有输出。如果既没有匹配到恢复路径，也没有匹配到删除路径，则不会输出确认信息（因此 no-op restore 是静默的）。

## 结构化输出（JSON）

```json
{
  "command": "restore",
  "data": {
    "source": "HEAD",
    "worktree": true,
    "staged": false,
    "restored_files": ["src/main.rs"],
    "deleted_files": []
  }
}
```

从索引恢复时（工作树恢复未指定 `--source`），`source` 字段为 `null`。

## 设计理由

### 为什么与 checkout 分离？

Git 的 `checkout` 命令承担两种非常不同的用途：切换分支和恢复文件。这种重载被广泛认为是 Git 最差的 UX 决策之一。Git 自身也通过在 Git 2.23 中引入 `git restore`（用于文件）和 `git switch`（用于分支）解决了这一点。Libra 从一开始就遵循这种拆分，使 `restore` 成为文件内容的首选命令，并且永远不用于分支操作。`checkout -- <path>` 仅作为兼容别名保留给拥有 Git 肌肉记忆的用户。

### 为什么显式使用 `--worktree` / `--staged` 标志？

Git 的 `restore` 默认为仅恢复工作树，并要求 `--staged` 才能以索引为目标。Libra 遵循相同约定，但让这些标志正交且可组合：

- 无标志：仅工作树（从索引）。
- `--staged`：仅索引（从 HEAD）。
- `--staged --worktree`：两个目标。

这种显式模型消除了 Git `checkout` 中的困惑：`git checkout -- file` 恢复工作树，而 `git checkout HEAD -- file` 同时恢复工作树和索引，这个区别许多用户从未真正内化。

### 为什么 `--staged` 会自动将 `--source` 设为 HEAD？

取消暂存文件时，自然来源是 HEAD（最后一次提交）。每次都要求 `--source HEAD` 会繁琐且容易出错。Libra 在使用 `--staged` 且未给出 `--source` 时自动默认到 HEAD，匹配 Git 行为和用户预期。

### 为什么要求 pathspec？

与可用 `--worktree` 作用于整个工作树的 `git restore` 不同，Libra 要求至少一个 pathspec 参数。这可以防止意外恢复整个工作树。当你明确想恢复所有内容时，使用 `.` 作为 pathspec。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| Pathspec | `<pathspec>...`（必需） | `<pathspec>...`（可选） | `jj restore <paths>...` |
| 来源提交 | `-s` / `--source <tree-ish>` | `-s` / `--source <tree>` | `--from <revision>` |
| 目标工作树 | `-W` / `--worktree` | `-W` / `--worktree`（默认） | 默认行为 |
| 目标索引/暂存区 | `-S` / `--staged` | `-S` / `--staged` | N/A（没有暂存区） |
| 两个目标 | `-S -W` | `-S -W` | N/A |
| 从文件读取 pathspec | `--pathspec-from-file <FILE>` / `--pathspec-file-nul` | `--pathspec-from-file` / `--pathspec-file-nul` | N/A |
| Overlay 模式 | `--overlay` / `--no-overlay`（最后给出的生效；默认 no-overlay=移除缺失路径） | `--overlay` / `--no-overlay` | N/A |
| 冲突解决 | `--ours` / `-2`、`--theirs` / `-3`、`--merge`、`--conflict=merge\|diff3`（仅工作树；整文件标记） | `--ours` / `--theirs` / `--merge` / `--conflict` | `--restore-descendants` |
| 跳过未合并 | `--ignore-unmerged` | `--ignore-unmerged` | N/A |
| Patch 模式 | 不支持 | `-p` / `--patch` | N/A |
| 不显示进度条 | `--no-progress`（no-op；从不渲染） | `--no-progress` | N/A |
| 进度条 | 不支持 | `--progress` | N/A |
| 目标修订 | 不支持 | N/A | `--to <revision>` |
| 将更改恢复到 | 不支持 | N/A | `--changes-in <revision>` |
| JSON 输出 | `--json` | 不支持 | N/A |
| Quiet 模式 | `--quiet` | 不支持 | N/A |

注意：jj 的 `restore` 作用于修订，而不是暂存区，将一个修订的内容恢复到另一个修订中。它不区分已暂存和未暂存更改。

## 错误处理

| 代码 | 条件 |
|------|-----------|
| `LBR-REPO-001` | 不是 libra 仓库 |
| `LBR-CLI-003` | 无法解析来源引用 |
| `LBR-CLI-002` | 无效路径编码 |
| `LBR-IO-001` | 无法读取索引或对象 |
| `LBR-IO-002` | 无法写入工作树文件 |
| `LBR-NET-001` | LFS 下载失败 |
| `LBR-CONFLICT-001` | 路径未合并且未给出冲突解决标志，或 `--ours`/`--theirs` 请求了不存在的冲突阶段（退出码 128） |

> `--ours` 与 `--theirs` 彼此互斥，并与 `--source`、`--staged`、`--ignore-unmerged` 互斥；任一此类组合会以 `LBR-CLI-002`、退出码 129 被拒绝。（`--source`、`--staged`、`--ignore-unmerged` 之间可以组合——例如 `--ignore-unmerged --source HEAD`。）
