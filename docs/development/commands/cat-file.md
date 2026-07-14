# `libra cat-file` 开发设计

## 命令实现目标

`libra cat-file` 的目标是读取对象仓库中的对象类型、大小和内容，并提供批处理查询能力。实现需要兼顾 Git plumbing 的退出码语义、批量 stdin 解析、对象遍历和符号链接解析；`-e --json`/`--machine` 在保留退出码契约的前提下输出 `{ exists: bool }` 信封。

## 对比 Git 与兼容性

- 兼容级别：`partial`。`-t` / `-s` / `-p` / `-e` / `--batch-check` / `--batch`（带内容输出）/ `--batch-command`（info/contents）/ `--batch-all-objects`（配合 --batch/--batch-check，遍历 loose+packed，按 id 排序）/ batch 格式串（`=<format>`）/ `--buffer`（缓冲 batch 输出，使 `--batch-command` 的 `flush` 命令生效；需配合 batch 模式）已支持。对象参数共用严格 resolver，支持 `@`、数字 reflog、parent/ancestor、typed/recursive peel、完整 tag ref 和 `REV:path`；`-e --json`/`--machine` 输出 `{ exists: bool }` 信封（保留退出码：存在 0 / 缺失 1 / 非法名 129）已支持（`--follow-symlinks` 未公开）。

- 当前矩阵承诺常用 Git 行为已支持；新增语义必须同步矩阵、用户文档和测试。


## 设计方案

- 入口与分发：已公开接入 `src/cli.rs::Commands`；已由 `src/command/mod.rs` 导出。CLI 层在 `src/cli.rs` 把解析后的参数交给命令模块，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/cat_file.rs`。参数/子命令类型包括：`CatFileArgs`；输出、错误或状态类型包括：源码未暴露独立输出/错误类型，错误通过 `CliResult` 或上层命令错误统一传播；主要执行函数包括：`execute`、`execute_safe`。
- 执行路径：`execute_safe` 负责 CLI 安全包装、错误映射和输出配置；Git 对象路径委托 `utils::util::resolve_object_spec_typed`，严格解析 revision、peel 与 tree path 后只读 blob/tree/commit/tag。Batch 仅把不可解析或不存在对象输出为 `<spec> missing`；ref/对象库读取失败或损坏必须作为结构化错误返回，不能静默降级为 missing。AI 路径另行读取 session、checkpoint、thread graph 或 agent profile 状态。

- 流程图：以下流程图按当前源码分层展示主路径和底层对象边界，便于维护者把代码入口、执行函数和副作用范围对应起来。

```mermaid
flowchart TD
    A["入口与分发<br/>src/cli.rs::Commands"] --> B["源码分层<br/>src/command/cat_file.rs"]
    B --> C["参数模型<br/>CatFileArgs"]
    C --> D["执行路径<br/>execute / execute_safe"]
    D --> E["底层对象<br/>Blob / Commit / TreeItem / TreeItemMode"]
    D --> F["输出与错误<br/>CliResult"]
    E --> G["副作用边界<br/>写入分支需先预检"]
```

- 底层操作对象：`Blob`（文件内容或 LFS pointer 写入对象库后的 blob 对象）；`Commit`（提交对象、父提交关系和提交消息载荷）；`TreeItem` / `TreeItemMode`（tree 中的路径项和 mode）；`Tree`（由索引或对象遍历生成的目录树对象）；`ClientStorage`（本地/分层对象存储读写入口）；`LocalStorage`（本地对象或发布存储根目录）；SeaORM / `.libra/libra.db`（配置、refs、reflog、AI/发布元数据等 SQLite 表）；`ObjectHash`（SHA-1/SHA-256 对象 ID 和 revision 解析结果）；`ObjectType`（blob/tree/commit/tag 类型分派）；session/thread store（AI 会话、线程、事件和恢复状态）
- 输出与错误契约：人类输出、`--json` / `--machine` 输出和 quiet/verbose 分支必须继续走现有 `OutputConfig` / `emit_json_data` / `CliError` 路径；新增失败模式要补稳定错误码、用户提示和回归测试。
- 副作用边界：凡是写入索引、对象库、refs/HEAD、reflog、SQLite/D1、工作树或远端的路径，都必须先完成参数校验和 dry-run/预检分支，再执行持久化，避免部分写入后静默成功。

## 实现历史

- 本节依据本地 main 分支提交历史重写，筛选与该命令实现、测试或文档路径直接相关的提交；以下是归纳后的实现脉络。
- 2026-06-04 `e832538e`（`feat(cat-file): implement basic batch-check engine and stdin stream parser (v0.17.1299)`）：当前 `CatFileArgs` 已公开 `--batch-check`（含可选 `=<format>` 原子展开）与 `--batch` stdin 模式；当前事实以源码为准。
- 2026-06-04 `2946746e`（`feat(cat-file): support batch-all-objects scanning and follow-symlinks resolution (v0.17.1301)`）：当前 `CatFileArgs` 已公开 `--batch-all-objects`（配合 --batch/--batch-check 遍历 loose+packed，`collect_all_object_ids` 走 objects/AB/CDEF + 解析 objects/pack/*.idx，去重并按 id 排序）；`--follow-symlinks` 仍未公开；当前事实以源码为准。
- 2026-06-04 `dac8f161`（`feat(cat-file): support full batch content output and buffering control (v0.17.1300)`）：当前 `CatFileArgs` 已公开 `--batch` 完整内容输出与 `--batch-command`（info/contents）；该提交未公开 `--buffer`（故当时 `flush` 报错）；`--buffer` 于后续提交公开（使 `flush` 生效），当前事实以源码为准。
- 2026-05-24 `8a1c5784`（`fix(cat-file): tighten error code mapping for remaining legacy paths`）：实现修正：tighten error code mapping for remaining legacy paths；该节点把边界行为、错误处理或兼容差异纳入当前实现约束。
- 2026-06-07 `a3d7a262`（`test(cat-file): isolate batch helper processes`）：测试契约：isolate batch helper processes；相关行为已有回归守卫，后续变更需要继续满足。
- 2026-07-14 P1-09：Git 对象模式切到共享严格 revision resolver，增加 typed/recursive peel、完整 tag ref、数字 reflog 与 `REV:path`；batch 对损坏对象 fail closed，不再误报 `<spec> missing`。
- 历史结论：当前文档应以这些提交之后的代码、测试和兼容矩阵为准；更早的迁移式文档只保留为背景，不再作为事实来源。

## 当前状态

- 公开状态：已公开；模块状态：已导出。
- 用户文档：`docs/commands/cat-file.md`。
- Synopsis：`libra cat-file [OPTIONS] [OBJECT]`。
- 公开参数/子命令包括：`-t`、`-s`、`-p`、`-e`、`--batch-check`、`--batch`、`--batch-command`、`--batch-all-objects`、`--ai <ID>`、`--ai-type <ID>`、`--ai-list <TYPE>`、`--ai-list-types`、`[OBJECT]`。`[OBJECT]` 与 batch 输入共用严格对象 resolver。`--batch-check` 与 `--batch` 共享 `build_batch_object`：前者仅 header，后者 header + raw 内容 + 换行；不存在/不可解析对象输出 missing，读取失败或损坏返回 `CliError`；每个对象单次缓冲写出以统一处理 BrokenPipe。`--batch-command` 从 stdin 逐行解析命令 `info <object>`（同 --batch-check）/ `contents <object>`（同 --batch），复用 `build_batch_object`；`flush` 在无 `--buffer` 时报 `LBR-CLI-002`，加 `--buffer` 则有效（缓冲输出，遇 `flush` 写出并清空 pending，EOF 时写出剩余）；未知命令报错。`--buffer` 不配合任何 batch 模式时报 `LBR-CLI-002`。`--batch-all-objects`（配合 `--batch`/`--batch-check`）以 `collect_all_object_ids`（loose objects/AB/CDEF + 解析 objects/pack/*.idx，去重并按 id 排序）替代 stdin 列表；缺 mode 时报 `LBR-CLI-002`。


## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容矩阵说明 | `-e` 已支持，且 `-e --json`/`--machine` 输出 `{ exists: bool }`（见下方已实现行） | 按当前兼容矩阵保留；实现状态变化时同步 `_compatibility.md` 和测试证据。 |
| ✅ 已实现 | `cat-file -e --json`/`--machine` 输出 `{ exists: bool }` 信封（`mode: "exists"`），同时保留 `-e` 的退出码语义：对象存在 exit 0、格式正确但缺失 exit 1（JSON 仍写到 stdout）、非法对象名 exit 129（`LBR-CLI-003`，`resolve_object_safe`，与非 JSON `-e` 路径一致，是 Libra 标准 invalid-target 码，与 Git 的 128 有意不同）。带集成测试 `test_cat_file_exist_check_json`。 | — |
| ✅ 已实现（部分） | Batch mode `--batch-check` | 从 stdin 逐行读对象名，输出 `<sha> <type> <size>`，无法解析/不存在时输出 `<input> missing`；ref 或对象库读取失败、损坏会 fail closed 并保留稳定错误码。支持可选 `=<format>` 原子展开（`%(objectname)`/`%(objecttype)`/`%(objectsize)`）。带 stdin 集成测试。 |
| ✅ 已实现（部分） | Batch mode `--batch` | 从 stdin 逐行读对象名，输出 `<sha> <type> <size>` + raw 对象内容 + 换行（二进制安全，单次缓冲写出），无法解析时输出 `<input> missing`；与 `--batch-check` 共享 `build_batch_object` 及同一 `=<format>` 原子展开。带 stdin 集成测试。 |
| ✅ 已实现（部分） | Batch mode `--batch-command` + `--buffer` | 从 stdin 逐行读命令：`info <object>`（header，同 --batch-check）/ `contents <object>`（header + 内容，同 --batch），复用 `build_batch_object`（含同一 `=<format>`）。无 `--buffer` 时 `flush` 报错且每命令即时写出；加 `--buffer` 时缓冲输出、`flush` 有效（写出+清空 pending）、EOF 写出剩余（输出字节与非缓冲一致）。`--buffer` 需 batch 模式否则报错。未知命令报错。带 stdin 集成测试（含 `--buffer`/`flush` 与无 batch 模式两例）。 |
| ✅ 已实现（部分） | Batch mode `--batch-all-objects` | 配合 `--batch`/`--batch-check`，用 `collect_all_object_ids`（loose objects/AB/CDEF + 解析 objects/pack/*.idx 的 `parse_index`，去重并按 id 排序）替代 stdin 对象列表；缺 `--batch`/`--batch-check` 时报 `LBR-CLI-002`。带集成测试。 |
| 兼容差异项 | Batch mode（其余） | 原始对照：未实现；相关参数/替代：`--follow-symlinks`；当前说明：延后（`--buffer` 已实现）。 后续实现时需要补对应回归测试并同步兼容矩阵。 |
| 兼容差异项 | 不支持 argument combination | 当前状态：LBR-CLI-002；Git/相关参数：129。 后续实现时需要补对应回归测试并同步兼容矩阵。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
