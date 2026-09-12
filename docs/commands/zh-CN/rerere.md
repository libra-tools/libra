# `libra rerere`

**RE**use **RE**corded **RE**solution（复用已记录的解决）。记录你如何解决一次合并冲突，并在相同冲突再次出现时自动复用该解决方案。

## 用法

```
libra rerere [status | diff | forget <path>... | clear | gc]
```

## 说明

不带子命令时，`rerere` 扫描已跟踪文件中的冲突标记并：

- 为每个新冲突记录 **preimage**（带标记的冲突文件），并在本 worktree 的 `MERGE_RR`（位于该 worktree 的 local gitdir——自 W2 起每个 worktree 各自跟踪当前冲突，而下述已记录的 resolution 仍跨 worktree 共享）中跟踪；
- 若已记录的 **postimage**（解决方案）匹配某冲突，则通过三方合并**复用**；只有合并干净时才把结果写回文件；
- 一旦被跟踪的冲突被手工解决，记录其 postimage，使下一次相同冲突自动解决。

完整冲突以其规范化 hunk 两侧的 SHA-256 匹配：rerere 丢弃 diff3 的 base 段、按字典序排列两侧并哈希所得 side pair。因此标签、两侧顺序和无关的文件上下文不会阻止命中。没有完整冲突标记的文件回退到历史的整文件哈希。既有的整文件键缓存保留；未命中时会按规范化键重新记录。

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

Rerere 把完整规范化 preimage 作为三方合并 base、把已记录解决作为另一侧，并与当前规范化冲突合并后才替换文件；若回放本身冲突，工作树文件保持不变。`rerere.enabled=true` 时，`merge` / `rebase` / `cherry-pick` 会自动记录 preimage、回放匹配解法并在完成时记录 postimage；默认关闭时行为不变。三个命令都支持 `--rerere-autoupdate`（暂存回放解法）与 `--no-rerere-autoupdate`（保持未暂存），last-wins；省略时继承 `rerere.autoUpdate`，显式选择跨 `--continue` 保持。
