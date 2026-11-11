# `libra diff-files`

显示 index 与工作树之间的差异 —— 复用唯一 `diff` 引擎的底层入口（见 [`diff`](diff.md)）。

## 用法

```
libra diff-files [-p] [-s] [--raw] [--name-only] [--name-status] [--exit-code] [--quiet] [-- <path>...]
```

## 说明

`diff-files` 通过共享 diff 引擎比较 index 与工作树，保留 Git plumbing 基于 stat 选择候选条目的语义。路径限定写在 `--` 之后。作为 Git plumbing，它忽略 porcelain `diff.renames`；`libra --json diff-files` 等全局输出标志仍可用。

`--name-status` 打印 Git 的 `A`/`M`/`D`/`R` 状态字母（例如 intent-to-add 条目为
`A\tnew.txt`）。工作树一侧是**基于 stat 的**：仅 stat 漂移而内容未变的条目按 `M` 报告、工作树 sha 全零且不哈希——与 `git diff-files` 及 `t3700-add.sh#24` 背后的契约一致。状态字母按**条目是否存在**判定，不按内容是否为空：`read-tree` 使 stat 失效的空 blob 是 `M` 而不是 `A`，工作树被截断为 0 字节是 `M` 而不是 `D`。其余 porcelain 标志面仍有意保持精简；完整补丁与状态选项请使用共享的 `libra diff`。

显式 `text`、`eol=lf`、`eol=crlf` 属性使 `-p` 按 clean 工作区内容比较（CRLF 对转换为 LF）。因此补丁可以为空，而基于 stat 的 raw/名称输出仍报告 `M` 和全零工作区 OID；`--quiet`、`--exit-code` 仍按该 stat 差异返回 1，包括补丁为空的情况。按内容判断干净状态请用 [`diff`](diff.md)。`-text` 保留字节，`text=auto` 同时支持文本转换、二进制探测和历史 CRLF 索引保护，限制见 [add](add.md)。存储对象不转换，命令不刷新索引或改写工作区文件。生成补丁时读取内容或属性失败会返回错误，不得以空补丁成功退出。

旧版符号链接工作树的内容比较从已解析的元数据目录读取 `info/attributes`。属性文件必须是可读的普通文件；属性文件自身的符号链接会被拒绝。 raw 输出和差异退出码仍保持上述基于 stat 的规则。

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
| `128` | 不在仓库内，或内容/属性读取失败。 |
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
