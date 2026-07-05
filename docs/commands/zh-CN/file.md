# libra file

`libra file` 组合对象级操作（Libra 扩展，没有 Git 等价物）。它的 v1 子命令是 `obliterate`（lore.md 2.5）。

## `file obliterate` — 带索引标记的 payload obliteration

实现“保留 ADDRESS 删 PAYLOAD”的合规删除模型（§19.6）：物理移除对象的 PAYLOAD 字节，同时保留其地址，使引用历史仍然可遍历。这是破坏性且不可逆的操作。

- 兼容性：`intentionally-different`。
- 概要：`libra file obliterate <oid> [--reason <text>] [--dry-run] [--yes]` 或 `libra file obliterate --recover`。

### 安全模型

- `--dry-run` 打印影响范围预览且不删除任何内容；真实运行 **必须** 带 `--yes`（否则 `LBR-OBLITERATE-003`）。
- v1 拒绝 packed-only 对象（`LBR-OBLITERATE-002`）— 不做 pack surgery（这是被拒绝的历史重写领域）；先 loosen/repack。
- 每次运行都会向 `.libra/obliteration-audit.jsonl` 追加持久、append-only、0600 的审计记录（§7.8）— OID（地址）、actor、approval source、reason、outcome；**绝不记录** 被擦除内容。

### 状态机（crash-safe）

tombstone row 缺失表示 Live：`(no row)` → INSERT `obliterating`（在触碰任何 payload 前 fsync）→ 物理删除 payload → UPDATE `obliterated`。崩溃最多留下 `obliterating` 且 payload 可能仍在 — 永远不会出现“已删除但仍是 Live”。`file obliterate --recover`（以及每次 obliterate 开始时的机会性 sweep）会幂等地重跑尾部。

### fsck / heal / restore 集成

fsck 会把 obliterated object 报告为 **intentionally absent** — 这是不同于 `missing` 的诊断，且永不翻转退出码 — 覆盖 object、tree、commit、parent、tag 和 index seam。`fsck --heal` 永不复活它，cloud restore 会拒绝重新下载它（拒绝重建）。

## 示例

```bash
libra file obliterate <oid> --dry-run
libra file obliterate <oid> --reason "gdpr erasure" --yes
libra file obliterate --recover
```

## 延后项（非 v1）

对象内字节级擦除（§3.5，拒绝）；pack surgery / history rewrite（拒绝）；§6.8 media/LFS chunk obliterate（等待 media layer）。
