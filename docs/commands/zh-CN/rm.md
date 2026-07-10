# `libra rm`

从工作树和/或索引中移除文件。

**别名：** `remove`, `delete`

## 概要

```
libra rm [--json|--machine] <pathspec>...
libra rm [--json|--machine] --cached <pathspec>...
libra rm [--json|--machine] -r <pathspec>...
libra rm [--json|--machine] --dry-run <pathspec>...
```

## 说明

`libra rm` 从工作树和索引中移除文件。默认情况下，它会从磁盘删除文件并取消暂存，从而在下一次提交中记录删除。使用 `--cached` 时，只移除索引条目，文件仍保留在磁盘上，这适合停止跟踪误添加的文件，同时不丢失本地更改。

移除目录需要 `-r`（recursive）标志。没有该标志时，指定目录路径会产生错误。传入 `-r` 后，Libra 只删除 pathspec 匹配到的已跟踪文件，并仅在目录变空后清理空父目录；未跟踪文件和被 exclude 的已跟踪文件会让目录继续保留在磁盘上。

Pathspec 使用 Libra 共享的 Git 风格匹配器：普通 pathspec 匹配文件或目录前缀，支持通配符，并支持高价值 magic 形式 `:(top)`、`:/`、`:(glob)`、`:(literal)`、`:(icase)`、`:(exclude)`、`:!`、`:^`。排除 pathspec 会从正向选择中扣除；启用 `core.ignorecase` 时，匹配会按忽略大小写处理。看起来像通配符的 pathspec 也会匹配同名的字面路径或目录前缀，以保留 Git 对 bracket 文件名和目录名的行为。

移除文件前，Libra 会检查未提交更改（包括已暂存和未暂存）。如果文件相对索引有本地修改，或索引与 HEAD 不同，除非传递 `--force` 或使用 `--cached`，否则命令会拒绝继续。此安全检查可防止文件有未保存工作时发生静默数据丢失。

别名：`remove`、`delete`。三个名称都会调用同一命令。

## 选项

| 标志 | 短选项 | 长选项 | 说明 |
|------|-------|------|-------------|
| Pathspec | | 位置参数 | 要移除的一个或多个文件或目录。支持共享 pathspec magic；除非使用 `--pathspec-from-file`，否则必需。 |
| Cached | | `--cached` | 只从索引中移除；保留工作树文件。 |
| Recursive | `-r` | `--recursive` | 指定目录时允许递归移除。 |
| Force | `-f` | `--force` | 强制移除，绕过未提交更改安全检查。 |
| Dry run | | `--dry-run` | 显示会被移除的内容，但不实际删除任何东西。 |
| Ignore unmatch | | `--ignore-unmatch` | 即使没有 pathspec 匹配任何文件，也以零状态退出。 |
| Pathspec from file | | `--pathspec-from-file <FILE>` | 从文件读取共享匹配器 pathspec，每行一个。 |
| NUL separator | | `--pathspec-file-nul` | Pathspec 文件条目使用 NUL 字节而不是换行分隔。 |
| Sparse | | `--sparse` | 为 Git 兼容按 no-op 接受。Git 用它允许移除 sparse-checkout cone 之外的索引条目；Libra 没有 sparse-checkout 状态，故不改变任何行为。 |

### 选项细节

**`--cached`**

取消暂存文件但保留工作树副本。运行 `libra rm --cached secret.env` 后，该文件会从索引中消失（并在下一次提交中显示为 "deleted"），但文件仍留在磁盘上。这是不删除文件而停止跟踪文件的标准方式。

```bash
$ libra rm --cached config/local.toml
rm 'config/local.toml'
```

**`-f` / `--force`**

绕过有未提交更改文件的安全检查。通常 Libra 会在以下情况下拒绝移除文件：
1. 工作树版本与索引不同（本地修改）。
2. 索引版本与 HEAD 不同（已暂存更改）。
3. 两个条件同时成立。

使用 `--force` 时，不管这些情况如何都会移除文件。

**`--dry-run`**

显示会移除什么，但不触碰文件系统或索引：

```bash
$ libra rm --dry-run src/old_module.rs tests/old_test.rs
rm 'src/old_module.rs'
rm 'tests/old_test.rs'
```

**`--pathspec-from-file`**

从文件而不是命令行参数读取共享匹配器 pathspec。与 `--pathspec-file-nul` 结合时，支持包含换行或其他特殊字符的文件名：

```bash
$ libra rm --pathspec-from-file files-to-remove.txt
$ libra rm --pathspec-from-file files.txt --pathspec-file-nul
```

## 常用命令

```bash
# 从索引和磁盘移除单个文件
libra rm src/deprecated.rs

# 停止跟踪文件但保留在磁盘上
libra rm --cached .env

# 递归移除目录
libra rm -r old_module/

# 预览会移除什么
libra rm --dry-run -r build/
libra --json rm --dry-run -r build/

# 强制移除有本地修改的文件
libra rm -f src/experimental.rs

# 移除清单中列出的文件
libra rm --pathspec-from-file cleanup-list.txt

# 从索引中移除 Rust 文件，但保留生成文件
libra rm --cached ':(glob)src/*.rs' ':(exclude)src/generated.rs'

# 从索引移除，如果文件未被跟踪则忽略
libra rm --cached --ignore-unmatch generated.rs
```

## 人类可读输出

每个被移除文件各自报告一行：

```text
rm 'src/deprecated.rs'
rm 'old_module/foo.rs'
rm 'old_module/bar.rs'
```

在 `--dry-run` 模式下，会产生相同输出，但不会修改文件。

全局 `--quiet` 会抑制主要人类可读输出，同时保留 stderr 上的警告和错误。

## JSON 输出

`--json` 和 `--machine` 使用 `rm` 命令信封。`paths` 包含所有匹配到、将从索引移除的已跟踪文件。`directories` 包含递归 plain 目录 pathspec，并报告已跟踪文件删除、空父目录清理后该目录是否实际从磁盘消失。在 `--dry-run` 中，会报告相同候选路径，但 `removed_from_index` 和 `removed_from_disk` 为 `false`。

```json
{
  "ok": true,
  "command": "rm",
  "data": {
    "pathspecs": ["src/deprecated.rs"],
    "paths": [
      {
        "path": "src/deprecated.rs",
        "removed_from_index": true,
        "removed_from_disk": true
      }
    ],
    "directories": [],
    "cached": false,
    "recursive": false,
    "forced": false,
    "dry_run": false
  }
}
```

`--machine` 以紧凑单行 JSON 输出同一信封。

## 设计理由

### 为什么有别名 `remove` 和 `delete`？

`rm` 对 Git 用户来说简短且熟悉，但不够自解释。`remove` 在脚本和文档中读起来自然。`delete` 匹配许多开发者首先想到的词汇。支持三个名称可以减少摩擦，而不增加任何实现复杂度；它们是映射到同一 handler 的 clap aliases。

### 为什么有 `--pathspec-from-file`？

以编程方式移除大量文件时（例如 CI 清理步骤或迁移脚本），可能会达到命令行参数限制。`--pathspec-from-file` 通过从文件读取路径来避免这个问题。`--pathspec-file-nul` 变体可安全处理包含空格或换行的路径名，遵循与 `git rm --pathspec-from-file` 相同的约定。

### 为什么检查未提交更改？

移除有本地修改的文件会静默销毁工作。Git 在相同场景中要求 `--force`。Libra 完全遵循此约定：如果工作树与索引不同，或索引与 HEAD 不同，命令会报错，并解释要使用哪个标志（`--cached` 保留文件，`-f` 强制删除）。这两个逃生标志让用户能清楚表达意图。

### 为什么没有命令专用 `--quiet` 标志？

与 `libra clean` 不同，`rm` 命令没有命令专用 quiet 标志。使用全局 `--quiet` 标志来抑制主要 stdout，同时保留警告和错误。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 基本移除 | `libra rm <path>` | `git rm <path>` | `jj file untrack <path>` |
| 仅缓存 | `--cached` | `--cached` | 默认（jj untrack 只影响跟踪） |
| 递归 | `-r` / `--recursive` | `-r` / `--recursive` | 隐式（jj untrack 处理目录） |
| 强制 | `-f` / `--force` | `-f` / `--force` | 不需要（无安全检查） |
| Dry run | `--dry-run` | `--dry-run` / `-n` | 不可用 |
| Ignore unmatch | `--ignore-unmatch` | `--ignore-unmatch` | 不可用 |
| 从文件读取 pathspec | `--pathspec-from-file` | `--pathspec-from-file` | 不可用 |
| NUL 分隔符 | `--pathspec-file-nul` | `--pathspec-file-nul` | 不可用 |
| Sparse | `--sparse`（按 no-op 接受） | `--sparse` | 不可用 |
| Quiet | 全局 `--quiet` | `-q` / `--quiet` | 不可用 |
| 别名 | `rm`, `remove`, `delete` | 仅 `rm` | `file untrack` |

注意：jj 的 `file untrack` 在概念上类似于 `libra rm --cached`，它停止跟踪文件但不删除文件。jj 没有一个命令能同时停止跟踪并删除文件。

## 错误处理

| 场景 | 行为 | 退出码 |
|----------|----------|------|
| 未提供 pathspec | 错误：没有指定要移除的内容 | 非零 |
| 路径不在索引中 | 错误（使用 `--ignore-unmatch` 时为零） | 非零 / 0 |
| 目录未使用 `-r` | 错误：不使用 `-r` 时不递归移除目录 | 非零 |
| 未提交的本地修改 | 错误：文件有本地修改，使用 `--cached` 或 `-f` | 非零 |
| 暂存更改与 HEAD 不同 | 错误：文件有已暂存更改，使用 `--cached` 或 `-f` | 非零 |
| 同时有已暂存和本地更改 | 错误：文件有与文件和 HEAD 都不同的已暂存内容，使用 `-f` | 非零 |
| 不在仓库内 | 错误：找不到仓库 | 非零 |
