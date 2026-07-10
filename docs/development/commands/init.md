# `libra init` 开发设计

## 命令实现目标

`libra init` 的目标是在目录中初始化 Libra 仓库，创建数据库、对象目录、默认 refs、索引和必要配置。在已存在仓库上再次 `init` 执行 Git 式安全重初始化（`is_reinit()` 检测后走 `reinitialize_existing()`：补齐标准布局、重应用 `--shared`、更新 `core.sharedRepository`、保留数据库 config/HEAD/refs/objects/vault/repoid 的其余状态，打印 `Reinitialized existing ...`），并支持 shared 模式、从 Git 布局迁移的边界、JSON 输出和错误码，且明确子模块递归初始化不在当前范围。

## 对比 Git 与兼容性

- 兼容级别：`partial`。新仓库初始化已支持；对已有 Libra 仓库执行安全 re-init/top-up 已实现（保留 DB，补齐布局，`InitOutput.reinitialized=true`，banner 改为 `Reinitialized existing ...`）。`--shared` 支持 `false`/`umask`/`true`/`group`/`all`/`world`/`everybody` 与 4 位八进制 numeric mode；numeric mode 在副作用前校验目录可遍历性，不满足时以 `LBR-CLI-002` fail-closed 且不创建 `.libra`。

- 当前矩阵承诺常用 Git 行为已支持；新增语义必须同步矩阵、用户文档和测试。


## 设计方案

- 入口与分发：已公开接入 `src/cli.rs::Commands`；已由 `src/command/mod.rs` 导出。CLI 层在 `src/cli.rs` 把解析后的参数交给命令模块，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/init.rs`。参数/子命令类型包括：`InitArgs`；输出、错误或状态类型包括：`InitError`、`InitOutput`；主要执行函数包括：`execute`、`execute_safe`。
- 执行路径：`execute_safe` 负责 CLI 安全包装、错误映射和输出配置；引用路径会读取或更新 SQLite refs、HEAD 与 reflog；数据库路径会通过 SeaORM/SQLite 或 D1 客户端持久化元数据；AI 路径会读写 session、checkpoint、thread graph 或 agent profile 状态。

- 流程图：以下流程图按当前源码分层展示主路径和底层对象边界，便于维护者把代码入口、执行函数和副作用范围对应起来。

```mermaid
flowchart TD
    A["入口与分发<br/>src/cli.rs::Commands"] --> B["源码分层<br/>src/command/init.rs"]
    B --> C["参数模型<br/>InitArgs"]
    C --> D["执行路径<br/>execute / execute_safe"]
    D --> E["底层对象<br/>reference::ActiveModel (ConfigKind::Head/Branch) / .libra/libra.db / ConfigKv"]
    D --> F["输出与错误<br/>InitError / InitOutput"]
    E --> G["副作用边界<br/>写入分支需先预检"]
```

- 底层操作对象：`reference::ActiveModel`（直接写入 `ConfigKind::Head` 与 `ConfigKind::Branch` 的 refs 行，无 `Branch` / `Head` 类型实例化）；SeaORM / `.libra/libra.db`（配置、refs、reflog、AI/发布元数据等 SQLite 表）；Vault/libvault（身份、密钥或 vault-backed 签名边界）；agent checkpoint（Agent 运行快照、回放和 transcript 截断输入）；`ConfigKv`（配置键值持久化行）
- 输出与错误契约：人类输出、`--json` / `--machine` 输出和 quiet/verbose 分支必须继续走现有 `OutputConfig` / `emit_json_data` / `CliError` 路径；新增失败模式要补稳定错误码、用户提示和回归测试。
- 副作用边界：凡是写入索引、对象库、refs/HEAD、reflog、SQLite/D1、工作树或远端的路径，都必须先完成参数校验和 dry-run/预检分支，再执行持久化，避免部分写入后静默成功。

## 实现历史

- 本节依据本地 main 分支提交历史重写，筛选与该命令实现、测试或文档路径直接相关的提交；以下是归纳后的实现脉络。
- 2026-01-19 `ae8c949a`（`add tests(init): handle unwritable parent dir, fix pre-commit hook generation, and clarify HEAD default behavior (#151)`）：基础实现节点：add tests(init): handle unwritable parent dir, fix Libra-owned `.libra/hooks/pre-commit.*` template generation, and clarify HEAD default behavior (#151)；当前实现的主要轮廓可追溯到该提交。该历史节点不表示支持 Git hooks bridge；`.git/hooks` / `core.hooksPath` 仍按 D3 拒绝。
- 2026-06-05 `d29da9bf`（`feat(init): support safe re-initialization of existing repositories`）：该提交曾尝试支持安全重新初始化，但在后续 reconcile 中丢失（[[goal_loop_work_vanished]] 模式）。
- 2026-06-25 (#156)：重新落地安全重初始化。`run_init_internal` 中 `is_reinit()` 命中后改走 `reinitialize_existing()`：`prepare_repository_layout` 补齐缺失模板、重应用 `--shared`、用 `get_db_conn_instance_for_path` 连接现有 DB（而非 `create_database_connection`，后者会拒绝已存在文件），从 config/HEAD 读回 objectformat/initrefformat/repoid/bare/vault.signing/分支填充 `InitOutput`（新增 `reinitialized: bool`），banner 改为 `Reinitialized existing{bare} Libra repository in ...`。`--from-git-repository` 在已存在仓库上拒绝（`InvalidArgument`）；`--initial-branch`/`--object-format` 若与现有不符则忽略并 warn。删除了不再可达的 `InitError::AlreadyInitialized` 变体。回归测试 `init_bare_reinit_tops_up_and_preserves_state` / `init_worktree_reinit_tops_up_and_preserves_state`。
- 2026-06-05 `901b433b`（`feat(init): persist core.sharedRepository and isolate vault.db from --shared chmod`）：功能演进：persist core.sharedRepository and isolate vault.db from --shared chmod；该节点扩展了当前命令可用的参数或行为。
- 2026-07-09（plan-20260708 P0-10）：当前 main 重新核对后修复 `--shared` 语义漂移：`parse_shared_mode()` 在任何仓库写入前解析并拒绝不可遍历 numeric mode（如 `0660`）；fresh init 的 `init_config()` 与 reinit 的 `persist_shared_repository()` 都持久化规范化后的 `core.sharedRepository`；Unix `apply_shared()` 复用解析结果，继续跳过 vault 数据库及 sidecar。回归测试 `compat_init_shared_mode`。
- 2026-06-07 `99c39206`（`fix(init): close compatibility plan gaps`）：实现修正：close compatibility plan gaps；该节点把边界行为、错误处理或兼容差异纳入当前实现约束。
- 历史结论：当前文档应以这些提交之后的代码、测试和兼容矩阵为准；更早的迁移式文档只保留为背景，不再作为事实来源。

## 当前状态

- 公开状态：已公开；模块状态：已导出。
- 用户文档：`docs/commands/init.md`。
- Synopsis：`libra init [OPTIONS] [DIRECTORY]`。
- 公开参数/子命令包括：`[DIRECTORY]`、`--bare`、`-b, --initial-branch <INITIAL_BRANCH>`、`--object-format <format>`、`--from-git-repository <path>`、`--vault <VAULT>`、`--template <template-directory>`、`--shared <MODE>`、`--ref-format <REF_FORMAT>`、`-q, --quiet`。


## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容差异项 | Recurse submodules | 原始对照：git init + git submodule init；相关参数/替代：不适用；当前说明：不适用 (submodules not 支持)。 后续实现时需要补对应回归测试并同步兼容矩阵。 |
| ✅ 已实现 | 安全重新初始化（在已存在仓库上再次 `init`） | `git init` 风格：打印 `Reinitialized existing{bare} Libra repository in ...`、补齐缺失标准布局（模板/目录）、重应用 `--shared`，并完整保留现有数据库（config/HEAD/refs/objects/vault/repoid）。`reinitialize_existing()` 用 `get_db_conn_instance_for_path` 连接现有 DB，从 config/HEAD 读回身份/格式填充 `InitOutput`（`reinitialized=true`）。`--from-git-repository` 在已初始化仓库上拒绝；`--initial-branch`/`--object-format` 与现有不符则忽略并 warn。带回归测试。 |
| ✅ 已实现 | `--shared` 配置持久化与 numeric fail-closed | `true`/`group` 规范化为 `group`，`all`/`world`/`everybody` 规范化为 `all`，numeric mode 保留原始八进制文本，均写入 `core.sharedRepository`。numeric mode 要求 owner `rwx`，且 group/other 若有 read/write 必须有 execute；否则在创建 `.libra` 前返回 `LBR-CLI-002`。带回归测试 `compat_init_shared_mode`。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
