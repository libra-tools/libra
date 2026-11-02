# `libra diff-index`

显示一个 tree 与工作树之间的差异 —— 复用唯一 `diff` 引擎的底层入口（见 [`diff`](diff.md)）。

## 用法

```
libra diff-index [--cached] [-p] [-s] [--raw] [--name-only] [--name-status] [--exit-code] [--quiet]
                 <tree> [-- <path>...]
```

## 说明

`diff-index <tree>` 等价于 `libra diff --old <tree> --no-renames`：把给定 tree-ish 与当前工作树做 diff。加上 `--cached` 则与 index 比较。路径限定写在 `--` 之后。作为 Git plumbing，它忽略 porcelain `diff.renames`；`libra --json diff-index ...` 等全局输出标志仍可用。

默认输出是 raw。`--name-status` 与 `diff-files` 一样按**条目是否存在**给字母：stat 失效的空 blob 是 `M` 而不是 `A`，工作树被截断为 0 字节是 `M` 而不是 `D`。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `<tree>` | 要比较的 tree-ish。 | `libra diff-index HEAD` |
| `--cached` | 与 index 比较，而不是工作树。 | `libra diff-index --cached HEAD` |
| `-p`, `--patch` | 输出补丁而非 raw 记录。 | `libra diff-index -p HEAD` |
| `--raw` | 强制 raw 记录（默认）。 | `libra diff-index --raw HEAD` |
| `--name-only` | 只打印变更路径名。 | `libra diff-index --name-only HEAD` |
| `--name-status` | 为每个变更路径打印状态字母。 | `libra diff-index --name-status HEAD` |
| `--exit-code` | 有差异时退出 1（默认 0）。 | `libra diff-index --exit-code HEAD` |
| `--quiet` | 不打印任何输出，有差异时退出 1。 | `libra diff-index --quiet HEAD` |
| `-s`, `--no-patch` | 完全抑制 diff 输出体。 | `libra diff-index -s HEAD` |
| `-- <path>...` | 将 diff 限定到路径。 | `libra diff-index HEAD -- src/` |
| `--json` / `--machine` | 结构化 diff 输出。 | `libra --json diff-index HEAD` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 无差异；或有差异但未给 `--exit-code`/`--quiet`。 |
| `1` | 有差异且给了 `--exit-code`/`--quiet`。 |
| `128` | 不在仓库内。 |
| `129` | 参数无效（`LBR-CLI-002`），或修订无法解析（`LBR-CLI-003`）。 |

## 示例

```bash
# 工作树相对 HEAD 的 tree 改了什么？
libra diff-index HEAD

# tree 对 index
libra diff-index --cached HEAD
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| tree 对工作树 | `libra diff-index <tree>` | `git diff-index <tree>` |
| tree 对 index | `libra diff-index --cached <tree>` | `git diff-index --cached <tree>` |

延后：`-m`。更多选项用 `libra diff`。
