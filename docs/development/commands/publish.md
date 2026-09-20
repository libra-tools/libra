# `libra publish` 开发设计

> **已移除（plan-20260920）：** `libra publish`、publish 管线、D1 publish API 与 `worker/` 已于 0.23.0 删除；本文保留为历史开发记录。仓库备份走 `libra cloud`。

## 命令实现目标

`libra publish` 的目标是把仓库发布为只读 Cloudflare Worker 站点，并管理 init/status/deploy/unpublish 等发布流程。实现需要比较发布 refs、跳过已存在 artifact、保护站点 ID 和密钥配置，并提供结构化输出。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Libra Cloudflare publish extension, not a Git command

- 该命令或行为属于 Libra 扩展/有意差异；重点是清晰边界、结构化输出和可测试错误，而不是 Git 完全同形。


## 设计方案

- 入口与分发：已公开接入 `src/cli.rs::Commands`；已由 `src/command/mod.rs` 导出。CLI 层在 `src/cli.rs` 把解析后的参数交给命令模块，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/publish.rs`。参数/子命令类型包括：`PublishArgs`、`PublishCommand`、`InitArgs`、`SyncArgs`、`StatusArgs`、`DeployArgs`、`UnpublishArgs`；输出、错误或状态类型包括：crate 私有输出类型 `PublishInitOutput`、`PublishSyncOutput`、`PublishStatusOutput`、`PublishDeployOutput`、`PublishUnpublishOutput`（均实现 `CommandOutput`），错误通过 `CliResult` 或上层命令错误统一传播；主要执行函数包括：`execute`、`execute_safe`。
- 源码意图：源码模块注释说明 `publish` 的 CLI surface 包括 init/sync/status/deploy/unpublish，并围绕 Worker 模板、D1/R2 同步、Wrangler 部署和下线编排展开。
- 执行路径：`execute_safe` 负责 CLI 安全包装、错误映射和输出配置；对象路径会解析 revision 并读写 blob/tree/commit/tag 等对象；引用路径会读取或更新 SQLite refs、HEAD 与 reflog；数据库路径会通过 SeaORM/SQLite 或 D1 客户端持久化元数据。

- 流程图：以下流程图按当前源码分层展示主路径和底层对象边界，便于维护者把代码入口、执行函数和副作用范围对应起来。

```mermaid
flowchart TD
    A["入口与分发<br/>src/cli.rs::Commands"] --> B["源码分层<br/>src/command/publish.rs"]
    B --> C["参数模型<br/>PublishArgs / PublishCommand / InitArgs"]
    C --> D["执行路径<br/>execute / execute_safe"]
    D --> E["底层对象<br/>D1Client / Storage / StorageExt / Branch"]
    D --> F["输出与错误<br/>CliResult"]
    E --> G["副作用边界<br/>写入分支需先预检"]
```

- 底层操作对象：`D1Client`（Cloudflare D1 元数据读写）；`Storage` / `StorageExt`（对象存储抽象，覆盖本地、remote 和 publish 存储）；`Branch` / branch store（SQLite refs 上的分支读写、过滤和上游关系）；`Head`（SQLite 中的 HEAD 指向、当前分支和 detached 状态）；`Commit`（提交对象、父提交关系和提交消息载荷）；`Tree`（由索引或对象遍历生成的目录树对象）；`Blob`（文件内容或 LFS pointer 写入对象库后的 blob 对象）；`ObjectHash`（SHA-1/SHA-256 对象 ID 和 revision 解析结果）；`LocalStorage`（本地对象或发布存储根目录）；`ObjectType`（blob/tree/commit/tag 类型分派）
- 输出与错误契约：人类输出、`--json` / `--machine` 输出和 quiet/verbose 分支必须继续走现有 `OutputConfig` / `output::emit` / `CliError` 路径；新增失败模式要补稳定错误码、用户提示和回归测试。
- 副作用边界：凡是写入索引、对象库、refs/HEAD、reflog、SQLite/D1、工作树或远端的路径，都必须先完成参数校验和 dry-run/预检分支，再执行持久化，避免部分写入后静默成功。

## 实现历史

- 本节依据本地 main 分支提交历史重写，筛选与该命令实现、测试或文档路径直接相关的提交；以下是归纳后的实现脉络。
- 2026-05-13 `3962996b`（`feat(publish): wire deploy orchestration`）：基础实现节点：wire deploy orchestration；当前实现的主要轮廓可追溯到该提交。
- 2026-05-24 `e233e4eb`（`feat(help): scrub publish 'Codex pass-N' meta + tighten 9 value_names (v0.17.901)`）：功能演进：scrub publish 'Codex pass-N' meta + tighten 9 value_names (v0.17.901)；该节点扩展了当前命令可用的参数或行为。
- 2026-05-14 `ac52908d`（`feat(publish): rebuild all ai projection threads`）：功能演进：rebuild all ai projection threads；该节点扩展了当前命令可用的参数或行为。
- 2026-05-30 `cdd28506`（`fix(publish): ship placeholder worker resources`）：实现修正：ship placeholder worker resources；该节点把边界行为、错误处理或兼容差异纳入当前实现约束。
- 2026-05-23 `2da050fe`（`docs(publish): add Examples section mirroring --help banner (v0.17.848)`）：文档与兼容口径：add Examples section mirroring --help banner (v0.17.848)；当前文档按该节点之后的实现状态校准。
- 历史结论：当前文档应以这些提交之后的代码、测试和兼容矩阵为准；更早的迁移式文档只保留为背景，不再作为事实来源。

## 当前状态

- 公开状态：已公开；模块状态：已导出。
- 用户文档：`docs/commands/publish.md`。
- Synopsis：`libra publish <init|sync|status|deploy|unpublish> [OPTIONS]`。
- 公开参数/子命令包括：`init`、`sync`、`status`、`deploy`、`unpublish` 五个子命令；`init` 支持 `--slug <SLUG>`、`--clone-domain <DOMAIN>`、`--display-origin <URL>`、`--name <NAME>`、`--visibility <MODE>`、`--worker-name <NAME>`、`--max-preview-bytes <BYTES>`；`sync` 支持 `--ref <REF>`、`--dry-run`、`--fail-on-dirty`、`--ai-redaction <POLICY>`、`--allow-sensitive-path <PATH>`、`--force`；`status` 支持 `--site-id <UUID>`；`deploy` 支持 `--skip-deploy`；`unpublish` 支持 `--yes`、`--site-id <UUID>` 等。


## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容矩阵说明 | Libra Cloudflare 发布扩展, 不是 Git 命令 | 按当前兼容矩阵保留；实现状态变化时同步 `_compatibility.md` 和测试证据。 |
| 功能缺口 | `libra publish init` 仅生成 Worker 模板脚手架与清单，尚未把 `--slug` / `--clone-domain` 等参数写入站点配置或密钥（`run_publish_init_at_root` 仍以 `_args` 忽略 `InitArgs`） | 后续实现时需要同步源码、测试和兼容矩阵。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
