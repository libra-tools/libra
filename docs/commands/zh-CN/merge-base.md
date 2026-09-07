# `libra merge-base`

查找两个提交的最佳共同祖先 —— `git merge-base` 的一个聚焦子集。底层为 `internal/merge_base.rs` 的唯一最近公共祖先（LCA）实现，`diff A...B` 也复用它。

## 用法

```
libra merge-base <commit> <commit>
libra merge-base --all <commit> <commit>
libra merge-base --is-ancestor <commit> <commit>
```

## 说明

给定两个提交，`merge-base` 打印它们的最佳共同祖先 —— 真正的 LCA：一个不是另一个共同祖先的**严格**祖先的共同祖先。对常见的「Y」形历史，即两分支分叉处。交叉合并（criss-cross）历史可能有多个 LCA；`--all` 全部打印，默认打印其一（确定性选择）。

带 `--is-ancestor` 时不打印任何内容；退出码回答「第一个提交是否为第二个的祖先」。

每个 `<commit>` 可为分支、tag、`HEAD` 或对象 id。

查找过程从两个提交同时向下染色，走一个按 committer date 排序的优先队列（与 Git 的 `paint_down_to_common` 同构）：一次带着两侧标记的合并遍历，取代「每侧一次全量遍历 + 每个共同祖先各一次」。commit 在拿到另一侧标记时会被再次访问，因此「一次遍历」指的是对图的一趟，而不是每个 commit 只访问一次。结果不变——committer date 只决定访问顺序，日期乱序或全部相同的历史给出的 base 完全一致。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `--all` | 打印所有最近公共祖先，而非一个。 | `libra merge-base --all main feature` |
| `--is-ancestor` | 测试祖先关系（退出 0/1），不打印 base。 | `libra merge-base --is-ancestor v1 main` |
| `--json` / `--machine` | 结构化输出：`{ bases: [...] }` 或 `{ is_ancestor }`。 | `libra --json merge-base main feature` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 打印了 merge base；或（`--is-ancestor`）第一个是第二个的祖先。 |
| `1` | 无共同祖先；或（`--is-ancestor`）第一个不是第二个的祖先。无输出。 |
| `128` | 提交无法解析，或参数个数不对。 |

## 示例

```bash
# main 与 feature 在哪里分叉？
libra merge-base main feature

# release tag 还在 main 主线上吗？
libra merge-base --is-ancestor v1.0 main && echo "可快进"

# 把 feature 与它从 main 分叉处对比
libra diff main...feature
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 最佳共同祖先 | `libra merge-base a b` | `git merge-base a b` |
| 所有 merge base | `libra merge-base --all a b` | `git merge-base --all a b` |
| 祖先测试 | `libra merge-base --is-ancestor a b` | `git merge-base --is-ancestor a b` |

延后（暂未公开）：多于两个提交，以及 `--octopus` / `--independent` / `--fork-point`。`merge`、`rebase`、`am` 与 `diff A...B` 均经该共享 LCA 计算；只剩 `log A...B` 仍有自己的可达集实现，其迁移是已记录的后续项。
