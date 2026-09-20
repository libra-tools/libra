# `libra read-tree`

把一个 tree 对象读入 index —— [`write-tree`](write-tree.md) 的底层配套命令，是 `git read-tree` 的一个聚焦子集。

## 用法

```
libra read-tree <tree-ish>
```

## 说明

`read-tree` 把 `<tree-ish>` 解析为一个 tree，展平成 stage-0 的 index 条目，并用该内容**替换** `.libra/index`。`<tree-ish>` 可以是：

- 一个 tree 对象 id，
- 一个 commit 对象 id（剥离到其 tree），
- 一个 ref、tag、分支名或 `HEAD`（剥离到其 tree）。

首版是**仅 index**：它从不触碰工作树，因此不会静默覆盖工作树文件。会修改工作树或做合并的 Git 选项（`-u`、`-m`、`--reset`、`--prefix`）未公开 —— 请用 `libra restore` / `libra checkout` 更新工作树。

`-m`/`--merge` 合并模式；其余语义见上。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `<tree-ish>` | 要读取的 tree（tree id、commit、ref、tag 或 `HEAD`）。 | `libra read-tree HEAD` |
| `--json` / `--machine` | 结构化输出：`{ tree: "<id>", entries: <n> }`。 | `libra --json read-tree HEAD` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | tree 已读入 index。 |
| `128` | 不在仓库内，或 `<tree-ish>` 不是有效的 tree-ish。 |

## 示例

```bash
# 把 index 重置为 HEAD 的 tree（工作树不变）
libra read-tree HEAD

# 读取由 write-tree 捕获的具体 tree id
TREE=$(libra write-tree)
libra read-tree "$TREE"

# 面向 agent 的结构化输出
libra --json read-tree HEAD
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 把 tree 读入 index | `libra read-tree <tree>` | `git read-tree <tree>` |
| 把 index 写成 tree | `libra write-tree` | `git write-tree` |

`-m`/`--merge` 把 tree 合并进当前索引：tree 条目新增或更新、tree 未提及的索引条目保留，被替换的条目保留索引 v3 扩展位（skip-worktree / intent-to-add）；不带 `-m` 时按 Git 的重建语义重建索引并清除扩展位。延后（未公开）：`-u`（更新工作树）、`--reset`、`--prefix`、多 tree 合并。
