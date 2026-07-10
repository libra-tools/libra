# `libra grep`

在已跟踪文件中搜索模式。

## 概要

```
libra grep [<options>] <pattern> [-- <pathspec>...]
libra grep -e <pattern> [-e <pattern>...] [-- <pathspec>...]
libra grep -f <file> [-- <pathspec>...]
```

## 说明

`libra grep` 在已跟踪文件中搜索文本模式。默认搜索工作树，但也可以搜索索引（`--cached`）或特定修订（`--tree <revision>`）。除非指定 `--fixed-string`，否则模式按正则表达式解释。

可以通过多个 `-e` 标志提供多个模式，或通过 `-f` 从文件读取。存在多个活动模式时，只要某个文件至少有一行匹配任一模式，该文件即匹配（OR 语义）。使用 `--all-match` 时，只有每个模式都至少在该文件中匹配一次，文件才会被包含（模式之间的 AND 语义，不是行之间的 AND）。

输出可通过标志调整为只显示文件名（`-l`、`-L`）、每文件匹配数量（`-c`）、行号（`-n`）、字节偏移（`-b`）或反向匹配（`-v`）。命令支持 pathspec 过滤，以将搜索限制到特定文件或目录。仓库内搜索使用共享 pathspec 引擎，支持 `:(top)`、`:(exclude)`、`:(icase)`、`:(literal)`、`:(glob)` magic。

当 stdout 是终端时，输出会通过分页器发送。在 JSON 模式下，会输出适合程序消费的结构化结果。

当 stdout 被管道连接且下游命令提前退出时，`libra grep` 会静默正常结束，不打印 panic/backtrace 或 `Broken pipe` 诊断。

退出码遵循 Git 的 grep 合同：有匹配时退出 0；没有选中匹配时退出 1，且不打印错误诊断；grep 命令错误（例如无效正则、不支持的 `-P`、缺失 pattern 文件或无效 `--tree` 修订）退出 2。仓库模式在 Libra 仓库外运行等仓库预检错误仍使用标准 Libra fatal 退出码。

## 选项

| 标志 | 短选项 | 长选项 | 说明 |
|------|-------|------|-------------|
| Pattern | | 位置参数 | 要搜索的模式。除非指定 `-e` 或 `-f`，否则必需。 |
| Regexp | `-e` | `--regexp <PATTERN>` | 添加要搜索的模式。可多次指定。 |
| Pattern file | `-f` | `--file <FILE>` | 从文件读取模式，每行一个。可多次指定。 |
| All match | | `--all-match` | 要求所有模式都至少在一个文件中匹配一次，该文件才会被包含。 |
| Fixed string | `-F` | `--fixed-string` | 将模式视为字面字符串，而不是正则表达式。 |
| Ignore case | `-i` | `--ignore-case` | 执行大小写不敏感匹配。 |
| Count | `-c` | `--count` | 只显示每个文件的匹配行数。 |
| Files with matches | `-l` | `--files-with-matches` | 只显示包含匹配的文件名。 |
| Files without matches | `-L` | `--files-without-matches` | 只显示不包含匹配的文件名。 |
| Line number | `-n` | `--line-number` | 为每个匹配行加上 1-based 行号前缀。 |
| Word regexp | `-w` | `--word-regexp` | 仅匹配模式构成完整单词的行（由单词边界包围）。 |
| Invert match | `-v` | `--invert-match` | 选择不匹配的行，而不是匹配行。 |
| Byte offset | `-b` | `--byte-offset` | 显示每行第一个匹配的 0-based 字节偏移。 |
| Max count | `-m` | `--max-count <NUM>` | 每个文件匹配 NUM 行后停止。 |
| Only matching | `-o` | `--only-matching` | 只打印行中匹配的部分，每个匹配一行（上下文行被抑制）。 |
| Pathspec | | 尾随位置参数 | 将搜索限制到匹配给定路径的文件。 |
| Tree | | `--tree <REVISION>` | 在指定修订或提交树中搜索，而不是工作树。 |
| Cached | | `--cached` | 在索引（暂存区）中搜索，而不是工作树。 |
| Untracked | | `--untracked` | 除已跟踪文件外，还搜索工作树中未跟踪、非忽略的文件。不能与 `--cached` 或 `--tree` 同用。 |
| No index | | `--no-index` | 直接搜索文件系统（给定路径或当前目录），不使用仓库或索引。可在仓库外使用，递归遍历每个文件（包括被忽略的，跳过 `.git`/`.libra`），显示相对当前目录的路径。不能与 `--cached`、`--untracked` 或 `--tree` 同用。 |
| Max depth | | `--max-depth <DEPTH>` | 每个 pathspec 下最多下降 DEPTH 层目录。直接位于 pathspec 目录内的文件深度为 0；负值表示无限制。未给 pathspec 时从工作树根度量深度（而非当前目录）——`libra grep` 始终搜索整个工作树并使用工作树相对路径，若要限定到某子目录，请将其作为 pathspec 传入。 |

### 选项细节

**位置 `<pattern>`**

主要搜索模式。默认解释为正则表达式：

```bash
$ libra grep "fn\s+execute"
src/command/merge.rs:pub async fn execute(args: MergeArgs) {
src/command/rebase.rs:pub async fn execute(args: RebaseArgs) {
```

**`-e` / `--regexp`**

添加额外模式。与 `--all-match` 组合时，所有模式都必须在文件中匹配：

```bash
# 查找同时包含 "TODO" 和 "FIXME" 的文件
$ libra grep -e "TODO" -e "FIXME" --all-match
```

**`-f` / `--file`**

从文件读取模式，每行一个：

```bash
$ libra grep -f patterns.txt
```

**`-F` / `--fixed-string`**

将模式视为字面字符串。适合搜索包含正则元字符的字符串：

```bash
$ libra grep -F "Vec<String>"
```

**`-i` / `--ignore-case`**

大小写不敏感匹配：

```bash
$ libra grep -i "error"
```

**`-c` / `--count`**

显示匹配数量而不是匹配行：

```bash
$ libra grep -c "TODO"
src/main.rs:2
src/lib.rs:5
```

**`-l` / `--files-with-matches`**

只显示有匹配的文件名：

```bash
$ libra grep -l "TODO"
src/main.rs
src/lib.rs
```

**`-L` / `--files-without-matches`**

只显示无匹配的文件名：

```bash
$ libra grep -L "TODO"
src/utils.rs
```

**`-n` / `--line-number`**

显示行号：

```bash
$ libra grep -n "TODO"
src/main.rs:42:// TODO: refactor this
```

**`-w` / `--word-regexp`**

只匹配完整单词：

```bash
# 匹配 "error"，但不匹配 "errors" 或 "error_handler"
$ libra grep -w "error"
```

**`-v` / `--invert-match`**

显示不匹配的行：

```bash
$ libra grep -v "^$" src/main.rs
```

**`-b` / `--byte-offset`**

显示匹配的字节偏移：

```bash
$ libra grep -b "TODO"
src/main.rs:1024:// TODO: refactor
```

**`--tree`**

在特定修订中搜索：

```bash
$ libra grep --tree HEAD~3 "deprecated"
$ libra grep --tree v1.0 "config"
```

**`--cached`**

在索引中搜索，而不是工作树：

```bash
$ libra grep --cached "TODO"
```

## 常用命令

```bash
# 在已跟踪文件中搜索模式
libra grep "TODO"

# 大小写不敏感正则搜索
libra grep -i "error|warning"

# 搜索字面字符串
libra grep -F "HashMap<String, Vec<u8>>"

# 只显示有匹配的文件名
libra grep -l "deprecated"

# 显示匹配数量
libra grep -c "unwrap()"

# 带行号搜索
libra grep -n "fn main"

# 在特定修订中搜索
libra grep --tree HEAD~5 "old_function"

# 在索引中搜索
libra grep --cached "staged_change"

# 限制到特定路径
libra grep "TODO" -- src/command/

# 多模式（OR）
libra grep -e "TODO" -e "FIXME" -e "HACK"

# 多模式（文件级 AND）
libra grep -e "use.*serde" -e "Serialize" --all-match
```

## 人类可读输出

默认输出（file:line 格式）：

```text
src/main.rs:// TODO: refactor this function
src/lib.rs:// TODO: add error handling
```

带行号（`-n`）：

```text
src/main.rs:42:// TODO: refactor this function
src/lib.rs:15:// TODO: add error handling
```

带字节偏移（`-b`）：

```text
src/main.rs:1024:// TODO: refactor this function
```

Count 模式（`-c`）：

```text
src/main.rs:1
src/lib.rs:3
```

Files-with-matches 模式（`-l`）：

```text
src/main.rs
src/lib.rs
```

Files-without-matches 模式（`-L`）：

```text
src/utils.rs
src/config.rs
```

Tree 搜索（`--tree`）：

```text
HEAD~3:src/main.rs:// TODO: old code
```

## 结构化输出（JSON）

```json
{
  "pattern": "TODO",
  "patterns": ["TODO"],
  "context": "working-tree",
  "total_matches": 5,
  "total_files": 2,
  "matches": [
    {
      "path": "src/main.rs",
      "line_number": 42,
      "line": "// TODO: refactor this function",
      "byte_offset": null
    }
  ],
  "counts": null,
  "files_with_matches": null,
  "files_without_matches": null,
  "warnings": []
}
```

使用 `--count` 时：

```json
{
  "pattern": "TODO",
  "patterns": ["TODO"],
  "context": "working-tree",
  "total_matches": 5,
  "total_files": 2,
  "matches": null,
  "counts": [
    { "path": "src/main.rs", "count": 2 },
    { "path": "src/lib.rs", "count": 3 }
  ],
  "warnings": []
}
```

使用 `-l` 时：

```json
{
  "pattern": "TODO",
  "patterns": ["TODO"],
  "context": "working-tree",
  "total_matches": 5,
  "total_files": 2,
  "matches": null,
  "files_with_matches": ["src/main.rs", "src/lib.rs"],
  "warnings": []
}
```

`warnings` 数组包含无法读取或被跳过文件的条目（例如二进制文件）：

```json
{
  "path": "assets/image.png",
  "message": "binary file, skipping"
}
```

## 设计理由

### 为什么内置而不是依赖外部 grep/ripgrep？

`grep`、`rg` 或 `ag` 等外部工具非常适合搜索磁盘上的文件，但它们不知道版本控制状态。它们不能：

- **搜索索引**：`--cached` 搜索暂存内容，这可能与工作树不同。外部工具只能看到工作树。
- **搜索历史修订**：`--tree` 在不 checkout 的情况下搜索特定提交内容。外部工具需要将整棵树提取到临时目录。
- **遵守已跟踪文件语义**：Libra 的 grep 默认只搜索已跟踪文件，自动排除未跟踪和被忽略文件，不需要单独 ignore 配置。
- **产生结构化输出**：JSON 输出在单个可解析结构中包含总计数、文件列表和 warnings 等元数据。

对于纯工作树搜索，外部工具通常更快。Libra 的 grep 用原始速度换取版本控制集成。

### 为什么用 `--tree` 搜索修订？

`--tree` 标志搜索特定修订 tree 对象的内容，直接从对象存储读取 blob 数据。这等价于 `git grep <revision>`，但使用具名标志而不是位置参数，以避免模式和修订 specifier 之间的歧义。

Git 的位置语法（`git grep <pattern> <revision>`）是常见困惑来源，因为解析器必须区分 pattern 和 revision。Libra 通过要求显式 `--tree` 标志避免这种歧义。

### 为什么用 `--cached` 搜索索引？

当文件已暂存但尚未提交时，索引（暂存区）可能包含与工作树不同的内容。搜索索引有助于验证即将提交的内容，尤其适合脚本和 CI 工作流。

### 为什么默认正则？

正则表达式是代码搜索的标准模式语言。多数开发者期望 `grep` 支持正则，Libra 遵循此约定。当正则元字符会造成问题时，可以使用 `--fixed-string` 标志进行字面搜索。

### 这与 Git 和 jj 如何比较？

Git 的 `grep` 范围类似：它用正则支持搜索已跟踪文件，提供 `-l`、`-c`、`-n`、`-w`、`-v`、`-i`，并可搜索修订和索引。Libra 的实现覆盖同一核心功能集，并增加结构化 JSON 输出。

jj 没有内置 grep 命令。用户需要使用外部工具进行文本搜索。这对工作树搜索效果很好，但意味着没有集成方式可以在不 checkout 的情况下搜索历史修订。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| Pattern | 位置参数 | 位置参数 | N/A（无 grep） |
| 额外模式 | `-e` / `--regexp` | `-e` | N/A |
| 模式文件 | `-f` / `--file` | `-f` | N/A |
| All match | `--all-match` | `--all-match` | N/A |
| 字面字符串 | `-F` / `--fixed-string` | `-F` / `--fixed-strings` | N/A |
| 忽略大小写 | `-i` / `--ignore-case` | `-i` / `--ignore-case` | N/A |
| Count | `-c` / `--count` | `-c` / `--count` | N/A |
| 有匹配的文件 | `-l` / `--files-with-matches` | `-l` / `--files-with-matches` | N/A |
| 无匹配的文件 | `-L` / `--files-without-matches` | `-L` / `--files-without-match` | N/A |
| 行号 | `-n` / `--line-number` | `-n` / `--line-number` | N/A |
| 单词正则 | `-w` / `--word-regexp` | `-w` / `--word-regexp` | N/A |
| 反向匹配 | `-v` / `--invert-match` | `-v` / `--invert-match` | N/A |
| 字节偏移 | `-b` / `--byte-offset` | 不支持 | N/A |
| Pathspec | 尾随位置参数 | 尾随位置参数 | N/A |
| 修订搜索 | `--tree <REVISION>` | `<revision>`（位置参数） | N/A |
| 索引搜索 | `--cached` | `--cached` | N/A |
| 上下文行 | `-A` / `-B` / `-C` | `-C` / `-A` / `-B` | N/A |
| 扩展正则 | `-E` / `--extended-regexp` | `-E` / `--extended-regexp` | N/A |
| Perl 正则 | 拒绝（退出 2） | `-P` / `--perl-regexp` | N/A |
| 最大匹配数 | `-m` / `--max-count` | `-m` / `--max-count` | N/A |
| 仅匹配部分 | `-o` / `--only-matching` | `-o` / `--only-matching` | N/A |
| 显示函数 | 不支持 | `-p` / `--show-function` | N/A |
| 最大深度 | `--max-depth <DEPTH>` | `--max-depth <DEPTH>` | 给定 pathspec 时等价（深度相对 pathspec 度量）。未给 pathspec 时，Libra 从工作树根度量深度而非当前目录，因为 `libra grep` 始终搜索整个工作树并使用工作树相对路径——若要限定到某目录，请将其作为 pathspec 传入。 |
| 线程 | 不支持 | `--threads` | N/A |
| 颜色 | 自动（终端检测） | `--color` | N/A |
| JSON 输出 | 内置 JSON 结构 | 不支持 | N/A |

注意：jj 没有内置 grep 命令。用户依赖 `grep`、`rg` 或 `ag` 等外部工具进行文本搜索。

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 不是 libra 仓库 | `LBR-REPO-001` | 128 |
| 未提供模式（且没有 `-e` 或 `-f`） | Clap 参数错误 | 2 |
| 无效正则模式 | `LBR-CLI-002`（CliInvalidArguments） | 2 |
| 找不到修订（`--tree`） | `LBR-CLI-003`（CliInvalidTarget） | 2 |
| 未找到匹配 | 仅状态信号 | 1 |
| 无法读取文件（非致命） | 输出中的 warning，跳过文件 | 0 |
| 无法读取模式文件（`-f`） | 带文件路径详情的错误 | 2 |
