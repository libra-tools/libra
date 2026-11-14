# `libra diff-index`

显示一个 tree 与工作树之间的差异 —— 复用唯一 `diff` 引擎的底层入口（见 [`diff`](diff.md)）。

## 用法

```
libra diff-index [--cached] [-p] [-s] [--raw] [--name-only] [--name-status] [--exit-code] [--quiet]
                 <tree> [-- <path>...]
```

## 说明

`diff-index <tree>` 通过共享 diff 引擎把给定 tree-ish 与当前工作树比较，工作树候选条目按 stat 判定。加上 `--cached` 则与 index 比较。路径限定写在 `--` 之后。作为 Git plumbing，它忽略 porcelain `diff.renames`；`libra --json diff-index ...` 等全局输出标志仍可用。

默认输出是 raw。`--name-status` 与 `diff-files` 一样按**条目是否存在**给字母：stat 失效的空 blob 是 `M` 而不是 `A`，工作树被截断为 0 字节是 `M` 而不是 `D`。

显式 `text`、`eol=lf`、`eol=crlf` 属性使 `-p` 按 clean 工作区内容比较（CRLF 对转换为 LF）。因此补丁可以为空，而基于 stat 的 raw/名称输出仍报告 `M` 和全零工作区 OID；`--quiet`、`--exit-code` 仍按该 stat 差异返回 1，包括补丁为空的情况。按内容判断干净状态请用 [`diff`](diff.md)。`-text` 保留字节，`text=auto` 同时支持文本转换、二进制探测和历史 CRLF 索引保护，限制见 [add](add.md)。存储对象不转换，命令不刷新索引或改写工作区文件。生成补丁时读取内容或属性失败会返回错误，不得以空补丁成功退出。

`--cached` 按已存储的 tree 和索引字节比较，不受当前工作区属性影响。

旧版符号链接工作树的内容比较从已解析的元数据目录读取 `info/attributes`。属性文件必须是可读的普通文件；属性文件自身的符号链接会被拒绝。 raw 输出和差异退出码仍保持上述基于 stat 的规则。


plumbing raw 记录使用完整对象 id（SHA-1 40 位 / SHA-256 64 位），缺失侧（如 `diff-index`/`diff-files` 的工作树侧）为完整全零 id。`-p --raw` 先输出 raw 记录、空行，再输出补丁，与 Git 一致。

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
| `128` | 不在仓库内，或内容/属性读取失败。 |
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
