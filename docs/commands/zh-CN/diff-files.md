# `libra diff-files`

显示 index 与工作树之间的差异 —— 复用唯一 `diff` 引擎的底层入口（见 [`diff`](diff.md)）。

## 用法

```
libra diff-files [-- <path>...]
```

## 说明

`diff-files` 等价于 `libra diff --no-renames`：显示未暂存改动（index 对工作树）。路径限定写在 `--` 之后。作为 Git plumbing，它忽略 porcelain `diff.renames`；`libra --json diff-files` 等全局输出标志仍可用。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `-- <path>...` | 将 diff 限定到路径。 | `libra diff-files -- src/` |
| `--json` / `--machine` | 结构化 diff 输出。 | `libra --json diff-files` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 无差异。 |
| `1` | 有差异（打印 diff）—— Git 底层退出约定。 |
| `128` | 不在仓库内。 |

## 示例

```bash
# 显示未暂存改动
libra diff-files

# 限定到目录
libra diff-files -- src/
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 未暂存改动 | `libra diff-files` | `git diff-files` |

延后：`-1`/`-2`/`-3` 阶段选择、raw 输出。更多选项用 `libra diff`。
