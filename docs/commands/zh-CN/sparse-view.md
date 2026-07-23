# libra sparse-view

`libra sparse-view` 管理 **只读 sparse VIEW filter**（lore.md 2.2）— git sparse-checkout 未被拒绝的补充。它是 Libra 扩展，刻意 **不** 命名为 `sparse-checkout`：它 **永远不会** 触碰工作树。

## 兼容性

- 级别：`intentionally-different`。
- MATERIALIZING 形式 — 顶层 `sparse-checkout` 命令和 `clone --sparse` — 仍然拒绝（D10）。`mv --sparse` / `rm --sparse` 保持已接受 no-op（skip-worktree cone membership 仍未实现）。

## 设计

一个 gitignore-syntax include patterns allowlist 会限定读取/查询命令 **显示** 的内容：

- `ls-files` — 只列出 in-view 的 tracked/other entries（unmerged entries 始终显示）。
- `diff` — WORKING-TREE diff（unstaged）被限定到 view。

它严格只读且 commit-safe：

- 工作树永不修改；不写 skip-worktree bits。
- `status` 内容 **永不** 过滤 — 它保持诚实，显示 `commit` 会记录什么（只用一行 advisory 提醒 view 已启用）。
- `diff --staged`（commit-authoritative）和 `diff A..B`（rev-vs-rev）**永不** 过滤。

Pattern 语义是 ALLOWLIST：最后匹配的 pattern 获胜，`!pat` 会在更宽泛 include 下重新 carve a hole，未匹配任何 pattern 的路径为 out-of-view（default-exclude）。没有 ancestor-dominance shortcut（它会破坏 `!child` negations）。禁用或空 view 是 no-op（输出与未配置 view 字节一致）。

状态：patterns 存在 `sparse_view` SQLite 表（owner `internal::sparse`）；toggle 存在按 worktree 投影的 `sparse_view_meta` 表（W1 §C.4.1.1，迁移 `2026072304`——已废弃无 scope 的 config_kv `sparse.enabled` 键）。

**自 W1 起按 worktree 隔离**：patterns 与 enabled toggle 是 per-worktree 事实——所有子命令与 `ls-files`/`diff`/`hydrate` 门都作用于当前 worktree 自己的视图；worktree 目录消失后 remove/prune 会回收其行。旧的仓库全局状态仅在不存在 linked worktree 时归属 main worktree；否则 schema 迁移 fail-closed，要求先显式 `sparse-view clear`（或在目标 worktree 重建视图）——patterns 绝不复制到所有 worktree。

## 示例

```bash
libra sparse-view set 'src/**' 'docs/**'   # 将 ls-files/diff 限定到这些路径
libra sparse-view add '!src/gen/**'        # 从 view 中 carve a hole
libra sparse-view list
libra sparse-view status                   # enabled 状态 + pattern 数量
libra sparse-view disable                  # 关闭（保留 patterns）
libra sparse-view clear                    # 删除所有 patterns 并禁用
```

## 延后项（非 v1）

Cone mode（自动包含父目录 + 完整子树）；任何 materialization（这是被拒绝的 D10 sparse-checkout）。
