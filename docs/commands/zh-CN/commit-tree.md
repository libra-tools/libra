# `libra commit-tree`

Plumbing：从已有 tree 创建 commit 对象 — 不影响索引、工作树、HEAD 或 ref（lore.md §1.15）。

## 概要

```
libra commit-tree <tree> [-p <parent>]... [-m <paragraph>]... [-F <file>]...
```

## 说明

把一个 tree、parents 和 message 封装成 commit 对象，写入对象库，并打印 OID。除此之外不做任何更改：需要用 `libra update-ref` 显式发布结果（其 protect/archive 策略会保护 `refs/heads/*`）。结合 `update-index` / `write-tree` / `read-tree` 的 `--index-file` scratch 标志，可以闭合 Git 惯用的离工作树 revision composition 流程：

```bash
BLOB=$(libra hash-object -w --stdin < content)
libra update-index --index-file scratch.idx --add --cacheinfo "100644,$BLOB,path/file"
TREE=$(libra write-tree --index-file scratch.idx)
COMMIT=$(libra commit-tree "$TREE" -p HEAD -m "composed")
libra update-ref refs/heads/topic "$COMMIT"
```

`<tree>` 接受 tree OID；commit-ish/refs/tags 会 peel 到其 tree（这是已记录的 Libra 对 Git 的超集）。`-p` 可重复（保持顺序；重复 parent 会警告并忽略，类似 Git）；parent 必须能作为 commit 加载。message 来自可重复的 `-m` 段落、`-F` 文件（`-` = stdin）或裸管道 stdin；`-m` 与 `-F` 可组合（所有 `-m` 段落在前，然后是 `-F` — argv 交错顺序不保留，已记录）。message 按字节精确保留（1.9/1.10 trailer block 会逐字节参与哈希）。`--json` 输出 `{"commit": "<oid>"}`。

## 与 Git 的有意差异

- 拒绝空 message（仓库级规则；git plumbing 接受空 message）— 目前还不能重放带空 message 的外部历史。
- v1 中 commit 永远不签名（git 在这里会遵守 `commit.gpgsign`）；vault 签名是已记录的后续项。
- 还不支持 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE`，所以 OID 不能跨运行复现（已记录后续项）。
- TTY 中没有 `-m`/`-F` 会成为用法错误，而不是交互式等待（agent-safe）。

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | Commit 对象已写入；OID 已打印。 |
| `128` | 致命错误（tree/parent 无法解析、不在仓库中、写入失败）。 |
| `129` | 用法错误（TTY 中没有/空 message，无效参数）。 |

## 示例

```bash
libra commit-tree $TREE -m 'root commit'
libra commit-tree $TREE -p HEAD -m subject -m 'Reviewed-by: Alice <a@e>'
echo msg | libra commit-tree $TREE -p A -p B
libra commit-tree HEAD -m 'same tree, new message'
```
