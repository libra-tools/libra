# libra layer

`libra layer` 实现 Lore 的 **本地 overlay primitive**（lore.md 2.4）：命名的、纯本地的 overlay，按显式命令物化到工作树，且 **永远不会进入提交**。这是 §3.5 composition pair 中 Phase-2 可落地的一半（其 versioned sibling `link` 延后到 §3.4 RFC）；§3.5 红线禁止的是 *默认* auto-compose 模型，不是这种 opt-in 的显式命令 overlay。

## 兼容性

- 级别：`intentionally-different` — Libra-only 扩展，没有 Git 等价物（Appendix A `无直接等价`）。

## 设计

一个 layer 是 `(name, source local dir, priority, enabled)`。状态存放在两个 SQLite side-table（`layer`、`layer_path`）中，只由 `internal::layer::LayerStore` 拥有 — 永不序列化进任何对象。两个不变量：

1. **Never-enters-commit** — 在两个关口强制：物化路径会被不可否定地排除出 ignore engine（`status`/`add .` 跳过它们），并且 `add` 暂存路径即使在 `--force`（绕过 ignore）下也会硬拒绝任何 layer-owned path。暂存此类路径是 `LBR-LAYER-001`。
2. **Never-clobbers** — 目标若与 tracked（index 或 HEAD）路径冲突，会在 `apply` 时被拒绝（`LBR-LAYER-001`，fail-closed）；`unapply`/`remove` 会跳过用户编辑过的 overlay 文件（content-hash mismatch）。

两个已启用 layer 之间发生同一目标冲突时的优先级：更高 `priority` 获胜，平局按 name 打破（stack 顺序中的 last-writer-wins）。

## 示例

```bash
libra layer add scratch --source ./overlays/scratch   # 注册本地 overlay
libra layer add ci --source ./ci --priority 10        # 更高 priority 赢得冲突
libra layer list                                      # 显示已注册 layer
libra layer apply                                     # 物化已启用 overlay
libra layer status                                    # 显示物化路径
libra layer unapply --layer scratch                   # 移除一个 layer 的文件（保留编辑）
libra layer remove scratch                            # 注销（先 unapply）
```

## 延后项（非 v1）

checkout/switch/merge/clone 时自动物化（§4.1 bypass surface — v1 仅显式命令）；versioned composition（`link`/subtree，§3.4-RFC-gated）；remote/object-DB sources；覆盖 tracked path（拒绝，永不静默 shadow）。
