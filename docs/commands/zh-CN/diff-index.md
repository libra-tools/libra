# `libra diff-index`

显示一个 tree 与工作树之间的差异 —— 复用唯一 `diff` 引擎的底层入口（见 [`diff`](diff.md)）。

## 用法

```
libra diff-index <tree> [-- <path>...]
```

## 说明

`diff-index <tree>` 等价于 `libra diff --old <tree> --no-renames`：把给定 tree-ish 与当前工作树做 diff。路径限定写在 `--` 之后。作为 Git plumbing，它忽略 porcelain `diff.renames`；`libra --json diff-index ...` 等全局输出标志仍可用。

`--cached`（把 tree 与 index 比较）**暂不支持**；HEAD 对 index 请用 `libra diff --staged`。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `<tree>` | 与工作树比较的 tree-ish。 | `libra diff-index HEAD` |
| `--cached` | 与 index 比较（暂不支持 → 退出 128）。 | |
| `-- <path>...` | 将 diff 限定到路径。 | `libra diff-index HEAD -- src/` |
| `--json` / `--machine` | 结构化 diff 输出。 | `libra --json diff-index HEAD` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 无差异。 |
| `1` | 有差异（打印 diff）—— Git 底层退出约定。 |
| `128` | tree 无法解析、给了 `--cached`（不支持），或不在仓库内。 |

## 示例

```bash
# 工作树相对 HEAD 的 tree 改了什么？
libra diff-index HEAD

# HEAD 对 index（目前用 diff --staged）
libra diff --staged
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| tree 对工作树 | `libra diff-index <tree>` | `git diff-index <tree>` |
| tree 对 index | `libra diff --staged`（仅 HEAD） | `git diff-index --cached <tree>` |

延后：对任意 tree 的 `--cached`、raw 输出、`-m`。更多选项用 `libra diff`。
