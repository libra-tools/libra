# `libra notes` 开发设计

## 命令实现目标

`libra notes` 的目标是管理提交 notes，包括 add、append、copy、edit、show、list、remove、merge 等操作。当前已实现并公开接入顶层 CLI；`notes merge` 是对扁平 note 行的 2-way 合并（Libra notes 是 SQLite 行、非 commit-backed tree，故无 3-way base），支持 `--strategy=manual|ours|theirs|union|cat_sort_uniq`；`prune`（删除 annotated 对象已不存在的 note）与 `get-ref`（打印生效 notes ref）已实现；`add`/`edit`/`append` 在无 `-m`/`-F` 时的交互式编辑器回退已实现（`edit` 与 `add -f` 预填现有 note）。

## 对比 Git 与兼容性

- 兼容级别：`partial`。基础 add/append/copy/edit/show/list/remove 与 `merge`（2-way 扁平行合并，`--strategy=manual|ours|theirs|union|cat_sort_uniq`，manual 在冲突时中止、无 NOTES_MERGE worktree）已公开；`prune`（按对象存在性删除孤立 note，支持 `-n`/`--dry-run`、`-v`）与 `get-ref`（打印生效 `--ref`，默认 refs/notes/commits）已实现；`add`/`edit`/`append` 在无 `-m`/`-F` 时的交互式编辑器回退已实现（`edit` 预填现有 note，note 保留 `#` 行）。


## 设计方案

- 入口与分发：`src/cli.rs::Commands::Notes` 公开顶层 CLI，`src/command/mod.rs` 与 `src/internal/mod.rs` 均导出 `notes` 模块；CLI 层在 `src/cli.rs` 把解析后的参数交给 `command::notes::execute_safe`，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/notes.rs`。参数/子命令类型包括：`NotesArgs`、`NotesSubcommand`；输出、错误或状态类型包括：`NotesOutput`、`NotesListEntry`、`NotesRemovedEntry`；主要执行函数包括：`execute`、`execute_safe`。
- 执行路径：`execute_safe` 负责 CLI 安全包装、错误映射和输出配置。

### 只读查询与 operation 分类（ADR-BRL-01 / #574 BRL-02）

- `notes_is_read_only_query` 与 `execute_safe` 共用 `default_notes_subcommand`：缺省子命令是 `list`。
- `List`、`Show`、`GetRef`，以及 `Prune { dry_run: true }` 为 `ReadOnly`。`Add` / `Append` / `Edit` / `Copy` / `Remove` / `Merge` 与非 dry-run `Prune` 保持 `RepoMutation`。
- `cli.rs` 只调用该谓词；`command_scope` 仍为 `Repository`。

- 流程图：以下流程图按当前源码分层展示主路径和底层对象边界，便于维护者把代码入口、执行函数和副作用范围对应起来。

```mermaid
flowchart TD
    A["入口与分发<br/>Commands::Notes / public CLI"] --> B["源码分层<br/>src/command/notes.rs"]
    B --> C["参数模型<br/>NotesArgs / NotesSubcommand"]
    C --> D["执行路径<br/>execute / execute_safe"]
    D --> E["底层对象<br/>blob 写入对象库 + SQLite notes 表"]
    D --> F["输出与错误<br/>NotesOutput / NotesListEntry / NotesRemovedEntry"]
    E --> G["副作用边界<br/>写 blob 对象 + SQLite notes 行"]
```

- 底层操作对象：`src/internal/notes.rs` 的 `add()` 把消息内容写成 blob 并经对象库 `put` 持久化，同时向 SQLite `notes` 表写入 (`notes_ref`、`object`、`blob`) 映射行；因此实现直接触达 Git 对象库与 SQLite 存储（但不写 refs/索引）。
- 输出与错误契约：人类输出、`--json` / `--machine` 输出和 quiet/verbose 分支必须继续走现有 `OutputConfig` / `emit_json_data` / `CliError` 路径；新增失败模式要补稳定错误码、用户提示和回归测试。
- 副作用边界：当前实现的 `add()` 写入 blob 对象与 SQLite `notes` 行（show/list/remove 含读取与删除行）；后续扩展持久化能力时，需要补齐回滚语义和测试证据。

## 实现历史

- 本节依据本地 main 分支提交历史重写，筛选与该命令实现、测试或文档路径直接相关的提交；以下是归纳后的实现脉络。
- 2026-06-10 `5076e26c`（`feat(notes):implement notes command (#380)`）：基础实现节点：implement notes command (#380)；当前实现的主要轮廓可追溯到该提交。
- 历史结论：`src/command/notes.rs` 与 `src/internal/notes.rs` 已实现，当前 `src/cli.rs::Commands` 已公开 `notes` 入口。

## 当前状态

- 公开状态：已公开；模块状态：`src/command/mod.rs` 导出 `notes`，`src/internal/mod.rs` 导出 `notes`，`src/cli.rs::Commands::Notes` 负责 CLI 接入。
- 用户文档：`docs/commands/notes.md`。
- Synopsis：`libra notes [--ref <ref>] add [-m <message>]... [-F <file>]... [-f] [<object>]`（`-m`/`-F` 可重复并按命令行顺序任意混用）；`libra notes [--ref <ref>] merge [-s|--strategy <manual|ours|theirs|union|cat_sort_uniq>] <other-ref>`。
- 公开参数/子命令包括：`Subcommands`（含 `merge`）、`Flag examples`。`merge` 把 `<other-ref>` 的 note 行合并进当前 `--ref`（默认 refs/notes/commits）：仅在 `<other>` 的对象→复制、相同→跳过、不同→按 `--strategy` 解决（`manual` 默认，冲突即中止且不改任何行；`ours`/`theirs`/`union`/`cat_sort_uniq` 自动解决；未知 strategy→`LBR-CLI-002`/129）。


## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容矩阵 | `COMPATIBILITY.md` 已登记该命令。 | 已纳入用户可见兼容矩阵和矩阵守卫。 |
| ✅ 已实现 | Append | `notes append` 在现有 note 后追加（空行分隔；无 note 时新建，复用 `notes::show` 读取 + `notes::add(force)` 写入）。带集成测试（`notes_append_concatenates_to_existing_note`、`notes_append_creates_note_when_absent`）。 |
| ✅ 已实现 | Copy | `notes copy [-f] <from> <to>` 复用 `notes::show(from)`（源无 note 报错）+ `notes::add(to, text, force)`（目标已有 note 且无 `-f` 报错）。带集成测试（`notes_copy_duplicates_note_to_another_object`、`notes_copy_fails_when_source_has_no_note`）。 |
| ✅ 已实现 | Edit | `notes edit` 无条件设置（替换）note，不存在则新建（区别于 `add` 已存在即失败）；复用 `notes::add(force=true)`。无 `-m`/`-F` 时打开编辑器并以现有 note 预填（见下方 Editor support 行）。带集成测试（`notes_edit_sets_and_replaces_note`、`edit_without_message_prefills_existing_note_in_editor`）。 |
| ✅ 已实现 | Merge | `notes merge <other-ref>`：2-way 扁平行合并（无 3-way base，Libra notes 是 SQLite 行）。`--strategy=manual`（默认，冲突中止、all-or-nothing、无 NOTES_MERGE worktree）/`ours`/`theirs`/`union`/`cat_sort_uniq`。带集成测试（`test_notes_merge_strategies_copy_and_manual_conflict`：manual-abort/theirs/copy/union/未知 strategy）。 |
| ✅ 已实现 | `notes prune` + `notes get-ref` | `internal::notes::prune(notes_ref, dry_run)`：`list(notes_ref, None)` 取该 ref 全部扁平行，对每行的 `annotated_object` 判存在性：`objects_storage().get(&hash)` 为 `Ok` 则保留，`Err(GitError::ObjectNotFound)` 或 id 无法解析则判 stale，其它读取错误（瞬时/损坏/分级存储）带 context 中止整个 prune（绝不据此删除）；收集 stale（sort+dedup）；对象存在性检查只把 `GitError::ObjectNotFound`（或无法解析的 id）判为 stale；其它读取错误（瞬时/损坏/分级存储）则带 context 中止，绝不删除。非 dry-run 时在单事务内按 `(notes_ref, object, blob)` 做 compare-and-swap `DELETE`（**不**走 `resolve_object`——目标对象已不存在故必失败；blob 来自 `list` 行），仅当某行 `rows_affected>0`（即并发未改写该 note）才计入 pruned 并返回。CLI `Prune { dry_run(-n/--dry-run), verbose(-v) }`：缺省静默，`-v`/`-n` 打印每个 pruned 对象**全** id（与 git 一致）。`GetRef` 打印 `notes_ref`。带集成测试（`prune_removes_notes_for_missing_objects_only`：删除 c2 loose object 后 prune 移除其 note、保留 HEAD note；dry-run 仅报告；`get_ref_prints_active_notes_ref`）。早先"非收敛"顾虑（存储层严格性）由直接的对象存在性检查 + dry-run 化解。 |
| ✅ 已实现 | Editor support | 原始对照：Interactive editor (default)；当前说明：`add`/`edit`/`append` 在无 `-m`/`-F` 时打开编辑器（`compose_note_via_editor`：`editor::resolve_editor`→`GIT_EDITOR`/`core.editor`/`VISUAL`/`EDITOR`，仅终端回退 `vi`，否则 `LBR-REPO-003`「no editor configured」exit 128；落 `NOTES_EDITMSG`）。`edit`（与 `add -f` 在 note 已存在时）经 `notes::show` 预填现有 note；普通 `add` 在 note 已存在时于打开编辑器前即以 `AlreadyExists` 中止，新建 note 的 `add` 与 `append` 为空缓冲。保存后 `clean_note_message`（`git stripspace` 仅空白清理，**保留** `#` 行——note 可含 `#`），空结果 `LBR-CLI-002` 中止。`-m`/`-F` 仍是 headless/agent 推荐路径（不触发编辑器）。带集成测试 `add_without_message_composes_via_editor`、`edit_without_message_prefills_existing_note_in_editor`、`add_with_empty_editor_buffer_aborts`、`error_add_without_message_falls_back_to_editor_then_no_editor`。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
- `libra notes` 已公开接入顶层 CLI；新增子命令/参数的最小闭环是：CLI 变体、`src/command/mod.rs` 导出、dispatch、用户文档、兼容矩阵和测试一起更新。
