# `libra pack-objects`

从 stdin 读取 object ids 并创建 pack。

> **隐藏 / 内部 plumbing。** `pack-objects` 不是 Libra 公开 Git 兼容表面的一部分（它从 `libra --help` 中隐藏）。它存在于内部和集成用途 — 大多数用户需要的是 [`libra repack`](repack.md)。

## 概要

```
libra pack-objects [--stdout] < object-ids
```

## 说明

`pack-objects` 从标准输入读取 object ids — 每行一个，并容忍 `libra rev-list --objects` 打印的 `<id> <path>` 形式（只使用开头 id）— 然后通过共享 pack writer 把这些对象编码成单个 pack。

默认情况下，pack 写入 `.libra/objects/pack/`，并把新的 `pack-<checksum>` stem 打印到 stdout。带 `--stdout` 时，原始 pack 字节会流式写到标准输出，方便管道传给 `libra index-pack`。

## 选项

| 选项 | 说明 |
|------|------|
| `--stdout` | 将原始 pack 字节写到 stdout，而不是写入 `objects/pack`。 |

## 退出状态

- `0` — 已生成 pack。
- `128` — stdin 未提供 object ids。
- 其他非零 — 在仓库外运行，或 pack 无法编码。
