# `libra push` 开发设计

## 命令实现目标

`libra push` 的目标是把本地 branch/tag 更新和相关对象发送到远端。实现需要覆盖多 refspec、delete、`--tags`、`--mirror`、dry-run、force 和本地 file remote 的有意拒绝。

## 对比 Git 与兼容性

- 兼容级别：`partial`。branch/tag update, multi-refspec, delete (`-d`/`--delete` 或 `:<ref>` refspec), `--tags`, and `--mirror` supported; `--force-with-lease[=<ref>[:<expect>]]`（发送前校验远端仍匹配 tracking-ref/expected OID，与 `--force` 互斥）和 `--porcelain`（机器可读的每 ref 行，与 `--json`/`--machine` 互斥）supported；`--atomic` supported（经 `resolve_atomic_capability` 在远端 discovery 通告 `atomic` 时附加该 capability，使远端要么全部更新要么全部不更新；远端未通告则提前以 `PushError::AtomicUnsupported` 拒绝）；`--push-option`/`-o <opt>` supported（经 `resolve_push_options_capability` 在远端通告 `push-options` 时附加 capability + 在命令 flush 后经 `encode_push_options` 追加 push-options 段；未通告则 `PushError::PushOptionsUnsupported`）；`--follow-tags` supported（经 `collect_follow_tag_refs`：列出 annotated tag，其 target 经 `is_ancestor` 可达任一被推送 ref 的 tip 且远端缺失时，由 `follow_tag_should_push` 选中并加入推送计划）；`--signed` supported（经 `resolve_push_cert_nonce` 在远端通告 `push-cert[=<nonce>]` 时取 nonce，`build_push_certificate` 构造 `certificate version 0.1` 文本，复用 vault `pgp_sign`/`signature_to_armored` 签名，`encode_push_cert_section` 以 `push-cert\0<caps>` … `push-cert-end` 帧封装；未通告则 `PushError::PushSignUnsupported`，无签名密钥则 `PushSignNoKey`）；`--no-progress` supported（经 `progress_output_config(output, args.no_progress)` 在 `--no-progress` 时把传给 “Compressing objects”/“Writing objects” `ProgressReporter` 的 output 的 `progress` 强制为 `ProgressMode::None`，抑制进度条，对齐 `git push --no-progress`）；`--force-if-includes`、`--thin`/`--no-thin` 与 `--no-verify`（Git 兼容接受入口；Libra 的 push 不运行客户端 `pre-push` hook，且 Git hooks bridge 按 D3 拒绝，故无可绕过）作为 **no-op** 接受。local file remote rejected — intentional (see [docs/development/commands/_compatibility.md#d2-本地-file-remote-的-push](docs/development/commands/_compatibility.md#d2-本地-file-remote-的-push))

- 当前矩阵明确仍是部分兼容；未覆盖的 Git surface 必须显式列在“还未实现的功能”。


## 设计方案

- 入口与分发：已公开接入 `src/cli.rs::Commands`；已由 `src/command/mod.rs` 导出。CLI 层在 `src/cli.rs` 把解析后的参数交给命令模块，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/push.rs`。参数/子命令类型包括：`PushArgs`；输出、错误或状态类型包括：`PushError`、`PushRefUpdateKind`、`PushRefUpdate`、`PushOutput`；主要执行函数包括：`execute`、`execute_safe`、`run_push`。
- 源码意图：源码模块注释说明该命令读取 remote 配置、与服务器协商，并发送本地 refs 与 pack 数据完成远端更新。
- 执行路径：`execute_safe` 负责 CLI 安全包装、错误映射和输出配置；核心领域逻辑集中在 `run_push`；对象路径会解析 revision 并读写 blob/tree/commit/tag 等对象；引用路径会读取或更新 SQLite refs、HEAD 与 reflog；网络路径会解析 remote 配置、协商协议并处理 pack/idx 数据；数据库路径会通过 SeaORM/SQLite 或 D1 客户端持久化元数据。

- 流程图：以下流程图按当前源码分层展示主路径和底层对象边界，便于维护者把代码入口、执行函数和副作用范围对应起来。

```mermaid
flowchart TD
    A["入口与分发<br/>src/cli.rs::Commands"] --> B["源码分层<br/>src/command/push.rs"]
    B --> C["参数模型<br/>PushArgs"]
    C --> D["执行路径<br/>execute / execute_safe / run_push"]
    D --> E["底层对象<br/>Branch / Head / ReflogContext / Reflog::insert_single_entry"]
    D --> F["输出与错误<br/>PushError / PushRefUpdateKind / PushRefUpdate"]
    E --> G["副作用边界<br/>写入分支需先预检"]
```

- 底层操作对象：SSH transport（SSH remote 连接和认证）；pack / idx 对象（传输包、索引、delta 和完整性校验）；`Branch` / branch store（SQLite refs 上的分支读写、过滤和上游关系）；`Head`（SQLite 中的 HEAD 指向、当前分支和 detached 状态）；`ReflogContext` / `Reflog::insert_single_entry`（在数据库事务内直接写入 SQLite reflog 和动作记录）；`Commit`（提交对象、父提交关系和提交消息载荷）；`Tree`（由索引或对象遍历生成的目录树对象）；`Blob`（文件内容或 LFS pointer 写入对象库后的 blob 对象）；`TreeItem` / `TreeItemMode`（tree 中的路径项和 mode）；SeaORM / `.libra/libra.db`（配置、refs、reflog、AI/发布元数据等 SQLite 表）；`ObjectHash`（SHA-1/SHA-256 对象 ID 和 revision 解析结果）；`ConfigKv`（配置键值持久化行）
- 输出与错误契约：人类输出、`--json` / `--machine` 输出和 quiet/verbose 分支必须继续走现有 `OutputConfig` / `emit_json_data` / `CliError` 路径；新增失败模式要补稳定错误码、用户提示和回归测试。
- 全局配置 schema 保护（P0-12）：CLI dispatch 前通过 `utils::client_storage::inspect_global_config_schema_future` 检查 `~/.libra/config.db` / `LIBRA_CONFIG_GLOBAL_DB`。`push` 默认把 future schema 视为 fail-closed 配置错误，返回 `LBR-CONFIG-001`（category `config`，exit 128），避免静默忽略全局 tiered storage 配置；`--offline` 或 `LIBRA_READ_POLICY=offline|local` 明确降级时仅 warning 一次并继续本地对象访问。诊断必须包含二进制路径、二进制版本、配置 DB 路径、当前/支持的 schema 版本和升级命令，且不得泄露 `vault.env.*` secret。回归测试：`compat_global_config_schema_future`。
- 副作用边界：凡是写入索引、对象库、refs/HEAD、reflog、SQLite/D1、工作树或远端的路径，都必须先完成参数校验和 dry-run/预检分支，再执行持久化，避免部分写入后静默成功。

## 实现历史

- 本节依据本地 main 分支提交历史重写，筛选与该命令实现、测试或文档路径直接相关的提交；以下是归纳后的实现脉络。
- 2025-11-27 `a4e9881b`（`feat: add force push support to push command (#69)`）：基础实现节点：add force push support to push command (#69)；当前实现的主要轮廓可追溯到该提交。
- 2026-06-07 `6b11a315`（`feat(push): add atomic push safety`）：功能演进：add atomic push safety；该节点新增的 `--atomic` 等 flag 已在后续提交回退，当前 `PushArgs` 不再公开。
- 2026-06-06 `e507dc57`（`feat(push): add --force-with-lease, --porcelain, and no-op compat flags (#1389)`）：功能演进：add --force-with-lease, --porcelain, and no-op compat flags (#1389)；该节点新增的 `--force-with-lease` / `--porcelain` / `--force-if-includes` / `--thin`/`--no-thin` 等 flag 曾被一次 reconcile 丢失内容，已于 2026-06-18 恢复到当前代码（lease 校验 + porcelain 输出 + no-op 兼容 flag），`PushArgs` 重新公开这些参数。
- 2026-05-29 `3a4990e8`（`fix(push): set upstream for up-to-date refspec`）：实现修正：set upstream for up-to-date refspec；该节点把边界行为、错误处理或兼容差异纳入当前实现约束。
- 历史结论：当前文档应以这些提交之后的代码、测试和兼容矩阵为准；更早的迁移式文档只保留为背景，不再作为事实来源。

## 当前状态

- 公开状态：已公开；模块状态：已导出。
- 用户文档：`docs/commands/push.md`。
- Synopsis：`libra push [OPTIONS] [<repository> [<refspec>...]]`。
- 公开参数/子命令包括：`[<repository>]`、`[<REFSPEC>...]`、`-u, --set-upstream`、`-f, --force`、`-d, --delete`、`--force-with-lease[=<ref>[:<expect>]]`、`--force-if-includes`、`--thin`、`--no-thin`、`--no-verify`（Git 兼容接受入口；接受式 no-op，字段 `no_verify` 解析后不被读取，Libra 的 push 不运行客户端 `pre-push` hook，且不支持 `.git/hooks` / `core.hooksPath` bridge）、`--no-progress`（**实际生效**：经 `progress_output_config` 把进度 output 强制为 `ProgressMode::None`，抑制 “Compressing/Writing objects” 进度条）、`--porcelain`、`-n, --dry-run`、`--tags`、`--mirror`。`-d`/`--delete` 在 `execute_safe` 入口经纯函数 `apply_delete_flag` 把每个位置 REFSPEC（须为不含 `:` 的纯 ref 名）改写为 `:<ref>` 删除请求，复用既有删除路径；缺少 ref、含 `:` 的 refspec、或与 `--set-upstream`/`--tags`/`--mirror` 组合均报错。


## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 边界 | `--signed` 与 `-o`/`--push-option` 同时使用 | push 证书段取代普通命令段，故 signed 路径暂不另发 push-options 段（罕见组合）；如需可后续把 push-option 行并入证书。 |
| 运行期校验 | push-cert 线格式的真实服务端往返 | `build_push_certificate`/`encode_push_cert_section` 已按 git pack-protocol 实现并单测（证书文本 + 帧字节 + capability 门控），实际服务端接受属 L2（需支持 push-cert 的服务器）。 |

（push 标志全部已实现：`--atomic`/`--push-option`(`-o`)/`--follow-tags`/`--signed`。`resolve_atomic_capability`/`resolve_push_options_capability`/`resolve_push_cert_nonce` 在远端 discovery 通告对应 capability 时附加，未通告则以 `PushError::AtomicUnsupported`/`PushOptionsUnsupported`/`PushSignUnsupported` 拒绝；push-options 段经 `encode_push_options`、push-cert 段经 `encode_push_cert_section` 写入；`--follow-tags` 经 `collect_follow_tag_refs` + `is_ancestor` + `follow_tag_should_push`。）

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
