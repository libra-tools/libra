# `libra ls-tree`

列出 tree 对象的内容。

## 概要

```bash
libra ls-tree [OPTIONS] <TREE-ISH> [PATH...]
```

Without -r or -d, a single directory path without a trailing slash lists the directory entry itself; with a trailing slash it lists the directory contents.

末尾 `/.` 同样选择目录内容；`-t` 输出到达路径时经过的树，`-r` 递归进入选中的目录。多个参数重叠时，被其它参数穿过的目录会展开，重复条目只输出一次。

兼容性限制：多路径仍按参数次序输出，不保证 Git 树序；完整 pathspec magic、子目录中的 `-t` 父树显示、缺失路径和 blob 尾斜杠行为、`-d` 加目录尾斜杠的选择尚未完全对齐 Git。

## 说明

`libra ls-tree` 将 `<TREE-ISH>` 解析为 commit root tree 或 tree object hash，然后打印该 tree 中的条目。从子目录调用时，路径和输出默认相对于该子目录。它是只读命令：不更新 refs、索引、工作树或对象存储。

当前兼容切片支持普通路径前缀过滤、`--full-name`、`--full-tree`、`REV:path` tree-ish 语法（解析一个 revision 并进入子树，例如 `HEAD:src`），以及 `--format`。完整 Git pathspec magic 延后。

## 选项

| 标志 | 说明 |
|------|------|
| `-r`, `--recursive` | 递归进入子树 |
| `-t` | 递归时显示 tree 条目，并显示到达各路径参数所经过的 tree |
| `-d` | 显示匹配 tree 条目自身，而不是其子项 |
| `-l`, `--long` | 显示 blob 大小；tree 和 commit 条目使用 `-` |
| `-z` | 用 NUL 而不是换行终止记录 |
| `--name-only` | 只打印条目路径 |
| `--name-status` | Git-compatible alias，只打印条目路径 |
| `--object-only` | 只打印 object IDs |
| `--full-name` | 从子目录调用时，打印相对于仓库根的路径 |
| `--full-tree` | 从仓库根列出，并将路径过滤解释为相对于仓库根 |
| `--format=<FORMAT>` | 替换对象元数据与路径原子；与 `-l` 及仅名称/对象 ID 模式互斥 |
| `--abbrev[=<N>]` | 将 object IDs 缩写为 `N` 字符，省略 `N` 时为 7 |
| `<TREE-ISH>` | 提交、分支、标签、`HEAD` 或 tree object hash |
| `[PATH...]` | 可选路径过滤；不带尾斜杠的目录路径选中目录条目自身（另一个路径参数指向其内部时选中其内容），带尾斜杠时选中其内容；除非设置 `--full-tree`，否则相对于当前目录 |

## 示例

```bash
libra ls-tree HEAD
libra ls-tree HEAD:src
libra ls-tree HEAD:src/nested
libra ls-tree -r HEAD src
libra ls-tree -l HEAD README.md
libra ls-tree --name-only HEAD src/
libra ls-tree --full-name HEAD
libra ls-tree --full-tree HEAD
libra ls-tree --object-only --abbrev HEAD
libra ls-tree -z HEAD
libra --json ls-tree HEAD
```

## 人类可读输出

默认输出匹配 Git 常见形状：

```text
100644 blob 4f3c2d1a7b8c9d0e1234567890abcdef12345678	README.md
040000 tree 5a6b7c8d9e0f1234567890abcdef1234567890	src
```

带 `-l` 时，blob 条目包含其解码对象大小：

```text
100644 blob 4f3c2d1a7b8c9d0e1234567890abcdef12345678      128	README.md
040000 tree 5a6b7c8d9e0f1234567890abcdef1234567890        -	src
```

%(objectsize:padded) pads blob sizes to at least seven columns.

In JSON output, a format string does not add a size field.

普通 `%(objectsize)` 输出十进制字节数；padded 原子右对齐到至少七列，大数不截断。tree/gitlink 使用 `-`（padded 时同样补空格）。未知原子按字面保留。人类格式仅在包含这两个完整大小原子时读取 blob；缺失对象报 `LBR-IO-001`。JSON 仅在 `-l` 时读取并输出大小，格式字符串不触发读取。

## 结构化输出

带 `--json` 时，输出使用标准命令信封。带 `-l` 时，blob 条目包含 `size` 字段：

```json
{
  "ok": true,
  "command": "ls-tree",
  "data": {
    "treeish": "HEAD",
    "root_tree": "5a6b7c8d9e0f1234567890abcdef1234567890",
    "recursive": false,
    "entries": [
      {
        "mode": "100644",
        "object_type": "blob",
        "object": "4f3c2d1a7b8c9d0e1234567890abcdef12345678",
        "path": "README.md",
        "size": 128
      }
    ]
  }
}
```

## 参数对比：Libra vs Git vs jj

| 功能 | Libra | Git | jj |
|------|-------|-----|----|
| Commit/tree listing | 支持 | 支持 | 使用 file/revset commands |
| Recursive listing | `-r` / `--recursive` | `-r` | 不同模型 |
| 递归时及到达路径时经过的 tree entries | `-t` | `-t` | 不同模型 |
| 子目录输出 | 相对于当前目录；`--full-name` 保留仓库路径 | 支持 | 不同模型 |
| 根作用域列出 | `--full-tree` | `--full-tree` | 不同模型 |
| 路径过滤 | 仅前缀过滤；除非设置 `--full-tree`，否则相对于当前目录 | 完整 pathspec | Revset/file patterns |
| 自定义格式 | `--format` | `--format` | 不同模型 |
| JSON 输出 | `--json` | 无 | 无 |

## 错误处理

| 场景 | StableErrorCode | 退出 |
|------|-----------------|------|
| 无效或缺失 tree-ish | `LBR-CLI-003` | 129 |
| `REV:path` 指向 blob（不是 tree） | `LBR-CLI-003` | 128 |
| 读取对象失败 | `LBR-IO-001` | 128 |
| 存储的 refs/objects 损坏 | `LBR-REPO-002` | 128 |
