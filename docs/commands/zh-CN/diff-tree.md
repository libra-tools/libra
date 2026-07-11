# `libra diff-tree`

显示两个 tree 之间的差异 —— 复用 [`diff`](diff.md) 引擎、采用 plumbing 退出码与 rename 默认值的底层入口。

## 用法

```
libra diff-tree <tree-a> <tree-b> [-- <path>...]
```

## 说明

`diff-tree` 等价于 `libra diff --old <tree-a> --new <tree-b> --no-renames`：对两个 tree-ish（commit 或 tree id）做 diff。路径限定写在 `--` 之后。作为 Git plumbing，它忽略 porcelain `diff.renames`；`libra --json diff-tree ...` 等全局输出标志仍可用。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `<tree-a> <tree-b>` | 要比较的两个 tree-ish。 | `libra diff-tree HEAD~1 HEAD` |
| `-- <path>...` | 将 diff 限定到路径。 | `libra diff-tree a b -- src/` |
| `--json` / `--machine` | 结构化 diff 输出（与 `diff` 同信封）。 | `libra --json diff-tree a b` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 无差异。 |
| `1` | 有差异（打印 diff）—— Git 底层退出约定。 |
| `128` | tree-ish 无法解析，或不在仓库内。 |

## 示例

```bash
# 比较某提交与其父提交
libra diff-tree HEAD~1 HEAD

# 限定到目录
libra diff-tree main feature -- src/
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 比较两个 tree | `libra diff-tree a b` | `git diff-tree a b` |

延后：单提交 `diff-tree <commit>`（对比父）、`-r`/`-t`/`--stdin`、raw 输出。更多选项用 `libra diff`。
