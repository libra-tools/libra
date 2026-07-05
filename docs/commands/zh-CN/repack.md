# `libra repack`

把仓库对象合并到单个 pack。

## 概要

```
libra repack [-a|--all] [-d|--delete] [-q|--quiet]
```

## 说明

`repack` 将仓库对象编码进一个新的 `pack-<checksum>.pack`（以及匹配的 `pack-<checksum>.idx`），位于 `.libra/objects/pack/` 下。它使用与 `maintenance` tasks 相同的共享 pack writer，因此 Libra 在磁盘上写出的每个 pack 都经过一个格式良好的编码器 — 生成的 pack 可通过 `libra index-pack` 和 `libra verify-pack` 往返验证。

默认只打包当前为 **loose** 的可达对象；已经位于现有 pack 中的对象保持原位。`--all` 会扩大集合到所有可达对象，生成一个合并后的单一 pack。

可达性从 refs、reflogs 和 index 计算 — 与 `libra maintenance run --task gc` 完全相同 — 因此仅被 reflog 引用的对象永远不会被丢弃。

## 选项

| 选项 | 说明 |
|------|------|
| `-a`, `--all` | 把所有可达对象（包括已存储在 pack 中的对象）打包进一个新的单一 pack。 |
| `-d`, `--delete` | 打包后删除现在已位于新 pack 中的 loose objects。只删除 object id 位于新 pack 中的文件；现有 packs 永不删除，因此不会留下未被引用的对象。 |
| `-q`, `--quiet` | 抑制信息摘要。 |

带 `--json` / `--machine` 时，命令输出一个信封，其 `data` 对象包含 `pack`（新 pack 名称）、`objects_packed` 和 `loose_removed`。

## 退出状态

- `0` — repack 完成（包括无需打包的 no-op 情况）。
- 非零 — 命令在仓库外运行，或 pack 无法写入。

## 兼容性

这是 Git `git repack` 的聚焦子集。Libra 总是通过共享 writer 写出单个未 delta 压缩的 pack；delta compression、`--window`/`--depth` 调优、geometric repacking（`--geometric`）、保留/写入 bitmaps，以及删除冗余 *packs*（区别于 loose objects）均未实现。见 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md)。

## 示例

```
# 把 loose reachable objects 打包成一个 pack。
libra repack

# 把每个可达对象合并进一个 pack。
libra repack -a

# 重新打包所有内容，并删除现在已打包的 loose copies。
libra repack -a -d

# 机器可读摘要。
libra --json repack -a -d
```

## 另见

- [`libra maintenance`](maintenance.md) — 计划优化任务（其 `loose-objects` 和 `incremental-repack` tasks 共享此 writer）。
- [`libra index-pack`](index-pack.md) — 为现有 pack 构建 index。
- [`libra verify-pack`](verify-pack.md) — 验证 pack 及其 index。
