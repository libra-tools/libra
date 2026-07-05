# `libra revision`

Revision ordinal index（Libra 扩展，lore.md §1.16，移植 Lore 的 `revision find number`）。Git 没有等价表面。

## 概要

```
libra revision find --number <N> [--ref <branch>]
libra revision number <commitish> [--ref <branch>]
libra revision index [--ref <branch>] [--rebuild]
```

## 说明

每个分支的 **first-parent chain** 获得单调的 1-based 编号（1 = root，N = tip），存储在可重建的 SQLite side table 中。编号是 tip 的纯函数（跨重建和机器确定性一致）。只通过合并进来的 side branch 可达的提交 **没有** ordinal — 反向查询会明确说明，而不是发明编号。

每次读取都会在与查询同一个事务中重新验证新鲜度：fast-forward 追加（已有 ordinal 永不改变）；history rewrite（rebase/amend/reset）和 `refs/replace` 变化会触发完整确定性重建。陈旧索引永不回答。长分支上的第一次查询会遍历整条链一次（O(chain) object loads，在分层存储下可能访问远端）；后续查询为索引命中。

| 子命令 | 用途 |
|---|---|
| `find --number <N>` | 打印 revision #N 的 OID（Lore 的 `revision find number`）。越界 → 退出 1 并说明链长度；`N < 1` → 129。 |
| `number <commitish>` | 反向查询：某个提交在 ref 链上的 ordinal。不在链上 → 退出 1 并给出覆盖范围说明。 |
| `index [--rebuild]` | 新鲜度报告（tip、count、built-at）；`--rebuild` 强制确定性重建并修剪已删除分支的索引行。 |

`--ref <branch>` 指向任意本地分支；默认是当前分支（detached HEAD → 报错并建议 `--ref`）。`--json` 输出结构化结果。`find --metadata`（按 1.10 revision metadata 搜索）是已记录后续项 — ordinal index 提供这种扫描会遍历的确定性迭代顺序。

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | 成功。 |
| `1` | 未命中（ordinal 越界；commit 不在链上）。 |
| `128` | 致命错误（不在仓库中；detached HEAD 且未给 `--ref`）。 |
| `129` | 用法错误（`--number` < 1）。 |

## 示例

```bash
libra revision find -n 1                 # root revision
libra revision number HEAD               # mainline 有多长？
libra revision find -n 42 --ref main     # main 的第 42 个 revision
libra revision index --rebuild           # 确定性重建 + prune
libra --json revision number HEAD        # 结构化输出
```

## 与 Git 对比

最接近的 Git 对应物：`git rev-list --first-parent --count <oid>`（反向）和 `<tip>~<k>` 后缀算术（正向）。在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中分类为 `intentionally-different`。
