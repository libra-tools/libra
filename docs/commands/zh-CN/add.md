# `libra add`

为下一次提交暂存文件内容。

## 概要

```
libra add [OPTIONS] [PATHSPEC...]
libra add -A
libra add -u [PATHSPEC...]
libra add --refresh [PATHSPEC...]
libra add --resolved [PATHSPEC...]
```

## 说明

`libra add` 将工作树中的文件更改暂存到索引中，为下一次 `libra commit` 做准备。它支持共享 Git 风格 pathspec 匹配、`--dry-run` 预览，以及用 `--refresh` 对已跟踪条目重新 stat 而不暂存新内容。

该命令相对于当前工作目录解析 pathspec，验证它们位于仓库根内，并遵守 Git/Libra ignore 来源。由 LFS 跟踪的文件会自动作为指针文件暂存。`-A` 标志会暂存整个工作树中的所有更改（新增、修改、删除），而 `-u` 只更新已跟踪文件，不添加新文件。

符号链接会按 Git 兼容的 symlink blob 暂存：索引 mode 为 `120000`，blob 内容为链接目标字节。暂存时不会跟随链接，因此指向工作树外部的 symlink 也只会被记录为链接本身，而不会读取目标文件内容。

## 选项

### `[PATHSPEC...]`

要暂存的一个或多个文件或目录。路径相对于当前目录解析。除非指定 `-A`、`-u`、`--refresh` 或 `--resolved`，否则必需。

全局 `--literal-pathspecs`（以及 `GIT_LITERAL_PATHSPECS`）会关闭通配和 `:(magic)`；`--no-literal-pathspecs` 取消。与 Git 不同，该标志也可以写在 `add` 子命令之后。

Pathspec 使用 Libra 共享的 Git 风格匹配器：普通 pathspec 匹配文件或目录前缀，支持通配符，并支持高价值 magic 形式 `:(top)`、`:/`、`:(glob)`、`:(literal)`、`:(icase)`、`:(exclude)`、`:!`、`:^`。排除 pathspec 会从正向选择中扣除；启用 `core.ignorecase` 时，匹配会按忽略大小写处理。看起来像通配符的 pathspec 也会匹配同名的字面路径或目录前缀，以保留 Git 对 bracket 文件名和目录名的行为。

```bash
libra add file.txt
libra add src/ tests/
libra add .
libra add ':(glob)src/*.rs' ':(exclude)src/generated.rs'
libra add ':(literal)literal/[abc].txt'
```

### `-A, --all`

更新索引以匹配整个工作树。暂存新文件、修改和删除。不带 pathspec 时，会更新工作树中的所有文件。与 `-u` 和 `--refresh` 互斥。

```bash
libra add -A
```

### `-u, --update`

只更新索引中已有并匹配 pathspec 的条目。暂存已跟踪文件的修改和删除，但不添加新（未跟踪）文件。pathspec 若只点到工作树里的未跟踪文件，会在任何暂存之前拒绝（`pathspec '…' did not match any file(s) known to the index`，`LBR-CLI-003`，退出码 129），索引不变。`--ignore-errors` 会跳过该校验并暂存能匹配的路径。与 `-A` 和 `--refresh` 互斥。

```bash
libra add -u
libra add -u src/
```

### `--refresh`

刷新索引中当前所有文件的条目。只更新已有索引条目的元数据（时间戳、文件大小）以匹配工作树，不添加新文件，也不移除条目。与 `-A` 和 `-u` 互斥。

```bash
libra add --refresh
```

### `-f, --force`

允许添加本来会被 ignore 规则忽略的文件。

```bash
libra add -f ignored_file.log
```

### `-n, --dry-run`

预览会暂存什么，但不实际修改索引。输出显示哪些文件会被添加、修改或移除。`-n` 对齐 Git；`-d` 也作为 Libra 兼容短别名被接受。

```bash
libra add -n file.txt
libra add --dry-run .
```

### `-v, --verbose`

产生更详细输出，显示暂存期间的逐文件动作。

```bash
libra add -v src/
```

### `--ignore-errors`

当单个路径失败时继续暂存剩余文件。失败路径会在输出中报告，但不会导致命令以错误退出。

```bash
libra add --ignore-errors src/
```

### `--pathspec-from-file <file>`

从 `<file>` 读取 pathspec（每行一个）；命令行不得同时给出 pathspec。文件中的条目使用与位置 pathspec 相同的共享匹配器和 magic 形式。值为 `-` 时从 stdin 读取（绝不打开工作树里字面名为 `-` 的文件）。换行模式按 `\n` 分割并去掉每行末尾的一个 `\r`，因此 CRLF 列表可用；空行会被忽略。列表为 NUL 分隔（例如其它工具的 `-z` 输出）时配合 `--pathspec-file-nul`——NUL 模式保留每个字节（含 CR）。

内容非 UTF-8 或文件无法读取时为致命 `LBR-IO-001`（exit 128），零写入。空列表按用法错误处理
（`nothing specified, nothing added`，exit 129）——Git 把空列表视为零操作，属有意差异。

换行模式下，以 `"` 开头的行按 Git C-style 引号字符串解码（`\n`、`\t`、`\"`、`\\`、八进制转义等），
因此含空格或引号的路径可正确传入；未加引号的行按字面使用。引号格式错误（未闭合、闭合引号后有多余
字节、未知转义）为致命 `LBR-IO-001`（exit 128），零写入。NUL 模式不解码。

`--pathspec-from-file` 不能与 `-p`/`--patch`、`--edit`、`--interactive` 或命令行 pathspec 参数共用
（Git 的 `cannot be used together` 契约）：每种组合都是用法错误（`LBR-CLI-002`，exit 129）、零写入。
（Git 对同样组合以 exit 128 拒绝；Libra 的 129 是其用法错误码——属有意差异。
`--interactive` 保留其自身的 declined-flag 拒绝：exit 128 + `LBR-UNSUPPORTED-001`。）

```bash
libra add --pathspec-from-file paths.txt
printf 'a.txt\nb.txt\n' | libra add --pathspec-from-file=-
libra add --pathspec-from-file paths.bin --pathspec-file-nul
```

### `--pathspec-file-nul`

将 `--pathspec-from-file` 输入视为 NUL 分隔，而不是换行分隔。必须与 `--pathspec-from-file` 一起使用；单独使用是用法错误。

### `--chmod=(+|-)x`

强制设置命中路径在索引中记录的可执行位：`+x` 记为 mode `100755`，`-x` 记为 `100644`。
blob 内容不变；仅 mode 变化的路径也会被报告为 modified。

只有普通文件才有可执行位。命中的符号链接（`120000`）或 gitlink（`160000`）会被拒绝：条目保持不变，
每条拒绝向 stderr 输出一行 `error: cannot chmod +x '<path>'`，其余路径照常处理后 `add` 以 1 退出。
`--json` 模式下改为在 envelope 上输出 `chmod_rejected: [{"path", "flip"}]`，退出码同为 1。
（Git 此处退出 255；Libra 采用与 ignored-path 报告共用的进程级 exit 1 模型。）

非法取值（非 `+x` / `-x`）按用法错误处理。

在没有 pathspec（也没有 `-A`、`-u`、`--refresh`、`--renormalize`、`--resolved`）时，`--chmod` 是成功的
零操作：`add` exit 0，不写索引、不写对象——因为没有可施加 mode 的目标。

```bash
libra add --chmod=+x scripts/build.sh
libra add --chmod=-x notes.txt
```

### `--renormalize`

从头重新暂存已跟踪文件，即使内容未变也重写其 blob。隐含 `-u`：只处理已跟踪文件（绝不处理
未跟踪文件）；从工作区删除的已跟踪文件会被暂存为删除。

```bash
libra add --renormalize
libra add --renormalize src/
```

### `--ignore-missing`

在 `--dry-run` 下，对没有匹配 add 候选的 pathspec 按 ignore 规则（`.libraignore`、`.gitignore`）分类：
命中 ignore 规则的路径跟其它 ignored 路径一样报告，并使 `add` 以 1 退出（若它是唯一 pathspec，
则仍走 `LBR-ADD-001` / 退出码 128 契约）；未命中 ignore 的路径则跳过并在 stderr 打印警告。与 Git
一致：`--ignore-missing` 需要配合 `--dry-run`。

```bash
libra add --dry-run --ignore-missing maybe-missing.txt other.txt
```

### `--resolved`

只暂存未合并（冲突）路径。工作树里仍含冲突标记的文件会整组拒绝（`LBR-CONFLICT-001`，退出码 128），索引不做任何写入。工作树文件已被删除的路径会从索引移除。不要求 pathspec；给出 pathspec 时只处理匹配的未合并路径。未冲突的本地修改不会被顺带暂存。

与 `-u`/`--update`、`-A`/`--all` 互斥。诊断文案为 Git 的 `options '…' and '--resolved' cannot be used together`（`LBR-CLI-002`，退出码 129）。Git 对同一组合退出 128。

```bash
libra add --resolved
libra add --resolved path/to/file
```

### `-p, --patch`

交互式逐个选择 hunk 暂存。每个 hunk 打印 unified diff，并提示
`Stage this hunk [y,n,q,a,d,s,e,p,P,?]? `（字母随当前可执行命令收缩）。
`s` 按上下文岛拆分；`e` 用 `$GIT_EDITOR` / `core.editor` 手工编辑。
`--auto-advance`（默认）在 `y`/`n` 后前进；`--no-auto-advance` 停留并在多文件时提供 `>`/`<`。
不能与 `--json`、`--machine`、`--dry-run`、`--resolved` 同用。

```bash
libra add -p
libra add -p --no-auto-advance src/main.rs
```

### `--auto-advance` / `--no-auto-advance`

后者覆盖前者。没有 `-p`/`--patch` 时 `--no-auto-advance` 为 128：
`the option '--no-auto-advance' requires '--interactive/--patch'`。

## 常用命令

```bash
libra add file.txt
libra add src/
libra add .
libra add -n file.txt
libra add --refresh
libra add --ignore-errors src/
libra add --pathspec-from-file paths.txt
libra add ':(glob)src/*.rs' ':(exclude)src/generated.rs'
libra add --chmod=+x scripts/build.sh
libra add --renormalize
libra add --resolved
libra add -p
```

未合并（冲突）路径也在同一候选集里：`add`、`add -A`、`add .`、`add -u` 会把工作树内容写入 stage 0，并在同一索引事务里删掉 stage 1–3。普通 `add` 不检查残留冲突标记（`--resolved` 会检查）。解决后的未合并路径记为 modified，而不是 new file。

## 人类可读输出

stdout 是终端时，默认人类模式才写暂存摘要。stdout 被重定向或接到管道时默认静默（与 Git 一致）。`-v` 与 `--dry-run` 总会打印。`--quiet` 仍然抑制 stdout。stderr 警告不受终端判定影响。

单个文件：

```text
add 'src/main.rs' (new file)
```

多个文件：

```text
add 'src/main.rs' (new file)
add 'src/lib.rs' (modified)
add 'old.txt' (deleted)
```

Dry-run：

```text
add 'src/main.rs' (new file)
add 'src/lib.rs' (modified)
(dry run, no files were staged)
```

被忽略文件会在 `stderr` 上产生 warning：

```text
warning: the following paths are ignored by configured ignore rules:
ignored.log
Hint: use -f if you really want to add them.
```

当部分路径已被暂存（或由 dry-run 报告）**且**另有显式 pathspec 被 ignore 时，`add` 会完成整个操作——暂存、输出、警告与 automation 事件——然后以 `1` 退出（与 Git 一致）。当**全部**路径都被 ignore 且没有其它暂存时，`add` 以 `LBR-ADD-001` / `128` 失败（与 Git 的 1 是有意差异）。`--json` 保持 stdout 的常规 data envelope（ignored 路径在 `data.ignored`），退出码为 `1`；退出码 `1` 优先于 `--exit-code-on-warning` 的 `9`。

`--quiet` 会抑制所有 `stdout` 输出，但保留 `stderr` warnings。

## 结构化输出

`libra add` 支持全局 `--json` 和 `--machine` 标志。

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- 成功时 `stderr` 保持干净

示例：

```json
{
  "ok": true,
  "command": "add",
  "data": {
    "added": ["src/main.rs"],
    "modified": ["src/lib.rs"],
    "removed": ["old.txt"],
    "refreshed": [],
    "ignored": [],
    "failed": [],
    "dry_run": false
  }
}
```

Dry-run：

```json
{
  "ok": true,
  "command": "add",
  "data": {
    "added": ["src/main.rs"],
    "modified": [],
    "removed": [],
    "refreshed": [],
    "ignored": [],
    "failed": [],
    "dry_run": true
  }
}
```

使用 `--ignore-errors` 的部分失败：

```json
{
  "ok": true,
  "command": "add",
  "data": {
    "added": ["good.txt"],
    "modified": [],
    "removed": [],
    "refreshed": [],
    "ignored": [],
    "failed": [
      {"path": "bad.bin", "message": "file too large"}
    ],
    "dry_run": false
  }
}
```

### Schema 说明

- `added` / `modified` / `removed` 对应已暂存的新文件、变更文件和删除文件
- `refreshed` 仅在使用 `--refresh` 时填充
- `ignored` 列出被 ignore 规则跳过的路径
- `failed` 列出暂存失败的路径，每个包含 `path` 和 `message`
- 传递 `-n` / `--dry-run` 时 `dry_run` 为 `true`；不会实际暂存文件

## 设计理由

### 没有 `--intent-to-add` / `-N`

Git 的 `--intent-to-add`（`-N`）会为未跟踪文件记录空 blob，使它们出现在 `git diff` 输出中，但不真正暂存其内容。这是为了在暂存前审查新文件的工作流便利。Libra 省略该标志，因为 `libra status` 已经清楚显示未跟踪文件，且 `libra diff` 设计为配合完整工作树状态工作。“intent 然后 stage”的两步工作流增加认知负担，却没有显著改善审查体验。想在提交前审查新文件的用户可以使用 `libra add --dry-run`，暂存后再使用 `libra diff --staged`。

### `--patch` / `-p` 交互式暂存

`libra add -p` 是与 Git 兼容的 hunk 会话（`y/n/q/a/d/j/J/k/K/g///s/e/p/P/?`、
`--[no-]auto-advance`）。`--json` / `--machine` / `--dry-run` 仍拒绝与 patch 会话组合，
以便代理保持非交互路径。`add -i` 仍按 D15 延后。

### `--refresh` 作为显式标志

在 Git 中，`git add --refresh` 会静默更新已跟踪文件的 stat 信息。Libra 将其作为一等模式暴露，并与 `-A` 和 `-u` 互斥（由 clap 参数组强制）。这让意图明确：`--refresh` 永远不暂存新内容，只更新元数据。互斥性避免 `-A --refresh` 这种意图模糊的组合。

### Ignore 来源优先级

Libra 会读取 Git 标准 ignore 文件（`.gitignore`、worktree 本地
`info/exclude`——即当前 worktree 自己 gitdir 下的 `.libra/info/exclude`，
Git 或双布局树还包括 `.git/info/exclude`——和 `core.excludesFile`）以及 Libra
扩展文件（`.libraignore`）。同一目录内 `.libraignore` 比 `.gitignore`
优先；更近目录的来源优先于祖先目录；`info/exclude` 和 `core.excludesFile`
是较低优先级 fallback。`info/exclude` 按 worktree 独立（绝不经 `commondir`
共享；见 [check-ignore.md](check-ignore.md)）。所有来源都使用 Git ignore
模式语法。

`libra init` 仍会在非 bare 仓库中创建根 `.libraignore`，以便保存 Libra 专用规则；Git 导入或非 bare clone 仍会把已有 `.gitignore` 文件复制为匹配的 `.libraignore` 文件，方便显式覆盖。

## 参数对比：Libra vs Git vs jj

| 参数 / 标志 | Git | jj | Libra |
|---|---|---|---|
| 暂存文件 | `git add file.txt` | N/A（jj 自动跟踪） | `libra add file.txt` |
| 暂存所有内容 | `git add .` 或 `git add -A` | N/A（自动） | `libra add .` 或 `libra add -A` |
| 只更新已跟踪 | `git add -u` | N/A | `libra add -u` |
| Dry-run 预览 | `git add -n` / `--dry-run` | N/A | `libra add -n` / `--dry-run` |
| 强制添加被忽略文件 | `git add -f` | N/A | `libra add -f` |
| 刷新 stat 信息 | `git add --refresh` | N/A | `libra add --refresh` |
| Verbose 输出 | `git add -v` | N/A | `libra add -v` |
| 忽略错误 | `git add --ignore-errors` | N/A | `libra add --ignore-errors` |
| Intent to add | `git add -N` / `--intent-to-add` | N/A | N/A（未实现） |
| 交互式 patch | `git add -p` / `--patch` | N/A | `libra add -p` / `--patch` |
| 交互式选择 | `git add -i` / `--interactive` | N/A | N/A（使用 `libra code` Web Code UI） |
| 暂存前编辑 diff | `git add -e` / `--edit` | N/A | N/A |
| 仅 chmod | `git add --chmod=+x` | `libra add --chmod=+x`（非普通索引条目会被拒绝并以 exit 1 结束） | N/A |
| Sparse checkout 路径 | `git add --sparse` | N/A | N/A |
| Ignore 文件 | `.gitignore` | N/A（jj 使用 `.gitignore`） | `.gitignore` + `.libraignore` |
| 结构化 JSON 输出 | N/A | N/A | `--json` / `--machine` |
| 错误提示 | 最少 | N/A | 每种错误类型都有可操作提示 |

## 错误处理

每个 `AddError` 变体都会映射到显式 `StableErrorCode`。

本地暂存与后续云目录更新具有独立的耐久边界。若 blob 与 index 已保存后，后台
`object_index` 更新遇到终止错误，`add` 保留正常成功输出、向 stderr 发出可操作警告，
并留下原子 repair marker，供下一条具备 schema 感知的仓库命令自动重试。修复仍待处理时，
`cloud sync` 与破坏性 agent cleanup 会 fail closed。使用 `--exit-code-on-warning` 时，
已完成的本地暂存返回退出码 9 / `LBR-WARN-001`；无需重跑 `add`。

| 场景 | 错误码 | 退出码 | 提示 |
|----------|-----------|------|------|
| 不在仓库内 | `LBR-REPO-001` | 128 | "run 'libra init' to create a repository" |
| Pathspec 没有匹配 | `LBR-CLI-003` | 129 | "check the spelling and use 'libra status' to see what changed" |
| 路径在仓库根外 | `LBR-CLI-003` | 129 | "only files within the repository root can be staged" |
| 无效路径编码 | `LBR-CLI-003` | 129 | "path contains invalid UTF-8 characters" |
| 索引文件损坏 | `LBR-REPO-002` | 128 | "the index file may be corrupted; try 'libra status' to verify" |
| 无法保存索引 | `LBR-IO-002` | 128 | "check disk space and file permissions" |
| Refresh 失败 | `LBR-IO-001` | 128 | -- |
| 条目创建失败 | `LBR-IO-002` | 128 | -- |
| 对象或持久索引 marker 写入失败 | `LBR-IO-002` | 128 | 检查存储权限后重试；错误会正常返回、不会 panic，且暂存区保持不变：错误信息说明对象负载已安全写入、未暂存任何路径，直接重试会复用已存储的负载、无需任何锁文件清理。若失败形态为锁超时，错误信息会指出持有者（其 pid 与用途，如 `marker_publication`、`queued_update`、`replay`、`deletion_fence`）或说明无法判定持有者；等待该进程结束后重试即可。锁等待采用 Git 式二次退避，最长等待 10 秒。不要删除 `.libra/object-index-repair-locks` 下的锁文件：它们只用于仲裁并发写入，本身不会阻塞任何操作，且会在持有进程退出时自动释放。只读命令在锁忙时会静默跳过待回放的云索引 repair marker（无告警），由下一条命令重试 |
| 路径已暂存但云索引修复仍待处理，且使用 `--exit-code-on-warning` | `LBR-WARN-001` | 9 | 修复警告中的数据库/marker 问题；下一条仓库命令会自动重试 |
| 工作目录错误 | `LBR-REPO-001` | 128 | "cannot determine the working tree" |
| 状态计算失败 | `LBR-REPO-002` | 128 | -- |
| 所有路径都被忽略（未暂存任何内容） | `LBR-ADD-001` | 128 | "use -f if you really want to add them" |
| 无 pathspec 且无模式标志 | `LBR-CLI-001` | 129 | "maybe you wanted to say 'libra add .'?" |
| `add -u` 的 pathspec 是未跟踪文件 | `LBR-CLI-003` | 129 | "did not match any file(s) known to the index" |
| `--resolved` 与 `-u` 或 `-A` 同用 | `LBR-CLI-002` | 129 | Git 的 `cannot be used together` 文案（Git 自身退出 128） |
| `--resolved` 时工作树仍含冲突标记 | `LBR-CONFLICT-001` | 128 | 列出全部仍含标记的路径；不写索引 |

## 兼容性说明

- jj 没有 `add` 命令；它自动跟踪所有工作树更改
- Libra 的 `add` 是 `commit` 前必需步骤，匹配 Git 的显式暂存模型
- `.gitignore` 与 `.libraignore` 都使用 Git ignore 模式语法；同目录内 `.libraignore` 可显式覆盖 `.gitignore`，导入和非 bare clone 仍会复制 `.gitignore` 规则，而不是删除或重命名原文件
- 未被 ignore 的缺失 pathspec 仍会输出 Libra 的 `--ignore-missing` 跳过 warning（Git 此时静默）
- 目录 pathspec 下，已存在的被忽略父目录不会被额外列出（Git 也会列出它）
- C-quoted 的 `--pathspec-from-file` 行可接受 1–3 位八进制数字（Git 要求恰好三位），且会拒绝闭合引号之后的剩余字节（Git 忽略它们）
- LFS 跟踪文件会在暂存期间自动转换为指针文件
- 其余仍不支持的交互选项以 `LBR-UNSUPPORTED-001` 拒绝（`-i`/`--interactive`，D15 剩余入口）。请用 `libra add -p` 或 `libra add <pathspec>`。

## Issue #477 notes

仍不支持的交互入口返回 `LBR-UNSUPPORTED-001`
