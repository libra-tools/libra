# `libra media`

FastCDC LFS media chunking client（lore.md §6）— 一个 **feature-gated** Libra 扩展（`fastcdc`，只编译进带 `--features fastcdc` 的构建；**默认二进制中不存在**）。它对媒体文件做内容定义分块，构建 versioned manifest，把 chunks 存入私有本地 store，重组并验证它们，并与远端协商 chunked-LFS 能力，在不支持时安全回退到标准 Git LFS。

`media` 是 Libra-only 扩展（`intentionally-different`）：Git 没有 media chunking 概念。Git 对象图永不被触碰 — chunk 永远不是 Git object ID，chunks/manifests 存放在私有 `.libra/media/` store 中，它是 `objects/` 的 sibling。`media_oid` 始终是完整文件的 SHA-256（独立于 `core.objectformat`），与标准 LFS pointer OID 字节一致。

## 子命令

| 子命令 | 说明 | 示例 |
|---|---|---|
| `chunk <path> [--store]` | 对文件做 FastCDC 分块并输出 manifest；`--store` 会把 chunks + manifest 持久化到 `.libra/media`。 | `libra media chunk big.psd --store` |
| `inspect <manifest>` | 解析并验证一个 manifest JSON 文件。 | `libra media inspect .libra/media/manifests/<oid>.json` |
| `verify <path> \| --media-oid <oid>` | 从本地 chunk store 重组并验证完整 `media_oid`（永不发布损坏文件）。 | `libra media verify big.psd` |
| `probe [--remote <name>]` | 探测远端 media capability endpoint 并报告传输决策（chunked vs standard-LFS fallback）。 | `libra media probe --remote origin` |
| `--json` | stdout 上的结构化 JSON 信封（全局标志）。 | `libra --json media chunk big.psd` |

## 安全回退

`media probe` 报告以下之一：`chunked (fastcdc-v1)`（完全兼容的 Libra-aware media server）、`standard-lfs (fallback)` 并附带原因（无 capability endpoint、服务端禁用、算法不兼容、仓库策略禁用、未知更高版本，或 backoff 后的服务端错误），或 `blocked`（服务端没有保留标准 fallback 对象且本地也没有完整对象 — 拒绝生成 chunk-only upload，而不是静默生成）。对当前所有可达远端 — 都没有运行（冻结的）Libra media server — 决策都是标准 Git LFS fallback。

## 延后项

Libra-aware media **server**（真实跨机器 chunked upload/download、capability + chunk + manifest-finalize endpoints、manifest lifecycle、GC/fsck/heal，以及所有 anti-side-channel 保证）已冻结在 lore.md §6.5–6.8，不属于此 client v1。Chunk-only repo policy（丢弃标准 LFS fallback 对象）和 range-based hydration 也已延后。

## 示例

```bash
libra media chunk big.psd                 # 对文件分块；打印 manifest 摘要
libra media chunk big.psd --store         # 同时本地持久化 chunks + manifest
libra media inspect .libra/media/manifests/<oid>.json
libra media verify big.psd                # 从 store 重组并验证 media_oid
libra media probe --remote origin         # capability probe；回退到标准 LFS
libra --json media chunk big.psd          # 给 agents 使用的结构化 JSON 输出
```
