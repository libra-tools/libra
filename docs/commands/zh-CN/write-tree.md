# `libra write-tree`

把当前 index 写成一个 tree 对象并打印其对象 id —— [`read-tree`](read-tree.md) 的底层配套命令，等价于 `git write-tree`。

## 用法

```
libra write-tree [--index-file <path>]
```

## 说明

`write-tree` 读取 `.libra/index`，构造一个**嵌套**的 Git tree 对象（每个目录一个 tree），把所有 tree 对象写入对象库，并打印根 tree 的对象 id。文件 mode（普通/可执行/符号链接/gitlink）会被保留，对象格式（SHA-1 / SHA-256）跟随仓库的 hash kind。

空 index 产生规范空 tree（SHA-1 下为 `4b825dc642cb6eb9a060e54bf8d69288fbee4904`）。

这是只读底层命令：它写入 tree 对象，但不移动任何 ref，也不修改 index 或工作树。

写入任何 tree 对象前，`write-tree` 会校验每个 stage-0 index 条目中指向本仓库对象的
mode。普通文件、可执行文件和符号链接必须指向可加载的 blob 对象；异常 tree-mode
条目必须指向可加载的 tree 对象。对象缺失或类型不匹配会 fail-closed，并返回
`LBR-REPO-002`。Gitlink（`160000`）不会被校验，因为它指向的子模块 commit 可能位于
另一个对象库。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `--index-file <path>` | 读取 scratch index 而不是 `.libra/index`；缺失的 scratch index 视为空。 | `libra write-tree --index-file scratch.idx` |
| `--json` / `--machine` | 结构化输出：`{ tree: "<id>" }`。 | `libra --json write-tree` |

Git 的 `--prefix=<prefix>` 与 `--missing-ok` 未公开（延后）。

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | tree 已写入，打印其 id。 |
| `128` | 不在仓库内、无法处理 index/tree，或 index 对象缺失/类型不匹配（`LBR-REPO-002`）。 |

## 示例

```bash
# 写出 index 并捕获 tree id
TREE=$(libra write-tree)

# 面向 agent 的结构化输出
libra --json write-tree

# 从 scratch index 构造 tree
libra update-index --index-file scratch.idx --cacheinfo 100644,$OID,path/file
libra write-tree --index-file scratch.idx
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 把 index 写成 tree | `libra write-tree` | `git write-tree` |
| 把 tree 读入 index | `libra read-tree <tree>` | `git read-tree <tree>` |
