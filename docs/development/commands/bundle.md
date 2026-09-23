# bundle 命令开发设计

## 命令实现目标

`libra bundle create/verify/list-heads/unbundle` 提供常见 Git v2 离线备份与对象迁移路径，保留 annotated tag object，并支持 SHA-1/SHA-256。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- create：explicit revisions + `--all`/`--branches`/`--tags`，完整 non-thin bundle。
- verify/list-heads：v2 header、prerequisite、PACK v2、完整 trailer checksum（list-heads 只读 header）。
- unbundle：验证并安装 pack/index，打印 heads，不更新 refs，与 Git unbundle 的消费边界一致。
- 延后：prerequisite/thin/incremental range create、verify 全 entry decode。 `libra clone <bundle>` 已实现（issues/474 CL-09）。

## 设计方案

- 入口：`BundleArgs`/`BundleSubcommand` → `execute_safe`；所有子命令 require repo/hash-kind preflight。
- `collect_bundle_heads` 合并 selectors 并按 ref name 去重；tag row 保留 raw OID。`collect_reachable_object` 用共享 `seen` 单次遍历 tag target、commit 历史和 tree/blob closure，避免多 ref 重复扫描；gitlink 不跨仓库追踪。
- 对象转 `Entry` 时显式传播序列化错误，并在保留前按 raw data 累计 1 GiB cap；`encode_pack` 复用 `PackEncoder`，spawn task 显式传播 hash kind，以固定 128 容量 channel 边生产边排空。header+pack 总输出也不得超过 cap。
- create 使用同目录 UUID private temp + create_new，flush/sync_all 后 rename；失败清 temp，不追随可预测 temp symlink。
- `read_bundle_bounded` 同时做 metadata 与 streaming growth cap。`validate_bundle_pack` 校验 magic/version 和 hash-kind-aware trailer checksum。
- unbundle 写 UUID temp pack、sync、build_index_v1(SHA-1)/v2(SHA-256)，先 rename pack 后 index，index 失败删除 pack。若 deterministic final pair 已存在，则逐字节核对 pack并重建 temp index核对 installed index，避免对损坏 pair 报成功。
- unbundle 不修改 SQLite refs/HEAD/index/worktree；输出 heads 供调用者显式 `update-ref`。prerequisite 缺失/partial pair/checksum/index/rename error 均 fail-closed。

## 实现历史

- 2026-06-30（GGT-13）：create explicit rev、basic verify/list-heads。
- 2026-07-14（P1-11）：selectors、annotated tag closure、checksum、bounded IO、unbundle、private temp/installed pair validation、Git clone interop 与 SHA-256 smoke。

## 当前状态

- 公开：四个 subcommands。
- 测试：legacy `command::bundle_test` 6 cases；`compat_import_export_roundtrip::bundle_selectors_unbundle_and_real_git_interoperate`（含 repeated unbundle/system Git clone）与 SHA-256 case；unit header parser。
- 用户文档：EN/zh-CN `docs/commands/bundle.md`。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| create | prerequisite/thin/`A..B` incremental | 延后；只写 full bundle。 |
| clone | （已实现）`libra clone <bundle>` | 先判定仓库目录，再 `<path>.bundle` / `<path>`；HEAD 选择见 ADR-CL-06。 |
| verify | exhaustive pack entry decode | checksum + version/prerequisite；unbundle 建 index 时执行更深验证。 |
| scale | >1 GiB bundle | 明确拒绝。 |

## 维护要求

pack writer/indexer必须保持 hash-kind 中立；安装顺序必须让 pack 在 index 之前可见，失败不得留下可被 reader 误认为完整的 pair。
