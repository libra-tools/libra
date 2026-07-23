# `libra rerere`

**RE**use **RE**corded **RE**solution（复用已记录的解决）。记录你如何解决一次合并冲突，并在相同冲突再次出现时自动复用该解决方案。

## 用法

```
libra rerere [status | diff | forget <path>... | clear | gc]
```

## 说明

不带子命令时，`rerere` 扫描已跟踪文件中的冲突标记并：

- 为每个新冲突记录 **preimage**（带标记的冲突文件），并在本 worktree 的 `MERGE_RR`（位于该 worktree 的 local gitdir——自 W2 起每个 worktree 各自跟踪当前冲突，而下述已记录的 resolution 仍跨 worktree 共享）中跟踪；
- 若已记录的 **postimage**（解决方案）匹配某冲突，则**复用**——把解决后的内容写回文件；
- 一旦被跟踪的冲突被手工解决，记录其 postimage，使下一次相同冲突自动解决。

冲突以冲突文件字节的 SHA-256 匹配，因此整个冲突文件与之前所见逐字节相同时复用解决方案。

| 子命令 | 说明 |
|--------|------|
| （无） | 记录 preimage / 复用解决 / 记录 postimage。 |
| `status` | 列出当前被跟踪冲突的路径。 |
| `diff` | 显示每个被跟踪文件自记录 preimage 以来的改动。 |
| `forget <path>...` | 删除指定路径的已记录解决。 |
| `clear` | 停止跟踪当前冲突（保留已记录解决）。 |
| `gc` | 按阈值（已解决 60 天 / 未解决 15 天）清理旧记录。 |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 成功。 |
| `128` | 不在仓库内、`forget` 一个无记录的路径，或 I/O 错误。 |

## 示例

```bash
# 合并留下冲突后，记录它们
libra rerere

# 手工解决文件后，让 rerere 学习该解决
libra rerere

# 下次相同冲突出现时，rerere 替你解决
libra rerere status
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 记录 / 复用 | `libra rerere` | `git rerere` |
| 检查 | `libra rerere status` / `diff` | `git rerere status` / `diff` |
| 删除 / 重置 | `libra rerere forget <p>` / `clear` / `gc` | `git rerere forget <p>` / `clear` / `gc` |

差异与延后项：匹配为整文件逐字节相同（Git 对每个冲突 hunk 归一化、与 ours/theirs 顺序无关）；与 `merge` / `rebase` / `cherry-pick` 的**自动**集成（`rerere.enabled` 与 `--rerere-autoupdate`）为已记录的后续项 —— 目前请显式运行 `libra rerere`。那些命令上的 `--rerere-autoupdate` 仍按 no-op 接受。
