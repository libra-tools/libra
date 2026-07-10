# `libra archive`

从已提交的 tree 快照创建归档。

## 概要

```bash
libra archive [OPTIONS] [TREEISH] [PATH]...
libra archive --list
```

## 说明

`libra archive` 类似 `git archive`：它解析一个提交、分支、标签或缩写提交哈希，遍历该提交 tree，并把已跟踪文件写成归档。该命令不会修改工作树或索引。

省略 `TREEISH` 时，命令归档 `HEAD`。默认格式是写到 stdout 的未压缩 tar 流。从交互式 shell 运行时，请使用 `--output <FILE>`，这样二进制归档字节会写入文件，而不是写到终端。若在 `TREEISH` 后提供 `PATH` 参数，则只归档该提交 tree 内匹配的文件或目录。

如果某个路径被被归档 tree 内 `.gitattributes` 或 `.libra_attributes`
文件中的 `export-ignore` 属性命中，该条目会从归档中省略。未提交的工作树
attributes 修改不会影响对既有 `TREEISH` 的归档。`export-subst` 尚未实现。

## 选项

| 标志 | 短参数 | 说明 | 默认值 |
|------|--------|------|--------|
| `[TREEISH]` | | 要归档的提交、分支、标签或缩写提交哈希 | `HEAD` |
| `[PATH]...` | | 将归档限制为 `TREEISH` 内匹配的文件或目录 | 所有文件 |
| `--list` | `-l` | 列出支持的归档格式并退出 | false |
| `--format <FMT>` | `-f` | 归档格式：`tar`、`tar.gz`、`tgz`、`tar.bz2`、`tbz2`、`tbz` 或 `zip` | `tar` |
| `--output <FILE>` | `-o` | 将归档字节写入文件，而不是 stdout | stdout |
| `--prefix <PREFIX>` | | 为每个归档路径添加相对目录前缀 | 无 |
| `--verbose` | `-v` | 将每个归档路径（已应用 prefix）作为进度报告到 stderr | false |
| `--add-file=<file>` | | 将未跟踪的工作树文件按其 basename 加入归档（位于 `--prefix` 下）。可重复；不受 `[PATH]...` 过滤。必须出现在 `[TREEISH]` 之前。 | 无 |
| `--compression-level <0-9>` | | `tar.gz`/`tar.bz2`/`zip` 的压缩级别（普通 `tar` 忽略）。这是 Git 的 `-0`..`-9`，clap 无法建模为裸数字标志。bzip2 没有级别 0，因此 0 按 1 处理。 | 格式默认值 |
| `--mtime <time>` | | 设置所有归档条目的修改时间（与 `--since`/`--until` 相同的日期格式：`YYYY-MM-DD`、RFC 3339、相对时间或 Unix 时间戳）。 | 被归档提交的 committer time |

`--prefix <PREFIX>` 必须是相对路径。绝对前缀和包含 `..` 路径组件的前缀会被拒绝，以防止归档路径穿越。

`PATH` 参数也必须是相对路径且不得包含 `..`。目录 pathspec 会包含该目录下所有匹配文件。`--list` 不要求位于仓库中。

## 示例

```bash
# 将 HEAD 写成未压缩 tar 归档。
libra archive -o project.tar

# 写出 gzip 压缩的发布归档。
libra archive --format=tar.gz --prefix=project-v1.0/ -o project-v1.0.tar.gz v1.0

# 使用短格式标志写出 bzip2 压缩归档。
libra archive -f tbz2 -o project.tar.bz2 HEAD

# 为一个分支写出 zip 归档。
libra archive --format=zip -o feature.zip feature-branch

# 列出支持的格式。
libra archive --list

# 只归档 HEAD 中 src/ 下的文件。
libra archive -o src.tar HEAD src/

# 把未跟踪文件（例如发布说明）连同 tree 一起纳入归档。
libra archive --add-file=RELEASE_NOTES.txt -o release.tar HEAD
```

## 输出

成功时，`libra archive` 会把归档字节写到 stdout，或写到 `--output <FILE>` 指定的路径。它不会额外打印成功消息。

Tar 归档会保留普通文件、可执行文件 mode、符号链接、嵌套路径、空文件和 Unicode 文件名。Zip 归档会先在内存中构建，因为 zip writer 需要可 seek 输出，然后再刷新到请求的目标。

## 错误处理

| 场景 | StableErrorCode |
|------|-----------------|
| 未知 `TREEISH` 或空仓库 | `LBR-CLI-003` |
| `PATH` 未匹配任何归档文件 | `LBR-CLI-003` |
| 未知 `--format <FMT>` 值 | `LBR-CLI-002` |
| 不安全的 `--prefix <PREFIX>` | `LBR-CLI-002` |
| `--add-file=<file>` 路径缺失或不可读 | `LBR-IO-001` |
| `--add-file=<file>` 不是普通文件（例如目录） | `LBR-CLI-002` |
| 不安全的 `PATH` pathspec | `LBR-CLI-002` |
| 无法读取引用的仓库对象 | `LBR-REPO-002` |
| 无法读取 blob 内容 | `LBR-IO-001` |
| 无法创建或写入输出文件 | `LBR-IO-002` |

失败输出使用 Libra 标准结构化 CLI 错误报告。
