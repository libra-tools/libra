# `libra diff-files`

显示 index 与工作树之间的差异 —— 复用唯一 `diff` 引擎的底层入口（见 [`diff`](diff.md)）。

## 用法

```
libra diff-files [-p] [-s] [--raw] [--name-only] [--name-status] [--exit-code] [--quiet] [-- <path>...]
```

## 说明

`diff-files` 等价于 `libra diff --no-renames`：显示未暂存改动（index 对工作树）。路径限定写在 `--` 之后。作为 Git plumbing，它忽略 porcelain `diff.renames`；`libra --json diff-files` 等全局输出标志仍可用。

`--name-status` 打印 Git 的 `A`/`M`/`D`/`R` 状态字母（例如 intent-to-add 条目为
`A\tnew.txt`）。工作树一侧是**基于 stat 的**：仅 stat 漂移而内容未变的条目按 `M` 报告、工作树 sha 全零且不哈希——与 `git diff-files` 及 `t3700-add.sh#24` 背后的契约一致。状态字母按**条目是否存在**判定，不按内容是否为空：`read-tree` 使 stat 失效的空 blob 是 `M` 而不是 `A`，工作树被截断为 0 字节是 `M` 而不是 `D`。其余 porcelain 标志面仍有意保持精简；完整补丁与状态选项请使用共享的 `libra diff`。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `-p`, `--patch` | 输出补丁而非 raw 记录。 | `libra diff-files -p` |
| `--raw` | 强制 raw 记录（默认）。 | `libra diff-files --raw` |
| `--name-only` | 只打印变更路径名。 | `libra diff-files --name-only` |
| `--name-status` | 为每个变更路径打印状态字母。 | `libra diff-files --name-status` |
| `-s`, `--no-patch` | 完全抑制 diff 输出体。 | `libra diff-files -s` |
| `--exit-code` | 有差异时退出 1（默认 0）。 | `libra diff-files --exit-code` |
| `--quiet` | 不打印任何输出，有差异时退出 1。 | `libra diff-files --quiet` |
| `-- <path>...` | 将 diff 限定到路径。 | `libra diff-files -- src/` |
| `--json` / `--machine` | 结构化 diff 输出。 | `libra --json diff-files` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 无差异；或有差异但未给 `--exit-code`/`--quiet`（Git 底层约定）。 |
| `1` | 有差异且给了 `--exit-code`/`--quiet`。 |
| `128` | 不在仓库内。 |
| `129` | 参数无效（`LBR-CLI-002`）。 |

## 示例

```bash
# 显示未暂存改动
libra diff-files

# 限定到目录
libra diff-files -- src/

# 机器可读的状态字母
libra diff-files --name-status
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 未暂存改动 | `libra diff-files` | `git diff-files` |

延后：`-1`/`-2`/`-3` 阶段选择。更多选项用 `libra diff`。
